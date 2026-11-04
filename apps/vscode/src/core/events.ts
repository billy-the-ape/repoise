import { RepoiseEvent } from "./types";

/**
 * Consumer-side state machine for one `--events` NDJSON run.
 *
 * Contract (docs/automation.md, event-stream.v1 schema): one run starts with
 * `started`, ends with exactly one terminal event, `sequence` is strictly
 * increasing, unknown event types and fields are ignored, and a stream whose
 * `schemaVersion` is unsupported must be rejected.
 */

export const SUPPORTED_EVENT_STREAM_VERSIONS: readonly number[] = [1];

const TERMINAL_TYPES = new Set<string>(["completed", "cancelled", "failed"]);
const KNOWN_TYPES = new Set<string>([
  "started",
  "progress",
  "warning",
  "heartbeat",
  "completed",
  "cancelled",
  "failed",
]);

export type StreamOutcome =
  | { kind: "event"; event: RepoiseEvent }
  | { kind: "malformed-line"; problem: string }
  | { kind: "unsupported-version"; version: unknown }
  | { kind: "ignored-unknown-type"; type: string }
  | { kind: "after-terminal"; type: string }
  | { kind: "duplicate-terminal"; type: string };

export interface StreamSummary {
  started: boolean;
  terminal: "completed" | "cancelled" | "failed" | null;
  terminalEvent: RepoiseEvent | null;
  warnings: string[];
  lastProgress: RepoiseEvent | null;
  /** True when the stream ended without its single terminal event. */
  incomplete: boolean;
}

export class EventStream {
  private nextSequence = 0;
  private sawStarted = false;
  private terminalEvent: RepoiseEvent | null = null;
  private warnings: string[] = [];
  private lastProgress: RepoiseEvent | null = null;

  feed(line: string): StreamOutcome {
    const trimmed = line.trim();
    if (trimmed.length === 0) {
      return { kind: "malformed-line", problem: "empty-line" };
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      return { kind: "malformed-line", problem: "invalid-json" };
    }
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      Array.isArray(parsed)
    ) {
      return { kind: "malformed-line", problem: "not-object" };
    }
    const raw = parsed as Record<string, unknown>;
    const version = raw.schemaVersion;
    if (
      typeof version !== "number" ||
      !SUPPORTED_EVENT_STREAM_VERSIONS.includes(version)
    ) {
      return { kind: "unsupported-version", version };
    }
    if (typeof raw.runId !== "string" || raw.runId.length === 0) {
      return { kind: "malformed-line", problem: "missing-runId" };
    }
    if (
      typeof raw.sequence !== "number" ||
      !Number.isInteger(raw.sequence) ||
      raw.sequence < this.nextSequence
    ) {
      return { kind: "malformed-line", problem: "sequence-regression" };
    }
    if (typeof raw.timestamp !== "string" || raw.timestamp.length === 0) {
      return { kind: "malformed-line", problem: "missing-timestamp" };
    }
    const type = raw.type;
    if (typeof type !== "string" || type.length === 0) {
      return { kind: "malformed-line", problem: "missing-type" };
    }
    if (typeof raw.payload !== "object" || raw.payload === null) {
      return { kind: "malformed-line", problem: "missing-payload" };
    }
    const event: RepoiseEvent = {
      schemaVersion: version,
      runId: raw.runId,
      sequence: raw.sequence,
      timestamp: raw.timestamp,
      type,
      payload: raw.payload as Record<string, unknown>,
    };
    this.nextSequence = event.sequence + 1;

    if (TERMINAL_TYPES.has(type)) {
      if (this.terminalEvent !== null) {
        return { kind: "duplicate-terminal", type };
      }
      this.terminalEvent = event;
      return { kind: "event", event };
    }
    if (this.terminalEvent !== null) {
      return { kind: "after-terminal", type };
    }
    if (type === "started") {
      if (this.sawStarted) {
        return { kind: "malformed-line", problem: "second-started" };
      }
      this.sawStarted = true;
    } else if (!KNOWN_TYPES.has(type)) {
      // Unknown event types are ignored (additive compatibility policy).
      return { kind: "ignored-unknown-type", type };
    }
    if (type === "warning" && typeof event.payload.message === "string") {
      this.warnings.push(event.payload.message);
    }
    if (type === "progress") {
      this.lastProgress = event;
    }
    return { kind: "event", event };
  }

  summary(): StreamSummary {
    return {
      started: this.sawStarted,
      terminal:
        this.terminalEvent !== null
          ? (this.terminalEvent.type as "completed" | "cancelled" | "failed")
          : null,
      terminalEvent: this.terminalEvent,
      warnings: [...this.warnings],
      lastProgress: this.lastProgress,
      incomplete: this.terminalEvent === null,
    };
  }
}
