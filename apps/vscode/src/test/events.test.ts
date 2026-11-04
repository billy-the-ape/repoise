import assert from "node:assert/strict";
import { test } from "node:test";
import { EventStream } from "../core/events";

function line(
  type: string,
  seq: number,
  payload: Record<string, unknown> = {},
  overrides: Record<string, unknown> = {},
): string {
  return JSON.stringify({
    schemaVersion: 1,
    runId: "index-1-1",
    sequence: seq,
    timestamp: "2026-11-03T00:00:00.000Z",
    type,
    payload,
    ...overrides,
  });
}

test("happy stream: started, progress, terminal completed", () => {
  const stream = new EventStream();
  assert.equal(
    stream.feed(line("started", 0, { operation: "index" })).kind,
    "event",
  );
  assert.equal(
    stream.feed(
      line("progress", 1, {
        operation: "index",
        phase: "discovery",
        done: 1,
        total: 2,
      }),
    ).kind,
    "event",
  );
  assert.equal(stream.feed(line("completed", 2, { result: {} })).kind, "event");
  const summary = stream.summary();
  assert.equal(summary.started, true);
  assert.equal(summary.terminal, "completed");
  assert.equal(summary.incomplete, false);
  assert.equal(summary.lastProgress?.payload.phase, "discovery");
});

test("rejects unsupported schema versions", () => {
  const stream = new EventStream();
  const outcome = stream.feed(
    line("started", 0, { operation: "index" }, { schemaVersion: 2 }),
  );
  assert.equal(outcome.kind, "unsupported-version");
  assert.equal(outcome.version, 2);
  assert.equal(stream.summary().incomplete, true);
});

test("ignores unknown event types (additive compatibility)", () => {
  const stream = new EventStream();
  assert.equal(stream.feed(line("started", 0, {})).kind, "event");
  assert.equal(
    stream.feed(line("telemetry", 1, {})).kind,
    "ignored-unknown-type",
  );
  assert.equal(stream.feed(line("completed", 2, {})).kind, "event");
  assert.equal(stream.summary().terminal, "completed");
});

test("rejects sequence regressions and bad envelopes", () => {
  const stream = new EventStream();
  assert.equal(stream.feed(line("started", 1, {})).kind, "event");
  assert.equal(stream.feed(line("progress", 0, {})).kind, "malformed-line");
  assert.equal(stream.feed(line("progress", 5, {})).kind, "event");

  const fresh = new EventStream();
  assert.equal(fresh.feed("not json").kind, "malformed-line");
  assert.equal(fresh.feed("[1, 2]").kind, "malformed-line");
  assert.equal(
    fresh.feed(
      JSON.stringify({
        schemaVersion: 1,
        runId: "r",
        sequence: 0,
        timestamp: "t",
        type: "started",
      }),
    ).kind,
    "malformed-line",
  );
});

test("duplicate terminal and events after terminal are reported", () => {
  const stream = new EventStream();
  stream.feed(line("started", 0, {}));
  assert.equal(stream.feed(line("failed", 1, {})).kind, "event");
  assert.equal(
    stream.feed(line("completed", 2, {})).kind,
    "duplicate-terminal",
  );
  assert.equal(stream.feed(line("progress", 3, {})).kind, "after-terminal");
  assert.equal(stream.summary().terminal, "failed");
});

test("a second started event is malformed", () => {
  const stream = new EventStream();
  assert.equal(stream.feed(line("started", 0, {})).kind, "event");
  assert.equal(stream.feed(line("started", 1, {})).kind, "malformed-line");
});

test("warnings are collected and incomplete streams are flagged", () => {
  const stream = new EventStream();
  stream.feed(line("started", 0, {}));
  stream.feed(line("warning", 1, { message: "provider unavailable" }));
  const summary = stream.summary();
  assert.deepEqual(summary.warnings, ["provider unavailable"]);
  assert.equal(summary.incomplete, true);
});
