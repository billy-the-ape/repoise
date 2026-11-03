import type * as vscode from "vscode";
import { EventStream } from "../core/events";
import { RepoiseEvent } from "../core/types";
import { runCli, CliRunHandle } from "../core/run";
import { runStatus, StatusOutcome } from "../core/status";
import {
  WatchInfo,
  WatchDecision,
  decideWatchNextExit,
  initialWatchInfo,
} from "../core/watchPolicy";
import { isCliSession } from "../core/cliSession";
import type { CliManager } from "./cliManager";

export interface WatchCallbacks {
  onEvent: (root: string, event: RepoiseEvent) => void;
  onStateChange: (root: string) => void;
}

/**
 * Minimal logging surface (structurally satisfied by
 * `vscode.OutputChannel`) so the supervisor stays importable without the
 * vscode module.
 */
export interface WatchLogSink {
  appendLine(line: string): void;
}

/** Test hooks; production callers leave these unset. */
export interface WatchManagerHooks {
  /** Replaces the `status --json` ownership probe. */
  probeStatus?: (binary: string, root: string) => Promise<StatusOutcome>;
  /** Replaces the CLI process runner used for the watch process. */
  runProcess?: typeof runCli;
}

/**
 * Extension-owned watch supervisor, one process per workspace root.
 *
 * Ownership rules (docs/automation.md writer section):
 *  - before starting, the authoritative scope lock is probed via
 *    `status --json`; if another writer owns the scope (state `owned` or
 *    `busy`) the extension attaches read-only (status display) and never
 *    spawns a competing writer;
 *  - restarts after abnormal exit are bounded (3) with fixed backoff;
 *  - a `scope-owned` failure stops restarting entirely (external owner);
 *  - stopping sends SIGTERM (cooperative cancel) — the CLI preserves the
 *    last published generation and releases the lock;
 *  - owned watches are cancelled when their root leaves the window or the
 *    extension deactivates. PID data from owner markers is never used to
 *    kill or adopt processes.
 */
export class WatchManager implements vscode.Disposable {
  private infos = new Map<string, WatchInfo>();
  private runs = new Map<string, CliRunHandle>();
  private timers = new Map<string, NodeJS.Timeout>();
  /**
   * Roots with an in-flight `start()` probe; the symbol identifies the
   * owning start, so a stale continuation can never delete a newer claim.
   */
  private inflight = new Map<string, symbol>();
  /** Per-root invalidation counter; bumping it voids probes in flight. */
  private epochs = new Map<string, number>();
  private stopping = false;

  constructor(
    private readonly cli: CliManager,
    private readonly output: WatchLogSink,
    private readonly callbacks: WatchCallbacks,
    private readonly hooks: WatchManagerHooks = {},
  ) {}

  private epochOf(root: string): number {
    return this.epochs.get(root) ?? 0;
  }

  info(root: string): WatchInfo {
    let info = this.infos.get(root);
    if (info === undefined) {
      info = initialWatchInfo();
      this.infos.set(root, info);
    }
    return info;
  }

  isRunning(root: string): boolean {
    return this.runs.has(root);
  }

  /** True while a bounded restart backoff is pending for the root. */
  timerPending(root: string): boolean {
    return this.timers.has(root);
  }

  async start(root: string): Promise<void> {
    if (
      this.runs.has(root) ||
      this.timers.has(root) ||
      this.inflight.has(root)
    ) {
      return;
    }
    // Claim the root synchronously with a unique token: concurrent starts
    // (auto-start plus the webview ready message, double-clicks) must not
    // all probe the scope lock and all spawn.
    const claim = Symbol("watch-start");
    this.inflight.set(root, claim);
    const epoch = this.epochOf(root);
    try {
      const session = await this.cli.resolve();
      if (!isCliSession(session)) {
        this.setOutcome(
          root,
          "stopped",
          `cannot start watch: ${session.message}`,
        );
        return;
      }
      const status = await (this.hooks.probeStatus ?? runStatus)(
        session.binary,
        root,
      );
      if (status.ok) {
        const state = status.status.writer?.state;
        if (state === "owned" || state === "busy") {
          const owner = status.status.writer?.owner;
          this.setOutcome(
            root,
            "external",
            `scope is owned by ${
              owner ? `${owner.operation} (pid ${owner.pid})` : "another writer"
            }; displaying its status, not starting a competing writer`,
          );
          return;
        }
      } else {
        this.setOutcome(
          root,
          "stopped",
          `cannot verify scope ownership: ${status.detail}`,
        );
        return;
      }
      if (this.epochOf(root) !== epoch) {
        // A stop() landed while the probe was in flight; the spawn for this
        // root was invalidated.
        return;
      }
      this.spawn(root, session.binary, 1);
    } finally {
      // Release only our own claim: a stop() may have let a newer start
      // take over this root while our probe was in flight.
      if (this.inflight.get(root) === claim) {
        this.inflight.delete(root);
      }
    }
  }

  private spawn(root: string, binary: string, attempt: number): void {
    // A concurrent start may have won the root while we were settling the
    // probe; never run two watches for one root.
    if (this.runs.has(root) || this.stopping) {
      return;
    }
    const info = this.info(root);
    info.status = "starting";
    info.attempts = attempt;
    info.nextRestartAtMs = null;
    this.callbacks.onStateChange(root);

    const stream = new EventStream();
    const handle = (this.hooks.runProcess ?? runCli)({
      binary,
      args: ["watch", "--events", root],
      onStdoutLine: (line) => {
        const outcome = stream.feed(line);
        if (outcome.kind === "event") {
          const event = outcome.event;
          if (info.status === "starting") {
            info.status = "running";
            this.callbacks.onStateChange(root);
          }
          this.callbacks.onEvent(root, event);
        } else if (outcome.kind !== "ignored-unknown-type") {
          const problem =
            outcome.kind === "malformed-line"
              ? `malformed stream line (${outcome.problem})`
              : outcome.kind === "unsupported-version"
                ? `unsupported event-stream version ${String(outcome.version)}`
                : outcome.kind;
          this.output.appendLine(`watch stream issue for ${root}: ${problem}`);
        }
      },
    });
    this.runs.set(root, handle);
    info.pid = handle.pid;

    void handle.promise.then((result) => {
      // A different handle (or no handle) now owns this root: this run was
      // orphaned, and its settlement must not touch the shared state.
      if (this.runs.get(root) !== handle) {
        return;
      }
      this.runs.delete(root);
      info.pid = null;
      const summary = stream.summary();
      const terminalPayload = summary.terminalEvent?.payload ?? {};
      const terminalReason =
        typeof terminalPayload.reason === "string"
          ? terminalPayload.reason
          : null;
      const decision: WatchDecision = decideWatchNextExit({
        exitCode: result.exitCode,
        signal: result.signal,
        timedOut: result.timedOut,
        cancelledByUs: result.cancelledByUs,
        terminalType: summary.terminal,
        terminalReason,
        incomplete: summary.incomplete,
        attempts: attempt,
      });
      if (decision.action === "restart" && !this.stopping) {
        info.status = "waiting-restart";
        info.nextRestartAtMs = Date.now() + decision.delayMs;
        this.callbacks.onStateChange(root);
        this.output.appendLine(
          `watch for ${root} exiting abnormally (exit ${result.exitCode ?? "n/a"}); restarting in ${decision.delayMs}ms (attempt ${decision.attempt})`,
        );
        const timer = setTimeout(() => {
          this.timers.delete(root);
          if (!this.stopping) {
            this.spawn(root, binary, decision.attempt);
          }
        }, decision.delayMs);
        this.timers.set(root, timer);
      } else if (decision.action === "stop") {
        const label =
          decision.reason === "external-owner" ? "external" : "stopped";
        this.setOutcome(root, label, decision.detail);
        this.output.appendLine(`watch for ${root} stopped: ${decision.detail}`);
      }
    });
  }

  private setOutcome(
    root: string,
    status: WatchInfo["status"],
    outcome: string,
  ): void {
    const info = this.info(root);
    info.status = status;
    info.pid = null;
    info.nextRestartAtMs = null;
    info.lastOutcome = outcome;
    this.callbacks.onStateChange(root);
  }

  stop(root: string): void {
    // Invalidate an in-flight start() probe (if any) so it cannot spawn
    // after we've stopped.
    this.inflight.delete(root);
    this.epochs.set(root, this.epochOf(root) + 1);
    const timer = this.timers.get(root);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.timers.delete(root);
    }
    const handle = this.runs.get(root);
    if (handle !== undefined) {
      // The cooperative cancel yields a terminal `cancelled` event, which
      // drives the final status refresh through onCliEvent.
      handle.cancel();
      return;
    }
    // No live process (e.g. backoff in progress): clear the pending restart
    // so the state does not claim a countdown that will never fire.
    this.setOutcome(root, "stopped", "watch stopped by user");
  }

  /** Cancel every owned watch (window reload, deactivation, root disposal). */
  stopAll(): void {
    this.stopping = true;
    this.inflight.clear();
    for (const root of this.infos.keys()) {
      this.epochs.set(root, this.epochOf(root) + 1);
    }
    for (const [root, timer] of this.timers) {
      clearTimeout(timer);
      this.timers.delete(root);
      this.setOutcome(root, "stopped", "extension shutting down");
    }
    for (const handle of this.runs.values()) {
      handle.cancel();
    }
  }

  dispose(): void {
    this.stopAll();
    this.infos.clear();
    this.runs.clear();
    this.inflight.clear();
    this.epochs.clear();
  }
}
