// runAdapter: the coordinator host for the version 1 agent adapter protocol.
// It writes the JSON request to the adapter's stdin, performs the
// hello/capability handshake, enforces bounded NDJSON streaming, measures
// external elapsed time, accounts usage without double counting, and cancels
// only owned process trees (SIGINT, grace, SIGKILL).

import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
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

  const maxEventBytes = request.limits.maxEventBytes ?? DEFAULT_MAX_EVENT_BYTES;
  const maxEvents = request.limits.maxEvents ?? DEFAULT_MAX_EVENTS;
  const wallClockMs = request.limits.wallClockMs ?? null;
  const eventsFile = request.outputs.eventsFile;

  try {
    mkdirSync(request.outputs.artifactDir, { recursive: true });
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
    spawnError = error;
  });

  // The versioned JSON request travels on stdin, then stdin closes.
  try {
    child.stdin.write(`${JSON.stringify(request)}\n`);
    child.stdin.end();
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
    if (!sawAnyLine && !done) failProtocol("no_hello", "no hello within the hello timeout");
  }, helloTimeoutMs);

  const handleLine = async ({ line, error }) => {
    sawAnyLine = true;
    clearTimeout(helloTimer);
    if (error === "line_oversized") {
      await failProtocol("event_oversized", `event line exceeds ${maxEventBytes} bytes`);
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
    if (finalEvent) {
      await failProtocol("events_after_final", "event received after the final event");
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
      const missing = required.filter((cap) => event.capabilities?.[cap] !== true);
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
    options.onEvent?.(event);

    if (finalEvent) {
      // The protocol is complete; allow a short window for the adapter to
      // exit on its own, then escalate. A final received during an in-flight
      // host cancellation still takes precedence over the host status.
      const settled = await Promise.race([
        exited.then(() => "exit"),
        delay(2000).then(() => "wait"),
      ]);
      if (settled !== "exit") {
        if (escalationTimer) clearTimeout(escalationTimer);
        await killTree(child.pid, "SIGKILL");
        child.stdout.destroy();
        await exited;
      }
      stop = true;
    }
  };

  while (!done && !stop) {
    const winner = await Promise.race([
      nextLine().then((result) => ({
        kind: result.done ? "exit" : "line",
        value: result.value,
      })),
      exited.then(() => ({ kind: "exit", value: null })),
    ]);
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
    await handleLine(winner.value);
  }

  done = true;
  if (wallTimer) clearTimeout(wallTimer);
  if (escalationTimer) clearTimeout(escalationTimer);
  abortSignal?.removeEventListener("abort", onAbort);
  await exited;

  if (finalEvent) {
    status = finalEvent.status;
    reason = finalEvent.reason ?? null;
    ledger?.recordFinal(finalEvent.usage);
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
