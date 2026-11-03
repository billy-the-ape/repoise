import { SUPPORTED_EVENT_STREAM_VERSIONS } from "./events";

/**
 * Capabilities handshake parsing and compatibility judgment
 * (`repoise capabilities --json`; schemas/capabilities.v1.schema.json).
 */

export interface CapabilitiesDoc {
  schemaVersion: number;
  name: string;
  version: string;
  eventStream: {
    schema: string;
    version: number;
    operations: string[];
    heartbeatIntervalMs: number;
  };
  features: string[];
  commands: { name: string; machine: string[] }[];
  benchmark: { supported: boolean; command: string };
  exitCodes: Record<string, number>;
  compatibility: {
    policy: string;
    supportedEventStreamVersions: number[];
  };
}

export type CapabilitiesParse =
  { ok: true; doc: CapabilitiesDoc } | { ok: false; problem: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asStringArray(value: unknown): string[] | null {
  return Array.isArray(value) && value.every((v) => typeof v === "string")
    ? value
    : null;
}

function asNumberArray(value: unknown): number[] | null {
  return Array.isArray(value) && value.every((v) => typeof v === "number")
    ? value
    : null;
}

export function parseCapabilities(raw: string | unknown): CapabilitiesParse {
  let doc: unknown;
  if (typeof raw === "string") {
    try {
      doc = JSON.parse(raw);
    } catch {
      return { ok: false, problem: "invalid-json" };
    }
  } else {
    doc = raw;
  }
  if (!isRecord(doc)) {
    return { ok: false, problem: "not-object" };
  }
  if (typeof doc.name !== "string" || typeof doc.version !== "string") {
    return { ok: false, problem: "missing-name-or-version" };
  }
  if (typeof doc.schemaVersion !== "number") {
    return { ok: false, problem: "missing-schemaVersion" };
  }
  const eventStream = doc.eventStream;
  if (!isRecord(eventStream)) {
    return { ok: false, problem: "missing-eventStream" };
  }
  const features = asStringArray(doc.features);
  if (features === null) {
    return { ok: false, problem: "bad-features" };
  }
  const commandsRaw = doc.commands;
  if (!Array.isArray(commandsRaw)) {
    return { ok: false, problem: "missing-commands" };
  }
  const commands: { name: string; machine: string[] }[] = [];
  for (const command of commandsRaw) {
    if (!isRecord(command) || typeof command.name !== "string") {
      return { ok: false, problem: "bad-commands" };
    }
    const machine = asStringArray(command.machine) ?? [];
    commands.push({ name: command.name, machine });
  }
  const compatibility = doc.compatibility;
  if (!isRecord(compatibility)) {
    return { ok: false, problem: "missing-compatibility" };
  }
  const supportedVersions = asNumberArray(
    compatibility.supportedEventStreamVersions,
  );
  if (supportedVersions === null) {
    return { ok: false, problem: "missing-supportedEventStreamVersions" };
  }
  return {
    ok: true,
    doc: {
      schemaVersion: doc.schemaVersion,
      name: doc.name,
      version: doc.version,
      eventStream: {
        schema:
          typeof eventStream.schema === "string" ? eventStream.schema : "",
        version:
          typeof eventStream.version === "number" ? eventStream.version : 0,
        operations: asStringArray(eventStream.operations) ?? [],
        heartbeatIntervalMs:
          typeof eventStream.heartbeatIntervalMs === "number"
            ? eventStream.heartbeatIntervalMs
            : 0,
      },
      features,
      commands,
      benchmark: {
        supported: isRecord(doc.benchmark)
          ? doc.benchmark.supported === true
          : false,
        command:
          isRecord(doc.benchmark) && typeof doc.benchmark.command === "string"
            ? doc.benchmark.command
            : "",
      },
      exitCodes: isRecord(doc.exitCodes)
        ? (doc.exitCodes as Record<string, number>)
        : {},
      compatibility: {
        policy:
          typeof compatibility.policy === "string" ? compatibility.policy : "",
        supportedEventStreamVersions: supportedVersions,
      },
    },
  };
}

export interface CapabilitiesCheck {
  compatible: boolean;
  problems: string[];
}

/**
 * Additive-compatibility judgment: a producer is incompatible when its
 * schema version is not 1 or when its supported event-stream versions do not
 * include 1 (this consumer's supported set). Missing optional features are
 * reported as problems but do not make the CLI unusable for read-only work.
 */
export function checkCapabilities(doc: CapabilitiesDoc): CapabilitiesCheck {
  const problems: string[] = [];
  let compatible = true;

  if (doc.schemaVersion !== 1) {
    compatible = false;
    problems.push(
      `capabilities schema version ${doc.schemaVersion} is not supported by this extension`,
    );
  }
  if (
    !doc.compatibility.supportedEventStreamVersions.some((v) =>
      SUPPORTED_EVENT_STREAM_VERSIONS.includes(v),
    )
  ) {
    compatible = false;
    problems.push(
      "the CLI does not support event-stream version 1; live index/watch progress will be unavailable",
    );
  }
  const features = new Set(doc.features);
  if (!features.has("event-stream")) {
    problems.push(
      "event-stream feature missing: live progress unavailable (status/index still work via --json)",
    );
  }
  if (!features.has("mcp-stdio")) {
    problems.push(
      "mcp-stdio feature missing: MCP definition setup is unsupported",
    );
  }
  if (!features.has("exclusive-scope-lock")) {
    problems.push(
      "exclusive-scope-lock feature missing: writer-ownership display is unreliable",
    );
  }
  const commands = new Set(doc.commands.map((c) => c.name));
  for (const name of ["status", "index", "watch", "mcp"]) {
    if (!commands.has(name)) {
      problems.push(`command '${name}' missing from the capabilities document`);
    }
  }
  return { compatible, problems };
}
