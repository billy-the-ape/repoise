import assert from "node:assert/strict";
import test from "node:test";
import {
  checkAp07Handshake,
  validateEventStream,
  handshakeFixtures,
  eventStreamFixtures,
} from "./ap07-contract.mjs";

test("the current candidate handshake passes the minimum contract", () => {
  const verdict = checkAp07Handshake(handshakeFixtures.compatible);
  assert.deepEqual(verdict, { ok: true, version: "0.1.0-alpha.2" });
});

test("additive unknown fields and features remain tolerated", () => {
  assert.ok(checkAp07Handshake(handshakeFixtures.additive).ok);
});

test("an alpha.1-looking legacy handshake is rejected with upgrade guidance", () => {
  const verdict = checkAp07Handshake(handshakeFixtures.legacyAlpha1);
  assert.equal(verdict.ok, false);
  // The rejection is capability-based, with actionable upgrade guidance,
  // never inferred from (or excused by) the version string.
  assert.match(
    verdict.guidance,
    /Upgrade to Repoise 0\.1\.0-alpha\.2 or later/,
  );
  assert.match(verdict.guidance, /event-stream/);
  assert.match(verdict.guidance, /exclusive scope lock/);
  assert.ok(verdict.reasons.some((reason) => reason.includes("event-stream")));
  assert.ok(
    verdict.reasons.some((reason) => reason.includes("exclusive-scope-lock")),
  );
  assert.ok(
    verdict.reasons.some((reason) =>
      reason.includes('does not stream the "watch"'),
    ),
  );
});

test("a future event-stream version is rejected by stream version", () => {
  const verdict = checkAp07Handshake(handshakeFixtures.futureEventStream);
  assert.equal(verdict.ok, false);
  assert.ok(
    verdict.reasons.some((reason) => reason.includes("event-stream version")),
  );
});

test("a new version string alone never establishes support", () => {
  const verdict = checkAp07Handshake(handshakeFixtures.versionOnly);
  assert.equal(verdict.ok, false);
  assert.ok(verdict.reasons.every((reason) => !reason.includes("0.2.0")));
  // Every problem names a missing capability, not the version string.
  assert.ok(
    verdict.reasons.length >= 3,
    `expected multiple capability problems: ${verdict.reasons}`,
  );
});

test("a missing capabilities document is rejected, not guessed", () => {
  for (const doc of [undefined, {}, { version: "0.1.0-alpha.1" }]) {
    const verdict = checkAp07Handshake(doc);
    assert.equal(verdict.ok, false);
    assert.match(verdict.guidance, /Upgrade to Repoise 0\.1\.0-alpha\.2/);
  }
});

test("a valid index stream passes stream validation", () => {
  const fixture = eventStreamFixtures.validIndexRun;
  assert.deepEqual(
    validateEventStream(fixture.events, {
      operation: fixture.operation,
      root: fixture.root,
    }),
    { ok: true },
  );
});

test("unknown additive event types are tolerated in a valid stream", () => {
  const fixture = eventStreamFixtures.additiveUnknownType;
  assert.deepEqual(
    validateEventStream(fixture.events, {
      operation: fixture.operation,
      root: fixture.root,
    }),
    { ok: true },
  );
});

test("a stream without a terminal event is incomplete output", () => {
  const fixture = eventStreamFixtures.missingTerminal;
  const verdict = validateEventStream(fixture.events, {
    operation: fixture.operation,
    root: fixture.root,
  });
  assert.equal(verdict.ok, false);
  assert.ok(
    verdict.problems.some((problem) => problem.includes("terminal event")),
    `expected the terminal problem: ${verdict.problems}`,
  );
});

test("a sequence gap is rejected", () => {
  const fixture = eventStreamFixtures.sequenceGap;
  const verdict = validateEventStream(fixture.events, {
    operation: fixture.operation,
    root: fixture.root,
  });
  assert.equal(verdict.ok, false);
  assert.ok(
    verdict.problems.some((problem) => problem.includes("sequence")),
    `expected the sequence problem: ${verdict.problems}`,
  );
});

test("a scope mismatch against the requested root is rejected", () => {
  const fixture = eventStreamFixtures.scopeMismatch;
  const verdict = validateEventStream(fixture.events, {
    operation: fixture.operation,
    root: fixture.root,
  });
  assert.equal(verdict.ok, false);
  assert.ok(
    verdict.problems.some((problem) => problem.includes("scope mismatch")),
    `expected the scope problem: ${verdict.problems}`,
  );
});

test("an unsupported event-stream schemaVersion is rejected per event", () => {
  const fixture = eventStreamFixtures.unsupportedSchemaVersion;
  const verdict = validateEventStream(fixture.events, {
    operation: fixture.operation,
    root: fixture.root,
  });
  assert.equal(verdict.ok, false);
  assert.ok(
    verdict.problems.some((problem) => problem.includes("schemaVersion")),
    `expected the schemaVersion problem: ${verdict.problems}`,
  );
});

test("an empty stream is rejected", () => {
  const verdict = validateEventStream([], { operation: "index", root: "/tmp" });
  assert.equal(verdict.ok, false);
});
