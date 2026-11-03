import { spawn } from "node:child_process";
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseCapabilities } from "../core/capabilities";
import { buildWindowsShimInvocation, runCli } from "../core/run";
import { FAKE, runFake } from "./helpers";

test("capabilities --json returns a parseable machine document", async () => {
  const lines: string[] = [];
  const handle = runCli({
    binary: process.execPath,
    args: [FAKE, "capabilities", "--json"],
    onStdoutLine: (line) => lines.push(line),
  });
  const result = await handle.promise;
  assert.equal(result.exitCode, 0);
  const parsed = parseCapabilities(lines.join("\n"));
  assert.equal(parsed.ok, true);
  if (parsed.ok) {
    assert.equal(parsed.doc.name, "repoise");
    assert.equal(parsed.doc.compatibility.supportedEventStreamVersions[0], 1);
  }
});

test("index --events stream: full happy path with progress and warning", async () => {
  const run = runFake(["index", "--events", "/fake/repo"]);
  const result = await run.handle.promise;
  assert.equal(result.exitCode, 0);
  assert.equal(result.spawnError, null);
  const summary = run.stream.summary();
  assert.equal(summary.started, true);
  assert.equal(summary.terminal, "completed");
  assert.equal(summary.incomplete, false);
  // started, progress, progress, warning, progress, completed
  assert.equal(run.events.length, 6);
  assert.equal(run.events[0].type, "started");
  assert.deepEqual(summary.warnings, [
    "embedding provider unavailable; lexical only",
  ]);
  const lastProgress = summary.lastProgress;
  assert.equal(lastProgress?.payload.phase, "publication");
  assert.equal(lastProgress?.payload.done, 42);
  assert.equal(lastProgress?.payload.etaSeconds, 1);
});

test("CRLF newlines do not corrupt event lines", async () => {
  const run = runFake(["index", "--events", "/fake/repo"], {
    FAKE_REPOISE_CRLF: "1",
  });
  const result = await run.handle.promise;
  assert.equal(result.exitCode, 0);
  assert.equal(run.stream.summary().terminal, "completed");
  assert.equal(run.events.length, 6);
});

test("a malformed line does not abort the stream", async () => {
  const run = runFake([
    "index",
    "--events",
    "/fake/repo",
    "--fixture",
    "malformed",
  ]);
  const result = await run.handle.promise;
  assert.equal(result.exitCode, 0);
  const types = run.events.map((e) => e.type);
  assert.deepEqual(types, ["started", "completed"]);
});

test("unknown event types are skipped without breaking the run", async () => {
  const run = runFake([
    "index",
    "--events",
    "/fake/repo",
    "--fixture",
    "unknown-type",
  ]);
  const result = await run.handle.promise;
  assert.equal(result.exitCode, 0);
  const types = run.events.map((e) => e.type);
  assert.deepEqual(types, ["started", "progress", "completed"]);
});

test("unsupported schema versions are rejected per line", async () => {
  const run = runFake([
    "index",
    "--events",
    "/fake/repo",
    "--fixture",
    "bad-version",
  ]);
  const result = await run.handle.promise;
  assert.equal(result.exitCode, 0);
  assert.equal(run.events.length, 0);
  assert.equal(run.stream.summary().incomplete, true);
});

test("cooperative cancellation: SIGTERM yields a cancelled terminal event", async () => {
  const run = runFake(["watch", "--events", "/fake/repo", "--fixture", "long"]);
  await run.firstStarted;
  run.handle.cancel();
  const result = await run.handle.promise;
  assert.equal(result.cancelledByUs, true);
  if (process.platform === "win32") {
    // Windows process.kill() maps to TerminateProcess (abrupt) per
    // docs/automation.md: no terminal event is guaranteed.
    assert.ok(result.exitCode !== 0 || run.stream.summary().incomplete);
  } else {
    assert.equal(result.exitCode, 0);
    assert.equal(run.stream.summary().terminal, "cancelled");
    assert.equal(run.stream.summary().terminalEvent?.payload.reason, "signal");
  }
});

test("timeout cancels and marks a stuck run", async () => {
  const handle = runCli({
    binary: process.execPath,
    args: [FAKE, "index", "--events", "/fake/repo", "--fixture", "sleep"],
    timeoutMs: 400,
  });
  const result = await handle.promise;
  assert.equal(result.cancelledByUs, true);
  assert.equal(result.timedOut, true);
});

test("spawn failure surfaces as a spawnError without throwing", async () => {
  const handle = runCli({
    binary: process.execPath,
    args: [FAKE, "no-such-subcommand"],
  });
  const result = await handle.promise;
  // The fake exits 2 for unknown subcommands (a clean non-zero exit, not
  // a spawn failure); use a genuinely missing binary for the ENOENT path.
  assert.equal(result.exitCode, 2);
  const missing = runCli({ binary: "/definitely/not/a/binary", args: [] });
  const missingResult = await missing.promise;
  assert.equal(missingResult.exitCode, null);
  assert.notEqual(missingResult.spawnError, null);
});

test("Windows .cmd shims route through cmd.exe with escaped arguments", () => {
  const invocation = buildWindowsShimInvocation("/tools/repoise.cmd", [
    "index",
    "/a b/c root",
    'quote"arg',
  ]);
  assert.equal(invocation.binary, "cmd.exe");
  assert.deepEqual(invocation.args.slice(0, 4), [
    "/d",
    "/s",
    "/c",
    "/tools/repoise.cmd",
  ]);
  assert.equal(invocation.args[4], "index");
  assert.equal(invocation.args[5], '"/a b/c root"');
  assert.equal(invocation.args[6], '"quote\\"arg"');
});

test("runCli rewrites .cmd binaries to the cmd.exe invocation on win32", () => {
  let captured: { binary: string; args: string[] } | null = null;
  const capture = (): { binary: string; args: string[] } | null => captured;
  const dummyChild = {
    pid: 4242,
    exitCode: null,
    signalCode: null,
    stdout: null,
    stderr: null,
    on: () => undefined,
    kill: () => true,
  } as unknown as import("node:child_process").ChildProcess;
  const handle = runCli({
    binary: "/tools/repoise.cmd",
    args: ["index", "/a b/c root"],
    platform: "win32",
    // A structurally valid dummy child; the promise is never awaited here.
    spawnImpl: ((binary: string, args: string[]) => {
      captured = { binary, args };
      return dummyChild;
    }) as unknown as typeof spawn,
  });
  void handle;
  const call = capture();
  if (call === null) {
    throw new Error("spawnImpl was never invoked");
  }
  assert.equal(call.binary, "cmd.exe");
  assert.deepEqual(call.args, [
    "/d",
    "/s",
    "/c",
    "/tools/repoise.cmd",
    "index",
    '"/a b/c root"',
  ]);
});

test("synchronous spawn failure settles with a spawnError", async () => {
  const handle = runCli({
    binary: "/tools/repoise.cmd",
    args: ["index"],
    platform: "win32",
    spawnImpl: () => {
      throw new Error("spawn EINVAL");
    },
  });
  const result = await handle.promise;
  assert.equal(result.exitCode, null);
  assert.equal(result.spawnError, "spawn EINVAL");
});
