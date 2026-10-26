// The Cline adapter main loop: exactly one trial per invocation.
//
// Lifecycle: hello (before anything else) -> validate request -> started ->
// prepare isolated trial state under the artifact dir -> spawn cline ->
// map the CLI's NDJSON events to protocol events -> exactly one final ->
// exit 0. The raw credential value is resolved from the adapter process
// environment at spawn time and never appears in the request, the
// effective-config artifact or any event.

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { ADAPTER_VERSION, helloEvent } from "./capabilities.js";
import { validateRequest } from "./request.js";
import {
  buildInvocation,
  probeClineVersion,
  resolveClineExecutable,
} from "./cline.js";

const USAGE_FIELDS = [
  "inputTokens",
  "outputTokens",
  "reasoningTokens",
  "cacheReadTokens",
  "cacheWriteTokens",
  "costUsd",
];

function emit(event) {
  process.stdout.write(`${JSON.stringify(event)}\n`);
}

const now = () => Date.now();
const num = (value) =>
  typeof value === "number" && Number.isFinite(value) ? value : null;
const sha256 = (text) => createHash("sha256").update(text).digest("hex");

function readStdin() {
  return new Promise((resolve) => {
    let data = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => (data += chunk));
    process.stdin.on("end", () => resolve(data));
  });
}

/** Defensive mapping of Cline usage payloads; unknown fields stay null. */
function mapClineUsage(payload) {
  const source = payload ?? {};
  return {
    inputTokens: num(source.tokensIn ?? source.inputTokens),
    outputTokens: num(source.tokensOut ?? source.outputTokens),
    reasoningTokens: num(source.reasoningTokens),
    cacheReadTokens: num(source.cacheReads ?? source.cacheReadTokens),
    cacheWriteTokens: num(source.cacheWrites ?? source.cacheWriteTokens),
    costUsd: num(source.costUsd ?? source.cost),
  };
}

export async function main() {
  let finalEmitted = false;
  let runResult = null;
  let exitCode = null;
  let stopKind = null; // "timeout" | "cancelled" | "budget_stop"
  let iterations = 0;
  let escalationTimer = null;
  const usageTotals = Object.fromEntries(USAGE_FIELDS.map((f) => [f, null]));
  let stderrTail = "";

  emit(helloEvent());
  let request = null;
  try {
    request = JSON.parse(await readStdin());
  } catch {
    request = null;
  }
  const requestErrors = validateRequest(request);
  if (requestErrors.length > 0) {
    for (const error of requestErrors) {
      emit({
        v: 1,
        t: "warning",
        ts: now(),
        code: "invalid_request_field",
        message: error,
      });
    }
    emitFinal("protocol_error", "invalid_request");
    return;
  }

  emit({
    v: 1,
    t: "started",
    ts: now(),
    trialId: request.identity.trialId,
    workspace: {
      root: request.workspace.root,
      revision: request.workspace.revision ?? null,
      scope: request.workspace.scope,
    },
  });

  if (request.limits?.maxToolCalls) {
    emit({
      v: 1,
      t: "warning",
      ts: now(),
      code: "limit_unenforceable_maxToolCalls",
      message:
        "the Cline CLI exposes no per-tool telemetry; maxToolCalls cannot be enforced or observed",
    });
  }

  const { executable, error: resolveError } = resolveClineExecutable();
  if (resolveError) {
    emitFinal("adapter_error", resolveError);
    return;
  }
  if (!existsSync(request.workspace.root)) {
    emitFinal("adapter_error", "workspace_missing");
    return;
  }

  const base = join(request.outputs.artifactDir, "cline");
  const configDir = join(base, "config");
  const dataDir = join(base, "data");
  const mcpSettingsPath = join(base, "mcp-settings.json");
  const effectiveConfigPath = join(base, "effective-config.json");
  mkdirSync(configDir, { recursive: true });
  mkdirSync(dataDir, { recursive: true });

  const version = probeClineVersion(executable);
  if (version) {
    emit({
      v: 1,
      t: "note",
      ts: now(),
      code: "harness_version",
      message: `cline ${version}`,
      data: { surface: "cli", revision: version },
    });
  } else {
    emit({
      v: 1,
      t: "warning",
      ts: now(),
      code: "harness_version_unresolved",
      message: "could not resolve the installed cline version",
    });
  }

  const invocation = buildInvocation(request, {
    configDir,
    dataDir,
    mcpSettingsPath,
  });
  if (invocation.prompt === null) {
    emitFinal("adapter_error", "prompt_file_missing");
    return;
  }
  writeFileSync(
    mcpSettingsPath,
    `${JSON.stringify({ mcpServers: invocation.mcpServers }, null, 2)}\n`,
  );

  // The immutable effective configuration for this trial: every effective
  // guidance, skill, MCP server, model, limit and state location, with
  // credential values redacted (names only).
  const effectiveConfig = {
    protocolVersion: 1,
    adapter: { name: "cline", version: ADAPTER_VERSION },
    harness: { surface: "cli", revision: version },
    identity: request.identity,
    workspace: {
      root: request.workspace.root,
      revision: request.workspace.revision ?? null,
      scope: request.workspace.scope,
    },
    task: {
      taskVersion: request.task.taskVersion ?? null,
      taskHash: request.task.taskHash ?? null,
      promptLength: invocation.prompt.length,
      promptSha256: sha256(invocation.prompt),
    },
    condition: {
      retrievalMode: request.condition.retrievalMode,
      requiredCapabilities: request.condition.requiredCapabilities ?? [],
      mcp: Object.keys(invocation.mcpServers).length > 0
        ? Object.entries(invocation.mcpServers).map(
            ([serverName, server]) => ({
              serverName,
              command: server.command,
              args: server.args,
            }),
          )
        : null,
      guidanceOverlay: request.condition.guidanceOverlay ?? null,
    },
    profile: {
      model: request.profile.model ?? null,
      provider: request.profile.provider
        ? {
            id: request.profile.provider.id,
            credentialRef: request.profile.provider.credentialRef ?? null,
            credentialPresent: invocation.credential !== null,
          }
        : null,
      reasoning: request.profile.reasoning ?? null,
      compaction: request.profile.compaction ?? null,
    },
    limits: request.limits ?? {},
    invocation: {
      argv: invocation.argv,
      credentialFlag: invocation.credential ? "-k" : null,
      envNames: Object.keys(invocation.env),
      mcpSettingsPath,
      configDir,
      dataDir,
    },
    recordedAt: new Date().toISOString(),
  };
  writeFileSync(
    effectiveConfigPath,
    `${JSON.stringify(effectiveConfig, null, 2)}\n`,
  );
  emit({
    v: 1,
    t: "artifact",
    ts: now(),
    path: effectiveConfigPath,
    kind: "effective-config",
    description: "Immutable effective configuration for this trial",
  });

  // --- child lifecycle -------------------------------------------------
  const spawnArgv = invocation.credential
    ? [...invocation.argv, "-k", process.env[invocation.credential.name]]
    : invocation.argv;

  let child;
  try {
    child = spawn(executable, spawnArgv, {
      env: invocation.env,
      cwd: request.workspace.root,
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    child.stdin.write(invocation.prompt);
    child.stdin.end();
  } catch (error) {
    emitFinal("adapter_error", `spawn_failed: ${error.message}`);
    return;
  }

  // Declared cumulative semantics: the largest observed value is the total.
  const foldUsage = (values) => {
    for (const field of USAGE_FIELDS) {
      const value = values[field];
      if (value === null) continue;
      usageTotals[field] =
        usageTotals[field] === null ? value : Math.max(usageTotals[field], value);
    }
  };

  function emitFinal(status, reason) {
    if (finalEmitted) return;
    finalEmitted = true;
    if (escalationTimer) clearTimeout(escalationTimer);
    emit({
      v: 1,
      t: "final",
      ts: now(),
      status,
      reason,
      usage: { ...usageTotals },
      durationMs:
        runResult && typeof runResult.durationMs === "number"
          ? runResult.durationMs
          : null,
      exitCode,
    });
    process.exit(0);
  }

  const budgetExceeded = () => {
    const total =
      (usageTotals.inputTokens ?? 0) + (usageTotals.outputTokens ?? 0);
    if (
      typeof request.limits?.tokenBudget === "number" &&
      total > request.limits.tokenBudget
    )
      return "token_budget";
    if (
      typeof request.limits?.costBudgetUsd === "number" &&
      (usageTotals.costUsd ?? 0) > request.limits.costBudgetUsd
    )
      return "cost_budget";
    if (
      typeof request.limits?.maxIterations === "number" &&
      iterations > request.limits.maxIterations
    )
      return "max_iterations";
    return null;
  };

  const initiateStop = (kind, message) => {
    if (finalEmitted || stopKind) return;
    stopKind = kind;
    if (message)
      emit({ v: 1, t: "warning", ts: now(), code: kind, message });
    child.kill("SIGINT");
    escalationTimer = setTimeout(() => {
      child.kill("SIGKILL");
      // Last resort: always end with a final even if close never fires.
      setTimeout(
        () =>
          emitFinal(
            kind === "timeout" ? "timeout" : kind,
            `stop_${kind}_forced`,
          ),
        2000,
      ).unref();
    }, 5000);
  };

  const finishFromRunResult = () => {
    if (finalEmitted) return;
    if (stopKind === "timeout") return emitFinal("timeout", "wall_clock_exceeded");
    if (stopKind === "budget_stop")
      return emitFinal("budget_stop", budgetExceeded() ?? "budget_exceeded");
    if (stopKind === "cancelled") return emitFinal("cancelled", "host_requested");
    const finishReason = runResult?.finishReason;
    if (finishReason === "completed") return emitFinal("completed", null);
    if (finishReason === "aborted") return emitFinal("agent_error", "run_aborted");
    return emitFinal("agent_error", `finish_reason_${finishReason ?? "unknown"}`);
  };

  const handleAgentEvent = (event) => {
    switch (event.type) {
      case "iteration_start": {
        iterations += 1;
        emit({
          v: 1,
          t: "model_request_start",
          ts: now(),
          requestId: `iteration-${iterations}`,
        });
        break;
      }
      case "iteration_end": {
        emit({
          v: 1,
          t: "model_request_end",
          ts: now(),
          requestId: `iteration-${iterations}`,
          durationMs: num(event.iteration?.durationMs),
          ok: !event.iteration?.error,
        });
        break;
      }
      case "usage": {
        const values = mapClineUsage(event);
        emit({ v: 1, t: "usage", ts: now(), values, raw: event, source: "provider" });
        foldUsage(values);
        break;
      }
      case "done": {
        const text = typeof event.text === "string" ? event.text : "";
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "agent_done",
          message: `agent finished (${event.reason ?? "unknown"})`,
          data: {
            reason: event.reason ?? null,
            textLength: text.length,
            textSha256: text ? sha256(text) : null,
          },
        });
        break;
      }
      case "error": {
        emit({
          v: 1,
          t: "warning",
          ts: now(),
          code: "agent_error_event",
          message: String(event.message ?? "agent error event"),
        });
        break;
      }
      case "notice": {
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "cline_notice",
          message: String(event.message ?? ""),
        });
        break;
      }
      default: {
        // Per-token content events are intentionally not amplified into
        // protocol events; other unknown agent events are recorded once.
        if (
          event.type !== "content_start" &&
          event.type !== "content_update" &&
          event.type !== "content_end"
        ) {
          emit({
            v: 1,
            t: "note",
            ts: now(),
            code: "cline_agent_event",
            data: { type: event.type },
          });
        }
        break;
      }
    }
    const over = budgetExceeded();
    if (over) initiateStop("budget_stop", `budget exceeded: ${over}`);
  };

  const handleClineLine = (line) => {
    if (line.trim() === "") return;
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      emit({
        v: 1,
        t: "note",
        ts: now(),
        code: "cline_unparsed_line",
        data: { prefix: line.slice(0, 200) },
      });
      return;
    }
    switch (parsed.type) {
      case "agent_event":
        handleAgentEvent(parsed.event ?? {});
        break;
      case "run_result": {
        runResult = parsed;
        foldUsage(mapClineUsage(parsed.usage));
        const text = typeof parsed.text === "string" ? parsed.text : "";
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "run_result",
          message: `run finished (${parsed.finishReason ?? "unknown"})`,
          data: {
            finishReason: parsed.finishReason ?? null,
            iterations: parsed.iterations ?? null,
            model: parsed.model ?? null,
            durationMs: num(parsed.durationMs),
            textLength: text.length,
            textSha256: text ? sha256(text) : null,
          },
        });
        break;
      }
      case "run_aborted":
        emit({
          v: 1,
          t: stopKind === "cancelled" ? "note" : "warning",
          ts: now(),
          code: "run_aborted",
          message: `run aborted (${parsed.reason ?? "unknown"})`,
          data: { reason: parsed.reason ?? null },
        });
        break;
      case "run_start":
      case "hook_event":
      case "team_event":
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: parsed.type,
          data: parsed.event?.type ? { type: parsed.event.type } : undefined,
        });
        break;
      case "error":
        emit({
          v: 1,
          t: "warning",
          ts: now(),
          code: "cline_error",
          message: String(parsed.message ?? "error"),
        });
        break;
      default:
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "unknown_event",
          data: { type: parsed.type },
        });
    }
    if (!finalEmitted && runResult) finishFromRunResult();
  };

  let stdoutBuffer = "";
  child.stdout.on("data", (chunk) => {
    stdoutBuffer += chunk.toString("utf8");
    let index;
    while ((index = stdoutBuffer.indexOf("\n")) !== -1) {
      const line = stdoutBuffer.slice(0, index);
      stdoutBuffer = stdoutBuffer.slice(index + 1);
      handleClineLine(line);
    }
  });
  child.stderr.on("data", (chunk) => {
    const text = chunk.toString("utf8");
    stderrTail = (stderrTail + text).slice(-4096);
    for (const line of text.split("\n")) {
      if (line.trim() === "") continue;
      try {
        const parsed = JSON.parse(line);
        if (parsed.type === "error") {
          emit({
            v: 1,
            t: "warning",
            ts: now(),
            code: "cline_stderr_error",
            message: String(parsed.message ?? "error"),
          });
        }
      } catch {
        // Not JSON; retained in stderrTail for final diagnostics only.
      }
    }
  });
  child.on("error", (error) => {
    emitFinal("adapter_error", `spawn_failed: ${error.message}`);
  });
  child.on("close", (code) => {
    exitCode = code;
    if (finalEmitted) return;
    if (runResult) return finishFromRunResult();
    if (stopKind === "timeout") return emitFinal("timeout", "wall_clock_exceeded");
    if (stopKind === "budget_stop")
      return emitFinal("budget_stop", budgetExceeded() ?? "budget_exceeded");
    if (stopKind === "cancelled") return emitFinal("cancelled", "host_requested");
    if (stderrTail.trim() !== "") {
      emit({
        v: 1,
        t: "warning",
        ts: now(),
        code: "cline_stderr_tail",
        message: stderrTail.trim().slice(-2000),
      });
    }
    emitFinal("adapter_error", `no_run_result (exitCode=${code ?? null})`);
  });

  if (typeof request.limits?.wallClockMs === "number" && request.limits.wallClockMs > 0) {
    setTimeout(
      () => initiateStop("timeout", "wall clock exceeded"),
      request.limits.wallClockMs,
    ).unref();
  }
  process.on("SIGINT", () => initiateStop("cancelled", "cancellation requested"));
  process.on("SIGTERM", () => initiateStop("cancelled", "cancellation requested"));
}

