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

  // VS Code caches the MCP provider's definitions; the emitter (returned
  // when the host supports 1.103+ MCP discovery) tells it to re-query when
  // trust, settings or workspace folders change.
  const mcpNotify = registerMcpProvider(context, cli);

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
          // Opens VS Code's trust management dialog.
          await vscode.commands.executeCommand("workbench.trust.manage");
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
      // The per-folder MCP definitions changed with the folder list.
      mcpNotify?.fire();
    }),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("repoise")) {
        cli.invalidate();
        // The cached CLI session is gone; the next provider query
        // re-resolves it, so ask VS Code to re-query the definitions.
        mcpNotify?.fire();
      }
    }),
    vscode.workspace.onDidGrantWorkspaceTrust(() => {
      // The provider returned [] while untrusted; re-query now that
      // repository-local CLI resolution is allowed again.
      mcpNotify?.fire();
    }),
  );
  context.subscriptions.push(panel, watch, cli);
}

export function deactivate(): void {
  // WatchManager.dispose (via context.subscriptions) cancels owned watches.
}
