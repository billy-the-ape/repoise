import {
  RepoiseEvent,
  asProgress,
  asStarted,
  asTerminal,
  asWarning,
} from "./types";

/**
 * Human-readable rendering of AP-02 stream events for the operations panel
 * and output channel. Returns null for events that need no visible line
 * (heartbeats). Only stream-provided numbers are rendered.
 */

function formatElapsed(ms: number): string {
  if (ms < 1000) {
    return `${ms}ms`;
  }
  const seconds = ms / 1000;
  if (seconds < 60) {
    return `${seconds.toFixed(1)}s`;
  }
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${Math.round(seconds % 60)}s`;
}

export function describeEvent(event: RepoiseEvent): string | null {
  const p = event.payload;
  switch (event.type) {
    case "started": {
      const started = asStarted(p);
      const scope = started.scope;
      const scopeText = scope?.root ? ` scope ${scope.root}` : "";
      return `started ${started.operation ?? "run"} (run ${event.runId}${scopeText})`;
    }
    case "progress": {
      const progress = asProgress(p);
      const parts: string[] = [];
      if (progress.phase !== undefined) {
        parts.push(`phase ${progress.phase}`);
      }
      if (progress.done !== undefined) {
        const total =
          progress.total !== null && progress.total !== undefined
            ? `/${progress.total}`
            : "";
        parts.push(
          `${progress.done}${total}${progress.unit ? ` ${progress.unit}` : ""}`,
        );
      }
      if (progress.elapsedMs !== undefined) {
        parts.push(formatElapsed(progress.elapsedMs));
      }
      if (progress.etaSeconds !== null && progress.etaSeconds !== undefined) {
        parts.push(`ETA ~${progress.etaSeconds}s`);
      }
      const notes: string[] = [];
      if (progress.generationId !== undefined) {
        notes.push(`generation ${progress.generationId}`);
      }
      if (progress.branchSwitch === true) {
        notes.push("branch switch detected");
      }
      if (progress.configChange === true) {
        notes.push("config change detected");
      }
      if (progress.changedPaths !== undefined) {
        notes.push(`${progress.changedPaths.length} changed path(s)`);
      }
      if (progress.vectorsTotal !== undefined) {
        notes.push(`${progress.vectorsTotal} vectors`);
      }
      if (progress.durationMs !== undefined) {
        notes.push(`took ${formatElapsed(progress.durationMs)}`);
      }
      if (notes.length > 0) {
        parts.push(notes.join(", "));
      }
      return parts.length > 0 ? parts.join(" · ") : "progress";
    }
    case "warning": {
      const warning = asWarning(p);
      const kind = warning.kind !== undefined ? ` [${warning.kind}]` : "";
      return `warning: ${warning.message ?? "unknown"}${kind}`;
    }
    case "heartbeat":
      return null;
    case "completed":
      return "run completed";
    case "cancelled": {
      const terminal = asTerminal(p);
      const reason = terminal.reason ? ` (reason: ${terminal.reason})` : "";
      return `run cancelled${reason}`;
    }
    case "failed": {
      const terminal = asTerminal(p);
      const message = terminal.message ?? "operational failure";
      const reason = terminal.reason ? ` [${terminal.reason}]` : "";
      return `run failed: ${message}${reason}`;
    }
    default:
      return `event ${event.type}`;
  }
}
