// AP-07 first-slice machine contract for the installed Repoise CLI.
//
// The first slice of the AP-07 operations UI (and any other automation
// consumer) must negotiate these surfaces by capability, never by version
// string: an alpha.1-looking version predates the AP-02 contracts, and a
// newer version never grants support on its own. Version is identity;
// capability negotiation is the authority.
//
// The contract checked here is documented in docs/automation.md
// ("AP-07 first-slice contract") and defined normatively by
// schemas/capabilities.v1.schema.json and schemas/event-stream.v1.schema.json.
// The fixtures exported from this module are the compatibility test inputs:
// incompatible handshakes must be rejected with actionable upgrade guidance,
// and additive unknown fields/events must remain tolerated.

/// Features a compatibility consumer requires before driving the CLI.
export const MINIMUM_FEATURES = [
  "event-stream",
  "mcp-stdio",
  "exclusive-scope-lock",
];

/// Event-stream operations the first slice drives.
export const MINIMUM_STREAM_OPERATIONS = ["index", "watch"];

/// Machine-contract schemas shipped with the installed launcher and the
/// native archives alongside the config schema.
export { MACHINE_SCHEMA_FILES } from "./common.mjs";

const RUN_ID_PATTERN = /^[a-z0-9-]+-[0-9]+-[0-9a-f]+$/;
const TIMESTAMP_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;
const TERMINAL_TYPES = ["completed", "cancelled", "failed"];
const KNOWN_EVENT_TYPES = [
  "started",
  "progress",
  "warning",
  "heartbeat",
  ...TERMINAL_TYPES,
];

/**
 * Checks a `capabilities --json` document against the minimum AP-07
 * contract. Returns `{ ok: true, version }` or `{ ok: false, reasons,
 * guidance }` where the guidance is actionable upgrade text (the rejection
 * never falls back to guessing from the version string).
 */
export function checkAp07Handshake(doc) {
  const version =
    typeof doc?.version === "string" ? doc.version : "unknown version";
  const problems = [];
  if (doc?.schemaVersion !== 1)
    problems.push('the capabilities document must have "schemaVersion": 1');
  if (doc?.name !== "repoise")
    problems.push('the capabilities document must have "name": "repoise"');
  if (!Array.isArray(doc?.features)) {
    problems.push("the capabilities document has no features list");
  } else {
    for (const feature of MINIMUM_FEATURES)
      if (!doc.features.includes(feature))
        problems.push(
          `the capabilities document is missing feature "${feature}"`,
        );
  }
  const stream = doc?.eventStream;
  if (!stream) {
    problems.push(
      "the capabilities document has no eventStream section (this build predates the --events streams)",
    );
  } else {
    if (!stream.operations?.includes)
      problems.push("the eventStream section has no operations list");
    else
      for (const operation of MINIMUM_STREAM_OPERATIONS)
        if (!stream.operations.includes(operation))
          problems.push(
            `the eventStream section does not stream the "${operation}" operation`,
          );
  }
  if (!Array.isArray(doc?.compatibility?.supportedEventStreamVersions)) {
    problems.push(
      "the compatibility section does not list supported event-stream versions",
    );
  } else if (!doc.compatibility.supportedEventStreamVersions.includes(1)) {
    problems.push("no supported event-stream version in common (need v1)");
  }
  const codes = doc?.exitCodes;
  if (
    !codes ||
    codes.ok !== 0 ||
    codes.operational !== 1 ||
    codes.usage !== 2 ||
    codes.indexMissingOrStale !== 3 ||
    codes.coverageNotMet !== 4
  )
    problems.push("the stable exit-code table is missing or shifted");
  if (problems.length > 0)
    return {
      ok: false,
      reasons: problems,
      guidance:
        `This Repoise CLI (v${version}) does not expose the Repoise automation ` +
        `contract (capabilities v1, event-stream v1, exclusive scope lock, ` +
        `stable exit codes). Capability negotiation, not the version string, ` +
        `decides compatibility. Upgrade to Repoise 0.1.0-alpha.2 or later, ` +
        `then re-run \`repoise capabilities --json\` and re-check.`,
    };
  return { ok: true, version };
}

/**
 * Validates one full NDJSON event stream (already parsed into an array of
 * envelope objects) against the v1 event-stream contract for the given
 * operation and resolved root. Unknown event types and unknown fields are
 * tolerated (additive policy); everything structural must hold. Returns
 * `{ ok: true }` or `{ ok: false, problems }`.
 */
export function validateEventStream(events, { operation, root }) {
  const problems = [];
  if (!Array.isArray(events) || events.length === 0) {
    return {
      ok: false,
      problems: ["the event stream is empty"],
    };
  }
  const runId = events[0]?.runId;
  let expectedSequence = 0;
  events.forEach((event, position) => {
    const where = `event ${position} (${JSON.stringify(event?.type ?? null)});`;
    for (const field of [
      "schemaVersion",
      "runId",
      "sequence",
      "timestamp",
      "type",
      "payload",
    ])
      if (event?.[field] === undefined)
        problems.push(`the event is missing the ${where} "${field}" field`);
    if (event?.schemaVersion !== 1)
      problems.push(
        `unsupported event-stream schemaVersion ${JSON.stringify(
          event?.schemaVersion,
        )} ${where}`,
      );
    if (!runId || event?.runId !== runId)
      problems.push(`runId changed or is missing ${where}`);
    if (!RUN_ID_PATTERN.test(String(event?.runId ?? "")))
      problems.push(`runId has an unexpected shape ${where}`);
    if (event?.sequence !== expectedSequence)
      problems.push(
        `sequence is ${JSON.stringify(event?.sequence)} but ${expectedSequence} was next ${where}`,
      );
    expectedSequence += 1;
    if (!TIMESTAMP_PATTERN.test(String(event?.timestamp ?? "")))
      problems.push(`timestamp has the wrong shape ${where}`);
    const type = event?.type;
    if (typeof type !== "string") {
      problems.push(`event type is not a string ${where}`);
      return;
    }
    // Every run opens with "started" even when the producer uses unknown
    // additive types, so this is checked before the unknown-type skip.
    if (position === 0 && type !== "started")
      problems.push(`the first event must be "started", got "${type}"`);
    if (!KNOWN_EVENT_TYPES.includes(type)) {
      // Additive compatibility: unknown event types are ignored, but the
      // envelope fields above still apply.
      return;
    }
    if (position === 0) {
      if (event?.payload?.operation !== operation)
        problems.push(
          `the started event reports operation ${JSON.stringify(
            event?.payload?.operation,
          )}, expected "${operation}"`,
        );
      const scope = event?.payload?.scope;
      if (
        !scope ||
        typeof scope.repoId !== "string" ||
        typeof scope.worktreeId !== "string" ||
        !["working-tree", "committed", "plain-directory"].includes(
          scope.snapshotMode,
        )
      )
        problems.push(
          "the started event carries no valid scope (repoId/worktreeId/snapshotMode)",
        );
      else if (root !== undefined && scope.root !== root)
        problems.push(
          `scope mismatch: the stream reports root ${JSON.stringify(
            scope.root,
          )}, the run was started for ${JSON.stringify(root)}`,
        );
      return;
    }
  });
  const terminals = events.filter((event) =>
    TERMINAL_TYPES.includes(event?.type),
  );
  const lastEvent = events[events.length - 1];
  if (terminals.length === 0) {
    problems.push(
      "no terminal event (completed/cancelled/failed): a stream without its terminal event is incomplete output",
    );
  } else if (terminals[terminals.length - 1] !== lastEvent) {
    // A terminal event was flushed but the stream kept going afterwards: the
    // CLI emits its single terminal as the last event, so a tail after it
    // means the process likely died mid-stream (incomplete output), not a
    // valid additive extension.
    problems.push(
      `the terminal event "${terminals[terminals.length - 1].type}" is not the last event: the stream ended mid-way (incomplete output)`,
    );
  } else if (terminals.length > 1) {
    problems.push(
      `expected exactly one terminal event, got ${terminals.length}`,
    );
  }
  return problems.length > 0 ? { ok: false, problems } : { ok: true };
}

/// Compatibility fixtures. The installed candidate must accept the
/// compatible and additive documents and reject every legacy/incompatible
/// one with actionable upgrade guidance.
const compatibleHandshake = {
  schemaVersion: 1,
  name: "repoise",
  version: "0.1.0-alpha.2",
  eventStream: {
    schema: "schemas/event-stream.v1.schema.json",
    version: 1,
    operations: ["index", "watch"],
    heartbeatIntervalMs: 15000,
  },
  features: ["event-stream", "mcp-stdio", "exclusive-scope-lock"],
  commands: [{ name: "index", machine: ["json", "events"] }],
  benchmark: { supported: true, command: "bench" },
  exitCodes: {
    ok: 0,
    operational: 1,
    usage: 2,
    indexMissingOrStale: 3,
    coverageNotMet: 4,
  },
  compatibility: {
    policy: "additive",
    supportedEventStreamVersions: [1],
  },
};
export const handshakeFixtures = {
  compatible: compatibleHandshake,
  // The same document with unknown additive fields/types: must be accepted.
  additive: {
    ...compatibleHandshake,
    experimentalTelemetry: { optIn: true },
    features: [...compatibleHandshake.features, "experimental-history-ui"],
  },
  // A plausible pre-AP-02 document: an alpha.1-looking binary advertised
  // mcp-stdio only, with no events or ownership surfaces. Must be rejected
  // even though its version string looks like a current prerelease.
  legacyAlpha1: {
    schemaVersion: 1,
    name: "repoise",
    version: "0.1.0-alpha.1",
    eventStream: {
      schema: "schemas/event-stream.v1.schema.json",
      version: 1,
      operations: [],
      heartbeatIntervalMs: 15000,
    },
    features: ["mcp-stdio"],
    commands: [{ name: "index", machine: ["json"] }],
    benchmark: { supported: false },
    exitCodes: { ok: 0, operational: 1, usage: 2 },
    compatibility: {
      policy: "additive",
      supportedEventStreamVersions: [1],
    },
  },
  // A future producer speaking a stream version this consumer does not
  // support: rejected on version, not on the (newer) version string.
  futureEventStream: {
    ...compatibleHandshake,
    eventStream: {
      ...compatibleHandshake.eventStream,
      version: 2,
      schema: "schemas/event-stream.v2.schema.json",
    },
    compatibility: {
      policy: "additive",
      supportedEventStreamVersions: [2],
    },
  },
  // A brand-new version string but a minimal document: the version string
  // must never be taken as evidence of support.
  versionOnly: {
    schemaVersion: 1,
    name: "repoise",
    version: "0.2.0",
    features: ["mcp-stdio"],
  },
};

/// Event-stream fixtures: valid streams, an incomplete stream (no terminal
/// event, i.e. an abrupt kill), a sequence gap, a scope mismatch and an
/// unknown additive event type that must be tolerated.
const fixtureScope = {
  root: "/tmp/fixture",
  repoId: "repo-1",
  worktreeId: "main",
  snapshotMode: "plain-directory",
};
function envelope(runId, sequence, type, payload) {
  return {
    schemaVersion: 1,
    runId,
    sequence,
    timestamp: "2026-11-03T12:00:00.000Z",
    type,
    payload,
  };
}
export const eventStreamFixtures = {
  validIndexRun: {
    operation: "index",
    root: fixtureScope.root,
    events: [
      envelope("index-41-abc", 0, "started", {
        operation: "index",
        pid: 41,
        scope: fixtureScope,
      }),
      envelope("index-41-abc", 1, "progress", {
        operation: "index",
        phase: "discovery",
        unit: "file",
        done: 1,
        total: 1,
        elapsedMs: 12,
      }),
      envelope("index-41-abc", 2, "completed", {
        operation: "index",
        scope: fixtureScope,
        result: { files_indexed: 1 },
      }),
    ],
  },
  additiveUnknownType: {
    operation: "watch",
    root: fixtureScope.root,
    events: [
      envelope("watch-42-def", 0, "started", {
        operation: "watch",
        pid: 42,
        scope: fixtureScope,
      }),
      envelope("watch-42-def", 1, "experimental-metric", {
        operation: "watch",
        value: 7,
      }),
      envelope("watch-42-def", 2, "cancelled", {
        operation: "watch",
        scope: fixtureScope,
        reason: "signal",
      }),
    ],
  },
  // An unknown additive type in the first slot: the envelope is valid and
  // the single terminal event is last, but the run must open with
  // "started", so this stream must be rejected.
  unknownFirstEvent: {
    operation: "watch",
    root: fixtureScope.root,
    events: [
      envelope("watch-47-9", 0, "experimental-metric", {
        operation: "watch",
        value: 7,
      }),
      envelope("watch-47-9", 1, "completed", {
        operation: "watch",
        scope: fixtureScope,
        result: {},
      }),
    ],
  },
  missingTerminal: {
    operation: "index",
    root: fixtureScope.root,
    events: [
      envelope("index-43-0", 0, "started", {
        operation: "index",
        pid: 43,
        scope: fixtureScope,
      }),
      envelope("index-43-0", 1, "progress", {
        operation: "index",
        phase: "discovery",
        unit: "file",
        done: 0,
        total: null,
        elapsedMs: 3,
      }),
    ],
  },
  // A terminal event was flushed but the stream kept going afterwards: the
  // CLI emits its single terminal as the last event, so a tail after it is
  // incomplete output (the process likely died mid-stream).
  terminalMidStream: {
    operation: "index",
    root: fixtureScope.root,
    events: [
      envelope("index-48-5", 0, "started", {
        operation: "index",
        pid: 48,
        scope: fixtureScope,
      }),
      envelope("index-48-5", 1, "completed", {
        operation: "index",
        scope: fixtureScope,
        result: { files_indexed: 1 },
      }),
      envelope("index-48-5", 2, "progress", {
        operation: "index",
        phase: "publication",
        unit: "file",
        done: 1,
        total: 1,
        elapsedMs: 9,
      }),
    ],
  },
  sequenceGap: {
    operation: "index",
    root: fixtureScope.root,
    events: [
      envelope("index-44-1", 0, "started", {
        operation: "index",
        pid: 44,
        scope: fixtureScope,
      }),
      envelope("index-44-1", 2, "completed", {
        operation: "index",
        scope: fixtureScope,
        result: { files_indexed: 0 },
      }),
    ],
  },
  scopeMismatch: {
    operation: "index",
    root: "/tmp/other-root",
    events: [
      envelope("index-45-2", 0, "started", {
        operation: "index",
        pid: 45,
        scope: fixtureScope,
      }),
      envelope("index-45-2", 1, "completed", {
        operation: "index",
        scope: fixtureScope,
        result: { files_indexed: 0 },
      }),
    ],
  },
  unsupportedSchemaVersion: {
    operation: "index",
    root: fixtureScope.root,
    events: [
      envelope("index-46-3", 0, "started", {
        operation: "index",
        pid: 46,
        scope: fixtureScope,
      }),
      {
        schemaVersion: 2,
        runId: "index-46-3",
        sequence: 1,
        timestamp: "2026-11-03T12:00:01.000Z",
        type: "completed",
        payload: { operation: "index", scope: fixtureScope },
      },
    ],
  },
};
