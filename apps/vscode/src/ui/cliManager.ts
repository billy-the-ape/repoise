import * as vscode from "vscode";
import {
  CapabilitiesDoc,
  checkCapabilities,
  parseCapabilities,
} from "../core/capabilities";
import { CliSource, discoverCli } from "../core/discovery";
import { runCli } from "../core/run";
import { log, truncate } from "./output";

/**
 * Resolves and caches the installed Repoise CLI plus its capabilities
 * handshake result. Discovery runs in the extension host, so on remote
 * workspaces it resolves the binary where the workspace filesystem lives.
 */

export interface CliSession {
  binary: string;
  source: CliSource;
  version: string;
  compatible: boolean;
  problems: string[];
  searched: string[];
  capabilities: CapabilitiesDoc;
}

export type CliFailure =
  | { problem: "not-found"; message: string }
  | { problem: "override-not-found"; message: string }
  | { problem: "capabilities-failed"; message: string }
  | { problem: "incompatible"; message: string };

export function isCliSession(
  value: CliSession | CliFailure,
): value is CliSession {
  return "binary" in value;
}

export class CliManager implements vscode.Disposable {
  private cached: CliSession | CliFailure | null = null;
  private worktreeIds = new Map<string, string>();

  constructor(private readonly output: vscode.OutputChannel) {}

  resolve(force = false): Promise<CliSession | CliFailure> {
    if (!force && this.cached !== null) {
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
    const override = config.get<string>("cliPath") ?? "";
    const workspaceRoots = (vscode.workspace.workspaceFolders ?? []).map(
      (folder) => folder.uri.fsPath,
    );
    const discovery = discoverCli({
      override: override.trim(),
      workspaceRoots,
    });
    if (!discovery.ok || discovery.path === undefined) {
      const searched = discovery.searched.slice(-5).join(", ");
      const message =
        discovery.problem === "override-not-found"
          ? `repoise.cliPath points at ${override.trim()}, which is not a file. Fix the setting or clear it to fall back to local/PATH discovery.`
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
