import assert from "node:assert/strict";
import { test } from "node:test";
import {
  WATCH_BACKOFF_MS,
  WATCH_MAX_RESTARTS,
  WatchExitInfo,
  decideWatchNextExit,
  initialWatchInfo,
} from "../core/watchPolicy";

function info(overrides: Partial<WatchExitInfo> = {}): WatchExitInfo {
  return {
    exitCode: 1,
    signal: null,
    timedOut: false,
    cancelledByUs: false,
    terminalType: null,
    terminalReason: null,
    incomplete: true,
    attempts: 1,
    ...overrides,
  };
}

test("our own cancellation stops without restart", () => {
  const decision = decideWatchNextExit(
    info({ cancelledByUs: true, exitCode: 0, signal: "SIGTERM" }),
  );
  assert.deepEqual(decision, {
    action: "stop",
    reason: "cancelled",
    detail: "cooperative stop",
  });
});

test("a terminal cancelled event stops without restart", () => {
  const decision = decideWatchNextExit(
    info({ terminalType: "cancelled", exitCode: 0, incomplete: false }),
  );
  assert.equal(decision.action, "stop");
  if (decision.action === "stop") {
    assert.equal(decision.reason, "cancelled");
  }
});

test("a scope-owned failure hands off to the external owner, never restarts", () => {
  const decision = decideWatchNextExit(
    info({
      terminalType: "failed",
      terminalReason: "scope-owned",
      exitCode: 1,
      incomplete: false,
    }),
  );
  assert.equal(decision.action, "stop");
  if (decision.action === "stop") {
    assert.equal(decision.reason, "external-owner");
  }
});

test("other failures restart with bounded backoff, then give up", () => {
  const crashed = (attempts: number) => decideWatchNextExit(info({ attempts }));
  const first = crashed(1);
  assert.deepEqual(first, {
    action: "restart",
    delayMs: WATCH_BACKOFF_MS[0],
    attempt: 2,
  });
  const second = crashed(2);
  assert.equal(second.action, "restart");
  if (second.action === "restart") {
    assert.equal(second.delayMs, WATCH_BACKOFF_MS[1]);
  }
  const third = crashed(3);
  assert.equal(third.action, "restart");
  if (third.action === "restart") {
    assert.equal(third.delayMs, WATCH_BACKOFF_MS[2]);
  }
  const fourth = crashed(WATCH_MAX_RESTARTS + 1);
  assert.equal(fourth.action, "stop");
  if (fourth.action === "stop") {
    assert.equal(fourth.reason, "max-restarts");
  }
});

test("a crash without a terminal event is treated as an abnormal exit", () => {
  const decision = decideWatchNextExit(
    info({ exitCode: 139, signal: null, attempts: 1 }),
  );
  assert.equal(decision.action, "restart");
});

test("spawn failure stops immediately", () => {
  const decision = decideWatchNextExit(info({ exitCode: null, signal: null }));
  assert.equal(decision.action, "stop");
  if (decision.action === "stop") {
    assert.equal(decision.reason, "spawn-failure");
  }
});

test("an unexpected completed terminal stops the supervisor", () => {
  const decision = decideWatchNextExit(
    info({ terminalType: "completed", exitCode: 0, incomplete: false }),
  );
  assert.equal(decision.action, "stop");
  if (decision.action === "stop") {
    assert.equal(decision.reason, "completed");
  }
});

test("initial watch info is idle and unowned", () => {
  const info = initialWatchInfo();
  assert.equal(info.status, "idle");
  assert.equal(info.pid, null);
  assert.equal(info.attempts, 0);
});
