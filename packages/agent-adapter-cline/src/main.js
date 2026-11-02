// The Cline adapter main loop: exactly one trial per invocation.
//
// Lifecycle: hello (before anything else) -> validate request -> started ->
// prepare isolated trial state under the artifact dir -> spawn cline ->
// map the CLI's NDJSON events to protocol events -> exactly one final ->
// exit 0. The raw credential value is resolved from the adapter process
// environment at spawn time; it never appears in the request or the
// effective-config artifact, and is scrubbed from all forwarded Cline
// output before it can appear in an event.

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { StringDecoder } from "node:string_decoder";
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

/**
 * Split a stdin stream into lines for the adapter: the first line is the
 * JSON request, every subsequent line is a host control line (the host
 * keeps stdin open and sends `cancel` on cancellation - the cooperative
 * channel that also exists on Windows, where signals cannot be delivered
 * to console-less Node processes). Both callbacks are fed by one line
 * consumer, so a `cancel` arriving in the same chunk as the request can
 * never be discarded.
 */
export function attachStdinLines(inputStream, onRequestLine, onControlLine) {
  const stream = inputStream ?? process.stdin;
  let buffer = "";
  let firstLineDone = false;
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 1);
      if (!firstLineDone) {
        firstLineDone = true;
        onRequestLine(line);
      } else {
        onControlLine(line);
      }
    }
  });
  // Stdin closed without a newline: deliver whatever arrived ("" if
  // nothing) instead of never firing.
  stream.on("end", () => {
    if (!firstLineDone) {
      firstLineDone = true;
      onRequestLine(buffer);
    }
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
  // The agent's final message text (from the `done` event), recorded on
  // the final event as `data.answer` for benchmark grading; null when no
  // final text was produced.
  let finalAnswer = null;
  let exitCode = null;
  let stopKind = null; // "timeout" | "cancelled" | "budget_stop"
  let iterations = 0;
  let escalationTimer = null;
  let child = null;
  const usageTotals = Object.fromEntries(USAGE_FIELDS.map((f) => [f, null]));
  let stderrTail = "";

  // Signal handlers are registered before anything else so a host
  // cancellation during the stdin read, the version probe or the spawn
  // still produces a final (Node's default SIGINT action would otherwise
  // kill the adapter without one).
  process.on("SIGINT", () =>
    initiateStop("cancelled", "cancellation requested"),
  );
  process.on("SIGTERM", () =>
    initiateStop("cancelled", "cancellation requested"),
  );
  // If the host dies (crash, external kill) the stdout pipe closes: sweep
  // the cline group so it cannot outlive a trial whose host is gone.
  // Fires harmlessly on the normal exit path too (child already swept).
  process.stdout.on("close", () => {
    sweepChild();
    process.exit(0);
  });

  // One stdin reader for both the request (the first line) and the host's
  // control lines; see attachStdinLines for why one consumer matters.
  const requestReady = new Promise((resolve) => {
    attachStdinLines(process.stdin, resolve, (line) => {
      if (line.trim() === "cancel")
        initiateStop("cancelled", "host requested cancellation");
    });
  });

  // The cline child (and any MCP servers it started) live in their own
  // process group that the host cannot reach, so the adapter sweeps them
  // on every final and on every exit path.
  function sweepChild() {
    if (!child) return;
    try {
      if (process.platform === "win32") {
        child.kill("SIGKILL");
      } else {
        process.kill(-child.pid, "SIGKILL");
      }
    } catch {
      // Group already gone.
    }
  }
  process.on("exit", sweepChild);

  emit(helloEvent());
  let request = null;
  try {
    request = JSON.parse(await requestReady);
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

  const {
    executable,
    prefixArgs,
    error: resolveError,
  } = resolveClineExecutable();
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

  const version = probeClineVersion({ executable, prefixArgs });
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
  // The optional expected harness version pin (the suite's
  // profile.harness.revision): when set, the probed version must match it
  // exactly before any agent work starts (validated as a dotted version by
  // request.js, never parsed later).
  const expectedVersion =
    typeof request.profile.harness.revision === "string"
      ? request.profile.harness.revision
      : null;
  if (invocation.prompt === null) {
    emitFinal("adapter_error", "prompt_file_missing");
    return;
  }
  // A prompt-file may contain a dash-prefixed prompt even when the inline
  // one was validated: reject before spawn, because the Cline CLI would
  // parse it as an option (it does not honour a `--` guard).
  if (invocation.prompt.startsWith("-")) {
    emitFinal("adapter_error", "prompt_leading_dash");
    return;
  }
  writeFileSync(
    mcpSettingsPath,
    `${JSON.stringify({ mcpServers: invocation.mcpServers }, null, 2)}\n`,
  );

  // Provider endpoint: the profile's optional endpoint (e.g. an
  // OpenAI-compatible base URL) is materialized in the Cline per-trial
  // provider settings, so the CLI does not fall back to its built-in
  // default host. The credential never enters this file (the `-k` flag
  // supplies it).
  const provider = request.profile.provider;
  if (
    provider &&
    typeof provider.endpoint === "string" &&
    provider.endpoint.length > 0
  ) {
    const settingsDir = join(dataDir, "settings");
    mkdirSync(settingsDir, { recursive: true });
    writeFileSync(
      join(settingsDir, "providers.json"),
      `${JSON.stringify(
        {
          version: 1,
          lastUsedProvider: provider.id,
          modes: {},
          providers: {
            [provider.id]: {
              settings: {
                provider: provider.id,
                baseUrl: provider.endpoint,
                // Mirrors the verified operator setting: a 10-minute
                // per-request cap for local 27B inference. The per-trial
                // wall-clock backstop remains the authoritative bound.
                timeout: 600000,
              },
              updatedAt: new Date().toISOString(),
              tokenSource: "manual",
            },
          },
        },
        null,
        2,
      )}\n`,
    );
  }

  // The immutable effective configuration for this trial: every effective
  // guidance, skill, MCP server, model, limit and state location, with
  // credential values redacted (names only).
  const effectiveConfig = {
    protocolVersion: 1,
    adapter: { name: "cline", version: ADAPTER_VERSION },
    harness: {
      surface: "cli",
      revision: version,
      expected: expectedVersion,
      pinned: expectedVersion !== null,
      match: expectedVersion === null ? null : version === expectedVersion,
      updateControl: invocation.updateControl,
    },
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
      mcp:
        Object.keys(invocation.mcpServers).length > 0
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
            credentialRef:
              typeof request.profile.provider.credentialRef === "string" &&
              /^env:[A-Za-z_][A-Za-z0-9_]*$/.test(
                request.profile.provider.credentialRef,
              )
                ? request.profile.provider.credentialRef
                : null,
            credentialPresent: invocation.credential !== null,
            endpoint:
              typeof request.profile.provider.endpoint === "string"
                ? request.profile.provider.endpoint
                : null,
          }
        : null,
      reasoning: request.profile.reasoning ?? null,
      compaction: request.profile.compaction ?? null,
    },
    limits: request.limits ?? {},
    invocation: {
      // Redacted: the recorded argv must not carry the prompt text (it is
      // hashed under task.promptSha256); the spawn vector below still
      // carries the real prompt as the trailing element.
      argv: [...invocation.argv.slice(0, -1), "<prompt>"],
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
  // The prompt is the trailing positional argument of invocation.argv and
  // must remain the final element, so the credential flag is inserted
  // before it, never after.
  const spawnArgv = invocation.credential
    ? [
        ...invocation.argv.slice(0, -1),
        "-k",
        process.env[invocation.credential.name],
        ...invocation.argv.slice(-1),
      ]
    : invocation.argv;

  // Forwarded Cline output can echo the credential (e.g. an invalid-key
  // error). Scrub the resolved value from everything forwarded into events.
  const credentialValue =
    invocation.credential &&
    typeof process.env[invocation.credential.name] === "string" &&
    process.env[invocation.credential.name].length > 0
      ? process.env[invocation.credential.name]
      : null;
  const redact = (text) =>
    typeof text === "string" && credentialValue !== null
      ? text.split(credentialValue).join("[REDACTED]")
      : text;
  const redactValue = (value) => {
    if (typeof value === "string") return redact(value);
    if (value !== null && typeof value === "object") {
      try {
        return JSON.parse(redact(JSON.stringify(value)));
      } catch {
        return "[REDACTED]";
      }
    }
    return value;
  };

  // A stop that arrived before the trial started (during the stdin read,
  // the version probe or the file writes) must not spawn the CLI at all;
  // only "cancelled" can be set this early.
  if (stopKind !== null) {
    emitFinal("cancelled", "host_requested");
    return;
  }

  // Version pin enforcement (fail-closed, before spawn — no model request):
  // a drifted executable (e.g. a background auto-update between preparation
  // and execution) must never be pooled into completed results; the failed
  // record is preserved with a stable, actionable reason and the effective-
  // config artifact already carries both versions.
  if (expectedVersion !== null) {
    if (version === null) {
      emitFinal(
        "adapter_error",
        `harness_version_unresolved (expected ${expectedVersion}): the version probe failed, so the pin cannot be verified — restore the pinned executable and re-run`,
      );
      return;
    }
    if (version !== expectedVersion) {
      emitFinal(
        "adapter_error",
        `harness_version_mismatch (expected ${expectedVersion}, CLI reports ${version}): the CLI may have been auto-updated or replaced — restore the pinned version or re-pin (suite profile.harness.revision), then re-run`,
      );
      return;
    }
  }

  try {
    child = spawn(executable, [...prefixArgs, ...spawnArgv], {
      env: invocation.env,
      cwd: request.workspace.root,
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
      // POSIX: give the cline child its own process group so the host's
      // group-level SIGINT reaches only this adapter; the adapter's
      // initiateStop alone controls the child's signalling.
      ...(process.platform === "win32" ? {} : { detached: true }),
    });
    // Record the child tree's root pid where the host looks
    // (docs/agent-adapters.md): the host's own group sweep cannot reach a
    // detached child group, and a protocol-violation kill SIGKILLs this
    // adapter before its own sweeps can run, so the host backstop sweeps
    // this pid. Best-effort: a write failure only weakens the backstop.
    if (child.pid) {
      try {
        writeFileSync(
          join(request.outputs.artifactDir, "child-pid.txt"),
          `${child.pid}\n`,
        );
      } catch {
        // Best-effort backstop only.
      }
    }
    // The prompt travels on argv, not stdin: close stdin without data so
    // the child sees EOF immediately instead of waiting for input.
    child.stdin.on("error", (error) => {
      emitFinal("adapter_error", `stdin_closed_failed: ${error.message}`);
    });
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
        usageTotals[field] === null
          ? value
          : Math.max(usageTotals[field], value);
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
      data: { answer: finalAnswer },
      usage: { ...usageTotals },
      durationMs:
        runResult && typeof runResult.durationMs === "number"
          ? runResult.durationMs
          : null,
      exitCode,
    });
    sweepChild(); // Never orphan the child group on any final path.
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

  function initiateStop(kind, message) {
    if (finalEmitted || stopKind) return;
    stopKind = kind;
    if (message) emit({ v: 1, t: "warning", ts: now(), code: kind, message });
    if (!child) return;
    // The cline child runs in its own process group on POSIX, so signal its
    // whole tree explicitly (this also reaches any MCP servers it spawned).
    const signalChild = (signal) => {
      if (process.platform === "win32") {
        child.kill(signal);
        return;
      }
      try {
        process.kill(-child.pid, signal);
      } catch {
        child.kill(signal); // Group already gone; fall back to the child.
      }
    };
    signalChild("SIGINT");
    // Shorter than the host's default cancelGraceMs (5000 ms), so if the
    // child ignores SIGINT this adapter kills its own group before the
    // host force-kills us and orphans it.
    escalationTimer = setTimeout(() => {
      signalChild("SIGKILL");
      // Last resort: always end with a final even if close never fires.
      setTimeout(
        () =>
          emitFinal(
            kind === "timeout" ? "timeout" : kind,
            `stop_${kind}_forced`,
          ),
        2000,
      ).unref();
    }, 3000);
  }

  const finishFromRunResult = () => {
    if (finalEmitted) return;
    if (stopKind === "timeout")
      return emitFinal("timeout", "wall_clock_exceeded");
    if (stopKind === "budget_stop")
      return emitFinal("budget_stop", budgetExceeded() ?? "budget_exceeded");
    if (stopKind === "cancelled")
      return emitFinal("cancelled", "host_requested");
    const finishReason = runResult?.finishReason;
    if (finishReason === "completed") return emitFinal("completed", null);
    if (finishReason === "aborted")
      return emitFinal("agent_error", "run_aborted");
    return emitFinal(
      "agent_error",
      `finish_reason_${redact(String(finishReason ?? "unknown"))}`,
    );
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
        emit({
          v: 1,
          t: "usage",
          ts: now(),
          values,
          raw: redactValue(event),
          source: "provider",
        });
        foldUsage(values);
        break;
      }
      case "done": {
        const text = typeof event.text === "string" ? event.text : "";
        if (text) finalAnswer = text;
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "agent_done",
          message: `agent finished (${redact(String(event.reason ?? "unknown"))})`,
          data: {
            reason: redactValue(event.reason ?? null),
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
          message: redact(String(event.message ?? "agent error event")),
        });
        break;
      }
      case "notice": {
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "cline_notice",
          message: redact(String(event.message ?? "")),
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
            data: { type: redactValue(event.type) },
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
        // Redact the full line before truncating: a credential that
        // straddles the 200-char boundary must not survive as a fragment.
        data: { prefix: redact(line).slice(0, 200) },
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
          message: `run finished (${redact(String(parsed.finishReason ?? "unknown"))})`,
          data: {
            finishReason: redactValue(parsed.finishReason ?? null),
            iterations: redactValue(parsed.iterations ?? null),
            model: redactValue(parsed.model ?? null),
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
          message: `run aborted (${redact(String(parsed.reason ?? "unknown"))})`,
          data: { reason: redactValue(parsed.reason ?? null) },
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
          data: parsed.event?.type
            ? { type: redactValue(parsed.event.type) }
            : undefined,
        });
        break;
      case "error":
        emit({
          v: 1,
          t: "warning",
          ts: now(),
          code: "cline_error",
          message: redact(String(parsed.message ?? "error")),
        });
        break;
      default:
        emit({
          v: 1,
          t: "note",
          ts: now(),
          code: "unknown_event",
          data: { type: redactValue(parsed.type) },
        });
    }
    if (!finalEmitted && runResult) finishFromRunResult();
  };

  // StringDecoder instances keep multibyte UTF-8 characters that span
  // chunk boundaries intact.
  const stdoutDecoder = new StringDecoder("utf8");
  const stderrDecoder = new StringDecoder("utf8");
  let stdoutBuffer = "";
  child.stdout.on("data", (chunk) => {
    stdoutBuffer += stdoutDecoder.write(chunk);
    let index;
    while ((index = stdoutBuffer.indexOf("\n")) !== -1) {
      const line = stdoutBuffer.slice(0, index);
      stdoutBuffer = stdoutBuffer.slice(index + 1);
      handleClineLine(line);
    }
  });
  child.stderr.on("data", (chunk) => {
    const text = stderrDecoder.write(chunk);
    // Redact the accumulated tail before trimming so a credential that
    // straddles the 4096-char boundary cannot survive in the tail.
    stderrTail = redact(stderrTail + text).slice(-4096);
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
            message: redact(String(parsed.message ?? "error")),
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
    if (stopKind === "timeout")
      return emitFinal("timeout", "wall_clock_exceeded");
    if (stopKind === "budget_stop")
      return emitFinal("budget_stop", budgetExceeded() ?? "budget_exceeded");
    if (stopKind === "cancelled")
      return emitFinal("cancelled", "host_requested");
    if (stderrTail.trim() !== "") {
      emit({
        v: 1,
        t: "warning",
        ts: now(),
        code: "cline_stderr_tail",
        message: redact(stderrTail.trim()).slice(-2000),
      });
    }
    emitFinal("adapter_error", `no_run_result (exitCode=${code ?? null})`);
  });

  if (
    typeof request.limits?.wallClockMs === "number" &&
    request.limits.wallClockMs > 0
  ) {
    setTimeout(
      () => initiateStop("timeout", "wall clock exceeded"),
      request.limits.wallClockMs,
    ).unref();
  }
}
