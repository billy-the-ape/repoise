import * as vscode from "vscode";
import { checkCapabilities, parseCapabilities } from "../core/capabilities";
import { discoverCli } from "../core/discovery";
import { runCli } from "../core/run";
import { CliSession, CliFailure, isCliSession } from "../core/cliSession";
import { log, truncate } from "./output";

export { isCliSession };

export class CliManager implements vscode.Disposable {
  private cached: CliSession | CliFailure | null = null;
  private worktreeIds = new Map<string, string>();

  constructor(private readonly output: vscode.OutputChannel) {}

  resolve(force = false): Promise<CliSession | CliFailure> {
    // A cached failure is always re-probed: after installing the CLI or
    // fixing PATH, the next action (refresh, index, watch start, MCP)
    // recovers without a window reload or a settings change.
    if (!force && this.cached !== null && isCliSession(this.cached)) {
      return Promise.resolve(this.cached);
    }
    return this.refresh();
  }

  /** Last resolved session/failure without running a new handshake. */
  current(): CliSession | CliFailure | null {
    return this.cached;
  }

  async refresh(): Promise<CliSession | CliFailure> {
    const config = vscode.workspace.getConfiguration("repoise");
    // Repository-local binaries only resolve in a trusted workspace; in
    // Restricted Mode discovery falls back to PATH (no override path, no
    // per-folder node_modules probe).
    const trusted = vscode.workspace.isTrusted;
    const override = trusted
      ? (config.get<string>("cliPath") ?? "").trim()
      : "";
    const workspaceRoots = trusted
      ? (vscode.workspace.workspaceFolders ?? []).map(
          (folder) => folder.uri.fsPath,
        )
      : [];
    const discovery = discoverCli({
      override,
      workspaceRoots,
    });
    if (!discovery.ok || discovery.path === undefined) {
      const searched = discovery.searched.slice(-5).join(", ");
      const message =
        discovery.problem === "override-not-found"
          ? `repoise.cliPath points at ${override}, which is not a file. Fix the setting or clear it to fall back to local/PATH discovery.`
          : `No repoise binary found (looked at: ${searched}). Install it (npm package, native archive or cargo install) or set repoise.cliPath. The extension never downloads a binary itself.`;
      const failure: CliFailure = {
        problem: discovery.problem ?? "not-found",
        message,
      };
      this.cached = failure;
      return failure;
    }
    const lines: string[] = [];
    const handle = runCli({
      binary: discovery.path,
      args: ["capabilities", "--json"],
      onStdoutLine: (line) => lines.push(line),
    });
    const result = await handle.promise;
    if (result.exitCode !== 0) {
      const failure: CliFailure = {
        problem: "capabilities-failed",
        message: `repoise capabilities exited ${result.exitCode ?? "abnormally"}${
          result.spawnError ? ` (${result.spawnError})` : ""
        }${result.stderr ? `: ${truncate(result.stderr)}` : ""}`,
      };
      this.cached = failure;
      return failure;
    }
    const parsed = parseCapabilities(lines.join("\n"));
    if (!parsed.ok) {
      const failure: CliFailure = {
        problem: "capabilities-failed",
        message: `Unparseable capabilities output (${parsed.problem}); the CLI may be too old or broken.`,
      };
      this.cached = failure;
      return failure;
    }
    const check = checkCapabilities(parsed.doc);
    const session: CliSession = {
      binary: discovery.path,
      source: discovery.source ?? "path",
      version: parsed.doc.version,
      compatible: check.compatible,
      problems: check.problems,
      searched: discovery.searched,
      capabilities: parsed.doc,
    };
    this.cached = session;
    log(
      this.output,
      `Using Repoise CLI v${session.version} (${session.source}) at ${session.binary}`,
    );
    for (const problem of session.problems) {
      log(this.output, `capability note: ${problem}`);
    }
    return session;
  }

  /** Cached worktree scope ids learned from recent status refreshes. */
  setWorktreeId(root: string, worktreeId: string | null): void {
    if (worktreeId === null || worktreeId.length === 0) {
      this.worktreeIds.delete(root);
      return;
    }
    this.worktreeIds.set(root, worktreeId);
  }

  worktreeIdFor(root: string): string | null {
    return this.worktreeIds.get(root) ?? null;
  }

  invalidate(): void {
    this.cached = null;
  }

  dispose(): void {
    this.cached = null;
    this.worktreeIds.clear();
  }
}
