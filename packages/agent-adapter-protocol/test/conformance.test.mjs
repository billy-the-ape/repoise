// Protocol v1 conformance suite: synthetic fake-adapter fixtures driven
// through the coordinator host. No live inference; every scenario is
// deterministic and covered by a checked-in fixture.
import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  statSync,
  existsSync,
  readFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { runAdapter } from "../src/index.js";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "fixtures");
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function makeRequest(root, overrides = {}) {
  const base = {
    protocolVersion: 1,
    identity: {
      experimentId: "exp-1",
      trialId: "trial-1",
      taskId: "task-1",
      replication: 0,
    },
    workspace: {
      root: "/tmp/ap03-workspace",
      revision: null,
      scope: "read",
      allowedWritePaths: [],
    },
    task: {
      prompt: "Say hello",
      promptFile: null,
      taskVersion: "1",
      taskHash: "sha256:test",
    },
    condition: {
      retrievalMode: "baseline",
      requiredCapabilities: [],
      mcp: null,
      guidanceOverlay: null,
    },
    profile: {
      harness: { name: "fake", revision: null },
      provider: null,
      model: null,
      reasoning: null,
      compaction: null,
      sampling: null,
    },
    limits: {},
    outputs: {
      artifactDir: join(root, "artifacts"),
      eventsFile: join(root, "events.ndjson"),
      resultFile: join(root, "result.json"),
    },
  };
  const merged = { ...base, ...overrides };
  merged.limits = { ...base.limits, ...(overrides.limits ?? {}) };
  merged.condition = { ...base.condition, ...(overrides.condition ?? {}) };
  return merged;
}

async function runFixture(name, { requestOverrides, runOptions } = {}) {
  const root = mkdtempSync(join(tmpdir(), "ap03-protocol-"));
  const request = makeRequest(root, requestOverrides);
  const result = await runAdapter(request, {
    executable: process.execPath,
    args: [join(fixtures, name)],
    ...(runOptions ?? {}),
  });
  return { root, request, result };
}

test("successful run: handshake, events, usage, outputs", async () => {
  const { root, result } = await runFixture("fake-ok.mjs");
  assert.equal(result.status, "completed");
  assert.equal(result.reason, null);
  assert.equal(result.adapter.name, "fake");
  assert.equal(result.adapter.version, "1.0.0");
  assert.equal(result.harness.revisionSource, "pinned");
  assert.equal(result.events[0].t, "hello");
  assert.equal(result.events.at(-1).t, "final");
  assert.equal(result.exitCode, 0);
  assert.ok(result.elapsedMs >= 0);
  assert.equal(result.adapterDurationMs, 120);
  // Delta events sum to the final total; the total is authoritative.
  assert.deepEqual(
    {
      input: result.usage.inputTokens,
      output: result.usage.outputTokens,
      cost: result.usage.costUsd,
    },
    { input: 15, output: 25, cost: 0.015 },
  );
  assert.equal(result.usage.estimated, false);
  assert.deepEqual(result.unknownEventTypes, []);
  assert.equal(result.errors.length, 0);
  const eventsFile = readFileSync(join(root, "events.ndjson"), "utf8");
  assert.equal(eventsFile.trim().split("\n").length, result.events.length);
  const resultFile = JSON.parse(readFileSync(join(root, "result.json"), "utf8"));
  assert.equal(resultFile.status, "completed");
});

test("incompatible protocol version fails preflight", async () => {
  const { result } = await runFixture("fake-bad-version.mjs");
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "unsupported_protocol_version");
  assert.equal(result.events.length, 0, "a rejected hello is not an accepted event");
  assert.equal(result.hello, null);
});

test("missing required capability fails preflight, never approximated", async () => {
  const { result } = await runFixture("fake-missing-telemetry.mjs", {
    requestOverrides: {
      condition: { requiredCapabilities: ["tool_telemetry"] },
    },
  });
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "missing_required_capability");
  assert.ok(result.errors.join(" ").includes("tool_telemetry"));
});

test("cumulative usage takes max, never sums snapshots and final", async () => {
  const { result } = await runFixture("fake-cumulative.mjs");
  assert.equal(result.status, "completed");
  assert.equal(result.usage.inputTokens, 30);
  assert.equal(result.usage.costUsd, 0.03);
  assert.equal(result.usage.outputTokens, null);
});

test("delta usage sums deltas and does not add the final total twice", async () => {
  const { result } = await runFixture("fake-delta.mjs");
  assert.equal(result.status, "completed");
  assert.equal(result.usage.inputTokens, 30);
});

test("malformed JSON line is a protocol error", async () => {
  const { result } = await runFixture("fake-malformed.mjs");
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "malformed_event");
});

test("truncated trailing line is a protocol error", async () => {
  const { result } = await runFixture("fake-truncated.mjs");
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "truncated_event");
});

test("oversized event line is a protocol error", async () => {
  const { result } = await runFixture("fake-oversized.mjs", {
    requestOverrides: { limits: { maxEventBytes: 200 } },
  });
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "event_oversized");
});

test("stalled process hits the wall clock and is escalated to timeout", async () => {
  const { result } = await runFixture("fake-stalled.mjs", {
    requestOverrides: { limits: { wallClockMs: 1000 } },
    runOptions: { cancelGraceMs: 200 },
  });
  assert.equal(result.status, "timeout");
  assert.equal(result.reason, "wall_clock_exceeded");
  assert.ok(result.elapsedMs >= 1000);
});

test("failed child without a final event is adapter_error", async () => {
  const { result } = await runFixture("fake-failed-child.mjs");
  assert.equal(result.status, "adapter_error");
  assert.equal(result.reason, "nonzero_exit");
  assert.equal(result.exitCode, 1);
});

test("host cancellation: cooperative adapter emits final cancelled", async () => {
  const controller = new AbortController();
  const pending = runFixture("fake-cancel.mjs", {
    runOptions: { signal: controller.signal, cancelGraceMs: 3000 },
  });
  setTimeout(() => controller.abort(), 300);
  const { result } = await pending;
  assert.equal(result.status, "cancelled");
  assert.equal(result.events.at(-1).t, "final");
  assert.equal(result.events.at(-1).status, "cancelled");
});

test("leaked grandchild is killed with the owned tree", async () => {
  const root = mkdtempSync(join(tmpdir(), "ap03-leak-"));
  const heartbeatFile = join(root, "heartbeat.txt");
  const controller = new AbortController();
  const request = makeRequest(root);
  const pending = runAdapter(request, {
    executable: process.execPath,
    args: [join(fixtures, "fake-leak.mjs")],
    signal: controller.signal,
    cancelGraceMs: 200,
    env: { LEAK_HB_FILE: heartbeatFile },
  });
  setTimeout(() => controller.abort(), 400);
  const result = await pending;
  assert.equal(result.status, "cancelled");
  assert.ok(existsSync(heartbeatFile));
  const first = statSync(heartbeatFile).size;
  assert.ok(first > 0, "heartbeat ran before the kill");
  await sleep(600);
  const second = statSync(heartbeatFile).size;
  assert.equal(
    second,
    first,
    "leaked grandchild kept writing after the owned tree was killed",
  );
});

test("event after the final is a protocol error", async () => {
  const { result } = await runFixture("fake-trailing.mjs");
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "events_after_final");
});

test("host timeout is not replaced by a cooperative cancelled final", async () => {
  const { result } = await runFixture("fake-timeout-coop.mjs", {
    requestOverrides: { limits: { wallClockMs: 1000 } },
    runOptions: { cancelGraceMs: 200 },
  });
  assert.equal(result.status, "timeout");
  assert.equal(result.reason, "wall_clock_exceeded");
  assert.equal(result.events.at(-1).t, "final");
  assert.equal(result.events.at(-1).status, "cancelled");
});

test("grandchild surviving a clean adapter exit is swept with the group", async () => {
  const root = mkdtempSync(join(tmpdir(), "ap03-exit-leak-"));
  const heartbeatFile = join(root, "heartbeat.txt");
  const request = makeRequest(root);
  const result = await runAdapter(request, {
    executable: process.execPath,
    args: [join(fixtures, "fake-exit-leak.mjs")],
    env: { LEAK_HB_FILE: heartbeatFile },
  });
  assert.equal(result.status, "completed");
  // The group sweep runs before runAdapter resolves; give the grandchild a
  // moment that would have produced more heartbeats, then verify stability.
  await sleep(300);
  const first = existsSync(heartbeatFile) ? statSync(heartbeatFile).size : 0;
  await sleep(500);
  const second = existsSync(heartbeatFile) ? statSync(heartbeatFile).size : 0;
  assert.equal(
    second,
    first,
    "grandchild kept writing after the adapter exited cleanly",
  );
});

test("unresolvable executable is adapter_error, not a host crash", async () => {
  const root = mkdtempSync(join(tmpdir(), "ap03-spawn-"));
  const request = makeRequest(root);
  const result = await runAdapter(request, {
    executable: "/nonexistent/ap03-adapter",
    args: [],
  });
  assert.equal(result.status, "adapter_error");
  assert.ok(result.reason.startsWith("spawn_failed"));
});

test("schema-minimal request (absent optional fields) is accepted", async () => {
  const root = mkdtempSync(join(tmpdir(), "ap03-minimal-"));
  const request = {
    protocolVersion: 1,
    identity: { experimentId: "e", trialId: "t", taskId: "k", replication: 0 },
    workspace: { root: "/tmp/ap03-workspace", scope: "read" },
    task: { prompt: "Say hello", taskVersion: "1", taskHash: "sha256:x" },
    condition: { retrievalMode: "baseline", requiredCapabilities: [] },
    profile: { harness: { name: "fake" } },
    outputs: { artifactDir: join(root, "artifacts") },
  };
  const result = await runAdapter(request, {
    executable: process.execPath,
    args: [join(fixtures, "fake-ok.mjs")],
  });
  assert.equal(result.status, "completed");
  assert.equal(result.reason, null);
});
