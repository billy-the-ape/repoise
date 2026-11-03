/**
 * Bounded restart/backoff policy for the extension-owned watch.
 *
 * The extension is a supervisor, not a new daemon: it starts at most a
 * bounded number of retries with fixed backoff, and it never restarts into a
 * scope another writer owns (the AP-02 `scope-owned` failure reason) — it
 * switches to "external owner" and lets the other writer keep publishing.
 * PID data is descriptive only; ownership is decided by the CLI's
 * authoritative scope lock (see docs/automation.md).
 */

export const WATCH_BACKOFF_MS = [5_000, 15_000, 60_000] as const;
export const WATCH_MAX_RESTARTS = 3;

export interface WatchExitInfo {
  exitCode: number | null;
  signal: string | null;
  timedOut: boolean;
  cancelledByUs: boolean;
  terminalType: "completed" | "cancelled" | "failed" | null;
  /** Machine `reason` from a `failed` payload (e.g. `scope-owned`). */
  terminalReason: string | null;
  /** True when the stream ended without its terminal event (crash/kill). */
  incomplete: boolean;
  /** How many starts this supervisor has already attempted (1-based). */
  attempts: number;
}

export type WatchDecision =
  | { action: "restart"; delayMs: number; attempt: number }
  | {
      action: "stop";
      reason:
        | "cancelled"
        | "completed"
        | "external-owner"
        | "spawn-failure"
        | "max-restarts"
        | "unexpected-exit";
      detail: string;
    };

export function decideWatchNextExit(info: WatchExitInfo): WatchDecision {
  if (info.cancelledByUs || info.terminalType === "cancelled") {
    return { action: "stop", reason: "cancelled", detail: "cooperative stop" };
  }
  if (info.terminalType === "failed" && info.terminalReason === "scope-owned") {
    return {
      action: "stop",
      reason: "external-owner",
      detail:
        "another writer owns the scope lock; the extension will not start a competing writer",
    };
  }
  if (info.terminalType === "completed") {
    return {
      action: "stop",
      reason: "completed",
      detail: "watch reported an unexpected terminal completion",
    };
  }
  if (info.exitCode === null && info.signal === null && !info.timedOut) {
    return {
      action: "stop",
      reason: "spawn-failure",
      detail: "the watch process could not be started",
    };
  }
  if (info.attempts >= WATCH_MAX_RESTARTS + 1) {
    return {
      action: "stop",
      reason: "max-restarts",
      detail: `gave up after ${WATCH_MAX_RESTARTS} bounded restarts`,
    };
  }
  const delayIndex = Math.min(info.attempts - 1, WATCH_BACKOFF_MS.length - 1);
  return {
    action: "restart",
    delayMs: WATCH_BACKOFF_MS[delayIndex],
    attempt: info.attempts + 1,
  };
}

export type WatchUiStatus =
  "idle" | "starting" | "running" | "waiting-restart" | "stopped" | "external";

export interface WatchInfo {
  status: WatchUiStatus;
  pid: number | null;
  attempts: number;
  nextRestartAtMs: number | null;
  lastOutcome: string | null;
  /** True once the bounded restart budget is exhausted (reset by a new run). */
  exhausted: boolean;
}

export function initialWatchInfo(): WatchInfo {
  return {
    status: "idle",
    pid: null,
    attempts: 0,
    nextRestartAtMs: null,
    lastOutcome: null,
    exhausted: false,
  };
}
