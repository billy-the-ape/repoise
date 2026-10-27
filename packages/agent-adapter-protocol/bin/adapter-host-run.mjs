#!/usr/bin/env node
// AP-04 host bridge: the thin Node entry point that `repoise bench` spawns
// for each repoise-condition trial. The Rust coordinator runs
//
//   node <this file> <adapter-executable> [adapter-args...]
//
// writes the version-1 JSON request on stdin (first line), and later may
// send a `cancel` control line on the same stdin to request graceful
// cancellation before escalating to signals.
//
// Contract:
// - Adapter environment variables are injected by the coordinator onto this
//   process's environment; their names are listed in `ADAPTER_ENV_VARS`
//   (comma-separated) and only those names pass through to the adapter.
// - The adapter's raw stderr is mirrored to `stderr.log` in the request's
//   artifact directory (best effort).
// - The coordinator host writes the event stream (`outputs.eventsFile`) and
//   the final result (`outputs.resultFile`) itself; the bridge exits 0 once
//   the host produced a result, 1 otherwise.
//
// No repository content is ever loaded or executed by this bridge: it only
// forwards the request, the process lifecycle and the artifacts.

import { appendFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { runAdapter } from "../src/index.js";

const executable = process.argv[2];
if (!executable) {
  process.stderr.write(
    "usage: run-adapter.mjs <adapter-executable> [args...]\n",
  );
  process.exit(2);
}

// A JS adapter (e.g. a test fixture or a script without a shebang/exec bit)
// is launched through Node itself; anything else runs directly.
const launch = (() => {
  let viaNode = false;
  try {
    viaNode =
      statSync(executable).isFile() && /\.(mjs|cjs|js)$/i.test(executable);
  } catch {
    // Not stat-able: run it as a command (PATH lookup or absolute path).
  }
  return viaNode
    ? {
        executable: process.execPath,
        args: [executable, ...process.argv.slice(3)],
      }
    : { executable, args: process.argv.slice(3) };
})();

const abortController = new AbortController();

// Read the request line, then keep stdin open for the cooperative `cancel`
// channel (the only graceful path on Windows).
// Stream-level UTF-8 decoding: the request is one raw UTF-8 line, and a
// multi-byte character straddling a pipe read boundary would otherwise
// decode to U+FFFD per chunk (silently changing the prompt the adapter
// receives versus the recorded requestHash). In string mode the
// `chunk.toString("utf8")` calls below are harmless no-ops.
process.stdin.setEncoding("utf8");
let requestRest = "";
const requestLine = await new Promise((resolve, reject) => {
  let buffer = "";
  const onData = (chunk) => {
    buffer += chunk.toString("utf8");
    const index = buffer.indexOf("\n");
    if (index >= 0) {
      process.stdin.removeListener("data", onData);
      // The coordinator may write the request and the `cancel` line before
      // the first listener is attached (SIGINT during trial setup); keep the
      // remainder so that cancel is not silently dropped.
      requestRest = buffer.slice(index + 1);
      resolve(buffer.slice(0, index));
    }
  };
  process.stdin.on("data", onData);
  process.stdin.on("end", () =>
    reject(new Error("stdin closed before the request line")),
  );
  process.stdin.on("error", reject);
});
const scanCancel = (text) => {
  for (const line of text.split("\n")) {
    if (line.trim() === "cancel") abortController.abort();
  }
};
scanCancel(requestRest);
process.stdin.on("data", (chunk) => scanCancel(chunk.toString("utf8")));
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => abortController.abort());
}

let request;
try {
  request = JSON.parse(requestLine);
} catch (error) {
  process.stderr.write(`invalid request line: ${error.message}\n`);
  process.exit(1);
}

// Adapter environment: only the names the coordinator listed pass through.
const adapterEnv = {};
for (const name of (process.env.ADAPTER_ENV_VARS ?? "").split(",")) {
  const key = name.trim();
  if (key && key in process.env) adapterEnv[key] = process.env[key];
}

// Mirror the adapter's raw stderr (and this bridge's own stderr) to the
// artifact directory; best effort, never fatal.
const stderrLog = request.outputs?.artifactDir
  ? join(request.outputs.artifactDir, "stderr.log")
  : null;
const mirrorStderr = (chunk) => {
  if (!stderrLog) return;
  try {
    appendFileSync(stderrLog, chunk);
  } catch {
    // Diagnostics only; never fail the trial on a logging problem.
  }
};
if (stderrLog) process.stderr.on("data", mirrorStderr);

let result = null;
try {
  result = await runAdapter(request, {
    executable: launch.executable,
    args: launch.args,
    env: adapterEnv,
    signal: abortController.signal,
    ...(stderrLog ? { onStderr: mirrorStderr } : {}),
  });
} finally {
  if (stderrLog) process.stderr.removeListener("data", mirrorStderr);
}

process.exit(result ? 0 : 1);
