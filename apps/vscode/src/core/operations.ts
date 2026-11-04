import { EventStream, StreamSummary } from "./events";
import { RepoiseEvent, asTerminal } from "./types";
import { runCli } from "./run";

/**
 * One-shot streaming operations (`index --events`). The progress numbers are
 * taken verbatim from the AP-02 event stream — the extension never guesses a
 * percentage from log text.
 */

export interface IndexSummary {
  terminal: "completed" | "cancelled" | "failed" | null;
  exitCode: number | null;
  failedMessage: string | null;
  failedReason: string | null;
  warnings: string[];
  scopeRoot: string | null;
  incomplete: boolean;
  stderr: string;
}

export async function runIndex(
  binary: string,
  root: string,
  onEvent: (event: RepoiseEvent) => void,
): Promise<IndexSummary> {
  const stream = new EventStream();
  let scopeRoot: string | null = null;
  const handle = runCli({
    binary,
    args: ["index", "--events", root],
    onStdoutLine: (line) => {
      const outcome = stream.feed(line);
      if (outcome.kind === "event") {
        if (outcome.event.type === "started") {
          const scope = outcome.event.payload.scope as
            { root?: string } | undefined;
          scopeRoot = scope?.root ?? null;
        }
        onEvent(outcome.event);
      }
    },
  });
  const result = await handle.promise;
  const summary: StreamSummary = stream.summary();
  let failedMessage: string | null = null;
  let failedReason: string | null = null;
  if (summary.terminalEvent !== null) {
    const terminal = asTerminal(summary.terminalEvent.payload);
    failedMessage = terminal.message ?? null;
    failedReason = terminal.reason ?? null;
  }
  return {
    terminal: summary.terminal,
    exitCode: result.exitCode,
    failedMessage,
    failedReason,
    warnings: summary.warnings,
    scopeRoot,
    incomplete: summary.incomplete,
    stderr: result.stderr.trim(),
  };
}
