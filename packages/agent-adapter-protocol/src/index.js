// runAdapter: the coordinator host for the version 1 agent adapter protocol.
// It writes the JSON request to the adapter's stdin, performs the
// hello/capability handshake, enforces bounded NDJSON streaming, measures
// external elapsed time, accounts usage without double counting, and cancels
// only owned process trees (SIGINT, grace, SIGKILL).

import {
  appendFileSync,
  mkdirSync,
  readFileSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import {
  DEFAULT_CANCEL_GRACE_MS,
  DEFAULT_HELLO_TIMEOUT_MS,
  DEFAULT_MAX_EVENT_BYTES,
  DEFAULT_MAX_EVENTS,
  EVENT_TYPES,
  PROTOCOL_VERSION,
} from "./protocol.js";
import { readNdjsonLines } from "./framing.js";
import {
  validateEventEnvelope,
  validateFinalEvent,
  validateHello,
  validateRequest,
} from "./validate.js";
import { UsageLedger, emptyUsage } from "./usage.js";
import { killTree, spawnAdapterProcess } from "./process.js";

function baseEnv() {
  const env = {};
  for (const name of ["PATH", "HOME", "USERPROFILE", "SystemRoot", "TMPDIR"]) {
    if (process.env[name]) env[name] = process.env[name];
  }
  return env;
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Launch an adapter, drive one trial and resolve to a run result.
 *
 * `options`:
 * - executable: adapter executable (explicit path or bare command name).
 * - args: additional argument array (never a shell template).
 * - env: extra environment variables merged over a minimal base.
 * - signal: AbortSignal; aborting cancels the run (status "cancelled").
 * - helloTimeoutMs / cancelGraceMs: lifecycle overrides.
 * - onEvent: callback receiving each accepted event as it is read.
 *
 * Result: `{ status, reason, errors, warnings, unknownEventTypes, events,
 * hello, adapter, harness, usage, elapsedMs, adapterDurationMs, exitCode,
 * exitSignal }`. `elapsedMs` is coordinator-measured wall time;
 * `adapterDurationMs` is the harness-reported duration and a separate value. */
export async function runAdapter(request, options = {}) {
  const startedHr = process.hrtime.bigint();
  const errors = [];
  const warnings = [];
  const events = [];
  const unknownEventTypes = new Set();
  let hello = null;
  let ledger = null;
  let finalEvent = null;
  let status = null;
  let reason = null;
  let exitCode = null;
  let exitSignal = null;
  let spawnError = null;
  let done = false;
  let cancelling = false;

  const helloTimeoutMs = options.helloTimeoutMs ?? DEFAULT_HELLO_TIMEOUT_MS;
  const cancelGraceMs = options.cancelGraceMs ?? DEFAULT_CANCEL_GRACE_MS;

  const requestErrors = validateRequest(request);
  if (requestErrors.length > 0) {
    errors.push(...requestErrors);
    return assemble({
      status: "protocol_error",
      reason: "invalid_request",
      errors,
      warnings,
      unknownEventTypes,
      events,
      hello: null,
      ledger: null,
      finalEvent: null,
      exitCode: null,
      exitSignal: null,
      startedHr,
      resultFile: null,
    });
  }

  const limits = request.limits ?? {};
  const maxEventBytes = limits.maxEventBytes ?? DEFAULT_MAX_EVENT_BYTES;
  const maxEvents = limits.maxEvents ?? DEFAULT_MAX_EVENTS;
  const wallClockMs = limits.wallClockMs ?? null;
  const eventsFile = request.outputs.eventsFile;

  try {
    mkdirSync(request.outputs.artifactDir, { recursive: true });
    // A reused artifact dir may hold a child-pid.txt from an earlier trial;
    // drop it before this run so the end-of-run sweep can only see this
    // run's recorded pid (never a stale, possibly recycled one).
    try {
      unlinkSync(join(request.outputs.artifactDir, "child-pid.txt"));
    } catch {
      // Nothing stale (or unreadable): nothing to clear.
    }
  } catch (error) {
    return assemble({
      status: "adapter_error",
      reason: "artifact_dir_unavailable",
      errors: [`cannot create artifact dir: ${error.message}`],
      warnings,
      unknownEventTypes,
      events,
      hello: null,
      ledger: null,
      finalEvent: null,
      exitCode: null,
      exitSignal: null,
      startedHr,
      resultFile: null,
    });
  }

  let child;
  try {
    child = spawnAdapterProcess({
      executable: options.executable,
      args: options.args ?? [],
      env: { ...baseEnv(), ...(options.env ?? {}) },
      cwd: request.outputs.artifactDir,
    });
  } catch (error) {
    return assemble({
      status: "adapter_error",
      reason: `spawn_failed: ${error.message}`,
      errors: [error.message],
      warnings,
      unknownEventTypes,
      events,
      hello: null,
      ledger: null,
      finalEvent: null,
      exitCode: null,
      exitSignal: null,
      startedHr,
      resultFile: null,
    });
  }

  child.on("error", (error) => {
    spawnError = spawnError ?? error;
  });

  // Async stdin write failures (EPIPE / destroyed stream after a failed
  // spawn) surface as "error" events, not synchronous throws.
  child.stdin.on("error", (error) => {
    spawnError = spawnError ?? error;
  });
  // Drain stderr so a chatty adapter cannot block on a full pipe buffer;
  // diagnostics are not part of the protocol.
  child.stderr.on("data", () => {});

  // The versioned JSON request travels on stdin as the first line. stdin
  // stays open: on cancellation the host sends a `cancel` control line
  // (the cooperative channel — the only graceful one on Windows, where
  // signals cannot reach console-less Node processes).
  try {
    child.stdin.write(`${JSON.stringify(request)}\n`);
  } catch (error) {
    spawnError = spawnError ?? error;
  }

  const exited = new Promise((resolve) => {
    child.once("close", (code, signal) => {
      exitCode = code;
      exitSignal = signal;
      resolve();
    });
  });

  const writeEventFile = (event) => {
    if (!eventsFile) return;
    try {
      appendFileSync(eventsFile, `${JSON.stringify(event)}\n`);
    } catch (error) {
      warnings.push({
        code: "events_file_failed",
        message: error.message,
      });
    }
  };

  let escalationTimer = null;
  async function initiateCancel(kind) {
    if (cancelling || done) return;
    cancelling = true;
    status = kind === "timeout" ? "timeout" : "cancelled";
    reason = kind === "timeout" ? "wall_clock_exceeded" : "host_requested";
    // Cooperative channel on every platform: the request stdin stays open,
    // so the host sends a `cancel` control line first. Adapters SHOULD
    // honor it with a cooperative final. On POSIX the owned group still
    // receives SIGINT, so adapters that never read stdin keep the
    // documented graceful path; on Windows killTree(SIGINT) is a no-op by
    // design and the control line is the only graceful path.
    try {
      child.stdin.write("cancel\n");
    } catch {
      // Stdin may already be gone (adapter exited); on POSIX the group
      // signal still applies.
    }
    await killTree(child.pid, "SIGINT");
    escalationTimer = setTimeout(() => {
      killTree(child.pid, "SIGKILL");
    }, cancelGraceMs);
  }

  // Protocol violations are never approximated: kill the owned tree now.
  async function failProtocol(code, message) {
    status = "protocol_error";
    reason = code;
    if (message) errors.push(message);
    if (escalationTimer) clearTimeout(escalationTimer);
    await killTree(child.pid, "SIGKILL");
    // A suspended generator can keep the stdout pipe open and delay the
    // child close event; destroying the stream guarantees it fires.
    child.stdout.destroy();
  }

  const wallTimer =
    wallClockMs !== null
      ? setTimeout(() => initiateCancel("timeout"), wallClockMs)
      : null;
  const onAbort = () => initiateCancel("cancelled");
  const abortSignal = options.signal;
  if (abortSignal) {
    if (abortSignal.aborted) await initiateCancel("cancelled");
    else abortSignal.addEventListener("abort", onAbort);
  }

  const lineIterator = readNdjsonLines(child.stdout, maxEventBytes)[
    Symbol.asyncIterator
  ]();
  let pendingLine = null;
  const nextLine = () => {
    if (pendingLine === null) {
      pendingLine = lineIterator.next().catch(() => {
        // The stream may be destroyed while the generator is suspended;
        // treat that as end of stream.
        return { done: true, value: undefined };
      });
    }
    return pendingLine;
  };

  let eventCount = 0;
  let sawAnyLine = false;
  let stop = false;
  const helloTimer = setTimeout(() => {
    if (!sawAnyLine && !done)
      failProtocol("no_hello", "no hello within the hello timeout");
  }, helloTimeoutMs);

  const handleLine = async ({ line, error }) => {
    if (error === "line_oversized") {
      await failProtocol(
        "event_oversized",
        `event line exceeds ${maxEventBytes} bytes`,
      );
      stop = true;
      return;
    }
    if (error === "truncated") {
      await failProtocol(
        finalEvent ? "events_after_final" : "truncated_event",
        "incomplete event line at end of stream",
      );
      stop = true;
      return;
    }
    if (line.trim() === "") return;
    // Only a real event counts as the first line: a blank line followed by a
    // stall must not disarm the hello timeout.
    sawAnyLine = true;
    clearTimeout(helloTimer);
    if (finalEvent) {
      await failProtocol(
        "events_after_final",
        "event received after the final event",
      );
      stop = true;
      return;
    }
    let event;
    try {
      event = JSON.parse(line);
    } catch {
      await failProtocol("malformed_event", "stdout line is not valid JSON");
      stop = true;
      return;
    }
    const envelopeErrors = validateEventEnvelope(event);
    if (envelopeErrors.length > 0) {
      await failProtocol("invalid_envelope", envelopeErrors.join("; "));
      stop = true;
      return;
    }
    if (!hello && event.t !== "hello") {
      await failProtocol("hello_not_first", "the first event must be hello");
      stop = true;
      return;
    }
    eventCount += 1;
    if (eventCount > maxEvents) {
      await failProtocol(
        "event_limit_exceeded",
        `adapter emitted more than ${maxEvents} events`,
      );
      stop = true;
      return;
    }

    if (event.t === "hello") {
      if (hello) {
        await failProtocol("duplicate_hello", "second hello received");
        stop = true;
        return;
      }
      if (event.protocolVersion !== PROTOCOL_VERSION) {
        await failProtocol(
          "unsupported_protocol_version",
          `adapter protocol version ${event.protocolVersion} is not supported`,
        );
        stop = true;
        return;
      }
      const helloErrors = validateHello(event);
      if (helloErrors.length > 0) {
        await failProtocol("invalid_hello", helloErrors.join("; "));
        stop = true;
        return;
      }
      hello = event;
      const required = request.condition.requiredCapabilities ?? [];
      const missing = required.filter(
        (cap) => event.capabilities?.[cap] !== true,
      );
      if (missing.length > 0) {
        await failProtocol(
          "missing_required_capability",
          `missing required capabilities: ${missing.join(", ")}`,
        );
        stop = true;
        return;
      }
      ledger = new UsageLedger(event.usage.eventSemantics);
    } else {
      if (!EVENT_TYPES.includes(event.t)) unknownEventTypes.add(event.t);
      if (event.t === "usage") ledger?.recordUsage(event);
      if (event.t === "warning" && typeof event.code === "string") {
        warnings.push({ code: event.code, message: event.message ?? "" });
      }
      if (event.t === "final") {
        const finalErrors = validateFinalEvent(event);
        if (finalErrors.length > 0) {
          await failProtocol("invalid_final", finalErrors.join("; "));
          stop = true;
          return;
        }
        finalEvent = event;
      }
    }
    events.push(event);
    writeEventFile(event);
    try {
      options.onEvent?.(event);
    } catch (error) {
      // A faulty consumer callback must not abort the protocol or skip the
      // owned-process cleanup: record it and keep driving the trial.
      warnings.push({
        code: "on_event_callback_failed",
        message: error.message,
      });
    }
  };

  try {
    while (!done && !stop) {
      const race = [
        nextLine().then((result) => ({
          kind: result.done ? "exit" : "line",
          value: result.value,
        })),
        exited.then(() => ({ kind: "exit", value: null })),
      ];
      if (finalEvent) {
        // The protocol is complete; allow a short window for the adapter to
        // exit on its own, then escalate. Reading continues in the meantime so
        // events emitted after the final are still detected and rejected.
        race.push(delay(2000).then(() => ({ kind: "grace", value: null })));
      }
      const winner = await Promise.race(race);
      if (winner.kind === "line") pendingLine = null;
      if (winner.kind === "exit") {
        // The child exited; drain lines still buffered by the stream before
        // finishing (the final event may be among them).
        for (;;) {
          const remaining = await nextLine();
          pendingLine = null;
          if (remaining.done) break;
          await handleLine(remaining.value);
          if (stop) break;
        }
        break;
      }
      if (winner.kind === "grace") {
        // Final received but the adapter did not exit within the grace window.
        if (escalationTimer) clearTimeout(escalationTimer);
        await killTree(child.pid, "SIGKILL");
        child.stdout.destroy();
        stop = true;
        break;
      }
      await handleLine(winner.value);
    }
  } finally {
    done = true;
    clearTimeout(helloTimer);
    if (wallTimer) clearTimeout(wallTimer);
    if (escalationTimer) clearTimeout(escalationTimer);
    abortSignal?.removeEventListener("abort", onAbort);
    // Best-effort sweep of the owned process group after every path, clean or
    // not: a descendant that survives the adapter's own exit (or ignores
    // SIGINT) must not outlive the trial. killTree resolves when the group is
    // already gone.
    await killTree(child.pid, "SIGKILL");
    // Best-effort sweep of a child tree the adapter recorded in
    // `child-pid.txt` under the artifact dir (see
    // docs/agent-adapters.md): such a tree lives in its own process group,
    // which this host's own sweep cannot reach. Protocol-violation kills
    // SIGKILL the adapter before any of its own cleanup can run, so this
    // backstop is what keeps those children from outliving the trial.
    // Resolves when the recorded pid is already gone.
    try {
      const recorded = Number(
        readFileSync(
          join(request.outputs.artifactDir, "child-pid.txt"),
          "utf8",
        ).trim(),
      );
      // Only sweep sane pids: never pid 1 (which would signal the whole
      // session, including this host), and never the host or its parent.
      if (
        Number.isInteger(recorded) &&
        recorded > 1 &&
        recorded !== process.pid &&
        recorded !== process.ppid
      ) {
        await killTree(recorded, "SIGKILL");
      }
    } catch {
      // No recorded child (or unreadable): nothing to sweep.
    }
    try {
      child.stdin.end(); // Close the request/control channel.
    } catch {
      // Already closed.
    }
    await exited;
  }

  if (finalEvent) ledger?.recordFinal(finalEvent.usage);
  // A final received during an in-flight host cancellation still takes
  // precedence over the host status, except that a cooperative `cancelled`
  // final never replaces a host-initiated timeout. A protocol violation
  // (e.g. events after the final) is never downgraded.
  if (
    finalEvent &&
    status !== "protocol_error" &&
    !(status === "timeout" && finalEvent.status === "cancelled")
  ) {
    status = finalEvent.status;
    reason = finalEvent.reason ?? null;
  } else if (status === null) {
    status = "adapter_error";
    reason = spawnError
      ? `spawn_failed: ${spawnError.message}`
      : exitCode === 0
        ? "missing_final_event"
        : "nonzero_exit";
  }

  return assemble({
    status,
    reason,
    errors,
    warnings,
    unknownEventTypes,
    events,
    hello,
    ledger,
    finalEvent,
    exitCode,
    exitSignal,
    startedHr,
    resultFile: request.outputs.resultFile,
  });
}

function assemble(state) {
  const {
    status,
    reason,
    errors,
    warnings,
    unknownEventTypes,
    events,
    hello,
    ledger,
    finalEvent,
    exitCode,
    exitSignal,
    startedHr,
    resultFile,
  } = state;
  const result = {
    status,
    reason,
    errors,
    warnings,
    unknownEventTypes: [...unknownEventTypes],
    events,
    hello,
    adapter: hello
      ? { name: hello.adapter.name, version: hello.adapter.version }
      : null,
    harness: hello?.harness ?? null,
    usage: ledger ? ledger.resolve() : emptyUsage(),
    elapsedMs: Number(process.hrtime.bigint() - startedHr) / 1e6,
    adapterDurationMs:
      finalEvent && typeof finalEvent.durationMs === "number"
        ? finalEvent.durationMs
        : null,
    exitCode,
    exitSignal,
  };
  if (resultFile) {
    try {
      mkdirSync(dirname(resultFile), { recursive: true });
      writeFileSync(resultFile, `${JSON.stringify(result, null, 2)}\n`);
    } catch (error) {
      result.warnings.push({
        code: "result_file_failed",
        message: error.message,
      });
    }
  }
  return result;
}
