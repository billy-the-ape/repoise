// Tests for the aggregate CI gate: selection errors, skipped selection
// (full suite), expected results, failures, cancellations and unexpected
// non-required jobs.
import test from "node:test";
import assert from "node:assert/strict";
import { evaluate } from "./gate.mjs";

const base = {
  GATE_SELECT_RESULT: "success",
  GATE_SELECT_OK: "true",
  GATE_RUN_CHECK: "true",
  GATE_RUN_DOCS: "true",
  GATE_CHECK_LINUX: "success",
  GATE_CHECK_MACOS: "success",
  GATE_CHECK_WINDOWS: "success",
  GATE_DOCS: "success",
};

const skippedSelection = {
  GATE_SELECT_RESULT: "skipped",
  GATE_CHECK_LINUX: "success",
  GATE_CHECK_MACOS: "success",
  GATE_CHECK_WINDOWS: "success",
  GATE_DOCS: "success",
};

test("full suite success passes", () => {
  assert.equal(evaluate(base).ok, true);
});

test("skipped selection requires the full suite to have succeeded", () => {
  assert.equal(evaluate(skippedSelection).ok, true);
  const cancelled = { ...skippedSelection, GATE_CHECK_WINDOWS: "canceled" };
  assert.equal(evaluate(cancelled).ok, false);
  const missingJob = { ...skippedSelection, GATE_DOCS: "skipped" };
  assert.equal(evaluate(missingJob).ok, false);
});

test("selection errors fail the gate even when the full suite ran", () => {
  const errored = { ...base, GATE_SELECT_OK: "false" };
  const result = evaluate(errored);
  assert.equal(result.ok, false);
  assert.match(result.failures.join(" "), /selection reported an error/);
});

test("a failed selection job fails the gate", () => {
  const failed = {
    GATE_SELECT_RESULT: "failure",
    GATE_CHECK_LINUX: "success",
    GATE_CHECK_MACOS: "success",
    GATE_CHECK_WINDOWS: "success",
    GATE_DOCS: "success",
  };
  const result = evaluate(failed);
  assert.equal(result.ok, false);
  assert.match(result.failures.join(" "), /selection reported an error/);
});

test("required check job failure and cancellation fail the gate", () => {
  assert.equal(evaluate({ ...base, GATE_CHECK_MACOS: "failure" }).ok, false);
  assert.equal(evaluate({ ...base, GATE_CHECK_LINUX: "canceled" }).ok, false);
  assert.equal(evaluate({ ...base, GATE_CHECK_LINUX: "stale" }).ok, false);
  assert.equal(evaluate({ ...base, GATE_DOCS: "failure" }).ok, false);
});

test("affected selection skips the right jobs and passes", () => {
  const docsOnly = {
    GATE_SELECT_RESULT: "success",
    GATE_SELECT_OK: "true",
    GATE_RUN_CHECK: "false",
    GATE_RUN_DOCS: "true",
    GATE_CHECK_LINUX: "skipped",
    GATE_CHECK_MACOS: "skipped",
    GATE_CHECK_WINDOWS: "skipped",
    GATE_DOCS: "success",
  };
  assert.equal(evaluate(docsOnly).ok, true);
});

test("unexpected non-required jobs fail the gate", () => {
  const stray = {
    GATE_SELECT_RESULT: "success",
    GATE_SELECT_OK: "true",
    GATE_RUN_CHECK: "false",
    GATE_RUN_DOCS: "true",
    GATE_CHECK_LINUX: "success",
    GATE_CHECK_MACOS: "skipped",
    GATE_CHECK_WINDOWS: "skipped",
    GATE_DOCS: "success",
  };
  assert.equal(evaluate(stray).ok, false);
});

test("missing selection outputs fail closed to the full suite", () => {
  const missing = {
    GATE_SELECT_RESULT: "success",
    GATE_SELECT_OK: "true",
    GATE_CHECK_LINUX: "success",
    GATE_CHECK_MACOS: "success",
    GATE_CHECK_WINDOWS: "success",
    GATE_DOCS: "success",
  };
  const result = evaluate(missing);
  assert.equal(result.ok, false);
  assert.match(result.failures.join(" "), /run_check\/run_docs are missing/);
});
