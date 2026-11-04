import assert from "node:assert/strict";
import { test } from "node:test";
import {
  CapabilitiesDoc,
  checkCapabilities,
  parseCapabilities,
} from "../core/capabilities";

function doc(overrides: Partial<CapabilitiesDoc> = {}): CapabilitiesDoc {
  return {
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
    commands: [
      { name: "status", machine: ["json"] },
      { name: "index", machine: ["json", "events"] },
      { name: "watch", machine: ["json", "events"] },
      { name: "mcp", machine: ["mcp-stdio"] },
    ],
    benchmark: { supported: true, command: "bench" },
    exitCodes: {
      ok: 0,
      operational: 1,
      usage: 2,
      indexMissingOrStale: 3,
      coverageNotMet: 4,
    },
    compatibility: { policy: "additive", supportedEventStreamVersions: [1] },
    ...overrides,
  };
}

test("parses a well-formed document from JSON text", () => {
  const parsed = parseCapabilities(JSON.stringify(doc()));
  assert.equal(parsed.ok, true);
  if (!parsed.ok) return;
  assert.equal(parsed.doc.name, "repoise");
  assert.deepEqual(parsed.doc.compatibility.supportedEventStreamVersions, [1]);
  assert.deepEqual(parsed.doc.eventStream.operations, ["index", "watch"]);
});

test("rejects garbage input with a stable problem code", () => {
  assert.equal(parseCapabilities("not json").ok, false);
  assert.equal(parseCapabilities({ name: "repoise" }).ok, false);
  const broken = parseCapabilities(
    JSON.stringify({ ...doc(), features: "nope" }),
  );
  assert.equal(broken.ok, false);
  if (!broken.ok) {
    assert.equal(broken.problem, "bad-features");
  }
});

test("a compatible producer passes the check cleanly", () => {
  const check = checkCapabilities(doc());
  assert.equal(check.compatible, true);
  assert.deepEqual(check.problems, []);
});

test("an unknown capabilities schema version is incompatible", () => {
  const check = checkCapabilities(doc({ schemaVersion: 2 }));
  assert.equal(check.compatible, false);
  assert.ok(check.problems.some((p) => p.includes("schema version 2")));
});

test("a producer without event-stream version 1 is incompatible", () => {
  const check = checkCapabilities(
    doc({
      compatibility: { policy: "additive", supportedEventStreamVersions: [2] },
    }),
  );
  assert.equal(check.compatible, false);
  assert.ok(check.problems.some((p) => p.includes("event-stream version 1")));
});

test("missing optional features are reported but keep compatibility", () => {
  const check = checkCapabilities(
    doc({ features: ["mcp-stdio", "exclusive-scope-lock"] }),
  );
  assert.equal(check.compatible, true);
  assert.ok(
    check.problems.some((p) => p.includes("event-stream feature missing")),
  );
});

test("missing core commands are reported as problems", () => {
  const check = checkCapabilities(
    doc({ commands: [{ name: "status", machine: ["json"] }] }),
  );
  assert.equal(check.compatible, true);
  const names = check.problems
    .filter((p) => p.includes("missing from the capabilities document"))
    .join(" ");
  assert.ok(
    names.includes("index") && names.includes("watch") && names.includes("mcp"),
  );
});
