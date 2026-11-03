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

test("stop-during-probe followed by two starts still spawns a single watch", async () => {
  let spawns = 0;
  // Each start gets its own probe (like the real status processes): the
  // first start's probe settles while the second start's probe is still
  // in flight. Without a per-start claim token, the first start's
  // `finally` would delete the second start's claim, letting the third
  // start pass the guard and overwrite the second start's handle.
  let resolveFirst: (result: StatusOutcome) => void = () => undefined;
  const firstProbe = new Promise<StatusOutcome>((resolve) => {
    resolveFirst = resolve;
  });
  let resolveSlow: (result: StatusOutcome) => void = () => undefined;
  const slowProbe = new Promise<StatusOutcome>((resolve) => {
    resolveSlow = resolve;
  });
  let probeCount = 0;
  const manager = new WatchManager(
    fakeCli(),
    { appendLine: () => undefined },
    { onEvent: () => undefined, onStateChange: () => undefined },
    {
      probeStatus: () => {
        probeCount += 1;
        if (probeCount === 1) {
          return firstProbe;
        }
        if (probeCount === 2) {
          return slowProbe;
        }
        return Promise.resolve(freeStatus());
      },
      runProcess: () => {
        spawns += 1;
        return fakeHandle();
      },
    },
  );
  void manager.start("/root");
  manager.stop("/root");
  void manager.start("/root");
  // The first start's probe now settles: stale epoch, and its claim
  // release must not touch the second start's claim.
  resolveFirst(freeStatus());
  await wait(5);
  // A late third start (e.g. the webview ready message).
  void manager.start("/root");
  resolveSlow(freeStatus());
  await wait(50);
  assert.equal(spawns, 1);
  assert.equal(manager.isRunning("/root"), true);
  manager.dispose();
});

test("a start issued while a stop is draining is reported, not silently dropped", async () => {
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
  await wait(50);
  assert.equal(spawns, 1);
  // Stop: the fake handle's promise settles on a microtask, so a start
  // issued synchronously right after stop() lands inside the drain window.
  manager.stop("/root");
  void manager.start("/root");
  assert.equal(spawns, 1);
  assert.equal(
    manager.info("/root").lastOutcome,
    "a stop for this root is still finishing; press Start again",
  );
  // After the drain settles, Start works again.
  await wait(5);
  void manager.start("/root");
  await wait(50);
  assert.equal(spawns, 2);
  assert.equal(manager.isRunning("/root"), true);
  manager.dispose();
});

test("a user Stop during drain suppresses the restart of a crashed watch", async () => {
  let spawns = 0;
  let resolveCrash: (result: CliRunResult) => void = () => undefined;
  const manager = new WatchManager(
    fakeCli(),
    { appendLine: () => undefined },
    { onEvent: () => undefined, onStateChange: () => undefined },
    {
      probeStatus: () => Promise.resolve(freeStatus()),
      runProcess: () => {
        spawns += 1;
        return {
          pid: 4242,
          // The child has already crashed: like run.ts's early return once
          // exitCode/signalCode are set, cancel() is a no-op and
          // cancelledByUs is never set.
          cancel: () => undefined,
          promise: new Promise<CliRunResult>((r) => {
            resolveCrash = r;
          }),
        };
      },
    },
  );
  void manager.start("/root");
  await wait(50);
  assert.equal(spawns, 1);
  // Crash, then a user Stop before 'close' fires; 'close' then delivers
  // the abnormal exit.
  manager.stop("/root");
  assert.equal(manager.timerPending("/root"), false);
  resolveCrash({
    exitCode: 1,
    signal: null,
    timedOut: false,
    cancelledByUs: false,
    spawnError: null,
    stderr: "",
  });
  await wait(5);
  // The Stop must not be overridden by a restart backoff.
  assert.equal(spawns, 1);
  assert.equal(manager.timerPending("/root"), false);
  assert.equal(manager.isRunning("/root"), false);
  assert.equal(manager.info("/root").status, "stopped");
  assert.equal(manager.info("/root").lastOutcome, "watch stopped by user");
  manager.dispose();
});
