import assert from "node:assert/strict";
import { test } from "node:test";
import type { CliRunHandle, CliRunResult } from "../core/run";
import type { StatusOutcome } from "../core/status";
import type { CliSession } from "../core/cliSession";
import type { CliManager } from "../ui/cliManager";
import { WatchManager } from "../ui/watchManager";
import { wait } from "./helpers";

/** `status --json` probe result with a free scope (probe passes). */
function freeStatus(): StatusOutcome {
  return {
    ok: true,
    status: {
      requestedRoot: "/root",
      scope: {
        adapter: "git",
        root: "/root",
        snapshotMode: "git-working-tree",
        repoId: "repo-1",
        worktreeId: "wt-1",
        remoteIdentity: null,
      },
      snapshot: null,
      index: null,
      freshness: null,
      writer: { lockHeld: false, state: "free", owner: null },
      lastError: null,
    },
  };
}

function fakeCli(): CliManager {
  const session: CliSession = {
    binary: process.execPath,
    source: "path",
    version: "0.1.0",
    compatible: true,
    problems: [],
    searched: [],
    capabilities: {
      schemaVersion: 1,
      name: "repoise",
      version: "0.1.0",
      eventStream: {
        schema: "schemas/event-stream.v1.schema.json",
        version: 1,
        operations: ["index", "watch"],
        heartbeatIntervalMs: 15000,
      },
      features: ["event-stream", "mcp-stdio", "exclusive-scope-lock"],
      commands: [],
      benchmark: { supported: false, command: "bench" },
      exitCodes: {},
      compatibility: { policy: "additive", supportedEventStreamVersions: [1] },
    },
  };
  return {
    resolve: () => Promise.resolve(session),
    current: () => session,
    refresh: () => Promise.resolve(session),
    setWorktreeId: () => undefined,
    worktreeIdFor: () => null,
    invalidate: () => undefined,
    dispose: () => undefined,
  } as unknown as CliManager;
}

function fakeHandle(): CliRunHandle {
  let resolveResult: (result: CliRunResult) => void = () => undefined;
  const promise = new Promise<CliRunResult>((resolve) => {
    resolveResult = resolve;
  });
  return {
    promise,
    cancel: () =>
      resolveResult({
        exitCode: 0,
        signal: null,
        timedOut: false,
        cancelledByUs: true,
        spawnError: null,
        stderr: "",
      }),
    pid: 9999,
  };
}

test("two concurrent starts spawn a single watch process", async () => {
  let spawns = 0;
  const manager = new WatchManager(
    fakeCli(),
    { appendLine: () => undefined },
    { onEvent: () => undefined, onStateChange: () => undefined },
    {
      probeStatus: () => Promise.resolve(freeStatus()),
      runProcess: () => {
        spawns += 1;
        return fakeHandle();
      },
    },
  );
  void manager.start("/root");
  void manager.start("/root");
  await wait(50);
  assert.equal(spawns, 1);
  assert.equal(manager.isRunning("/root"), true);
  manager.dispose();
});

test("a stop before an in-flight start settles prevents the spawn", async () => {
  let spawns = 0;
  let resolveProbe: (result: StatusOutcome) => void = () => undefined;
  const probe = new Promise<StatusOutcome>((resolve) => {
    resolveProbe = resolve;
  });
  const manager = new WatchManager(
    fakeCli(),
    { appendLine: () => undefined },
    { onEvent: () => undefined, onStateChange: () => undefined },
    {
      probeStatus: () => probe,
      runProcess: () => {
        spawns += 1;
        return fakeHandle();
      },
    },
  );
  void manager.start("/root");
  // The probe is in flight; stop() invalidates the epoch for this root.
  manager.stop("/root");
  resolveProbe(freeStatus());
  await wait(50);
  assert.equal(spawns, 0);
  assert.equal(manager.info("/root").status, "stopped");
  manager.dispose();
});
