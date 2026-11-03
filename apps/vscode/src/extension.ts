import * as vscode from "vscode";
import { CliManager } from "./ui/cliManager";
import { StatusPanel } from "./ui/statusPanel";
import { WatchManager } from "./ui/watchManager";
import { registerMcpProvider } from "./ui/mcpProvider";
import { createOutputChannel } from "./ui/output";

/**
 * AP-07 first slice: thin VS Code operations UI over the installed Repoise
 * CLI. No indexing, retrieval, benchmark or agent logic lives here — every
 * operation shells out to the native CLI with argument arrays and renders the
 * AP-02 machine contracts (docs/automation.md, docs/mcp.md, docs/vscode.md).
 */
export function activate(context: vscode.ExtensionContext): void {
  const output = createOutputChannel(context);
  const cli = new CliManager(output);
  const watch = new WatchManager(cli, output, {
    onEvent: (root, event) => panel.onCliEvent(root, event),
    onStateChange: (root) => panel.onWatchState(root),
  });
  const panel = new StatusPanel(cli, watch, output);

  // Repository-local binaries and repository content only run when the
  // workspace is trusted; opening an untrusted checkout never launches them.
  const guard =
    (fn: (...args: unknown[]) => unknown) =>
    async (...args: unknown[]): Promise<void> => {
      if (!vscode.workspace.isTrusted) {
        const trust = await vscode.window.showWarningMessage(
          "Repoise needs a trusted workspace before it can run the repository-local CLI.",
          "Trust workspace",
          "Dismiss",
        );
        if (trust === "Trust workspace") {
          await vscode.commands.executeCommand("workbench.trust");
        }
        return;
      }
      void fn(...args);
    };

  context.subscriptions.push(
    vscode.commands.registerCommand(
      "repoise.showStatus",
      guard(async () => {
        await panel.reveal();
      }),
    ),
    vscode.commands.registerCommand(
      "repoise.index",
      guard(async () => {
        await panel.reveal();
        await panel.triggerIndex();
      }),
    ),
    vscode.commands.registerCommand(
      "repoise.watchStart",
      guard(async () => {
        await panel.reveal();
        await panel.triggerWatchStart();
      }),
    ),
    vscode.commands.registerCommand(
      "repoise.watchStop",
      guard(async () => {
        await panel.reveal();
        await panel.triggerWatchStop();
      }),
    ),
    vscode.commands.registerCommand(
      "repoise.mcpShow",
      guard(async () => {
        await panel.reveal();
      }),
    ),
    vscode.workspace.onDidChangeWorkspaceFolders((event) => {
      // Owned watches of removed roots are cancelled; their scope locks are
      // released by the OS when the process exits.
      for (const folder of event.removed) {
        watch.stop(folder.uri.fsPath);
      }
      void panel.syncRoots();
    }),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("repoise")) {
        cli.invalidate();
      }
    }),
  );
  const mcpDisposable = registerMcpProvider(context, cli);
  if (mcpDisposable !== null) {
    context.subscriptions.push(mcpDisposable);
  }
  context.subscriptions.push(panel, watch);
}

export function deactivate(): void {
  // WatchManager.dispose (via context.subscriptions) cancels owned watches.
}
