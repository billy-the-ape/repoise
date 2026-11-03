import * as vscode from "vscode";
import { buildMcpDefinition } from "../core/mcp";
import { CliManager, isCliSession } from "./cliManager";

/**
 * Editor MCP discovery via `vscode.lm.registerMcpServerDefinitionProvider`
 * (VS Code 1.103+). One read-only stdio server definition per workspace
 * folder; refresh is included only when explicitly enabled.
 *
 * Registering with VS Code does not configure every installed agent —
 * Cline, Codex and others keep their own MCP settings. The panel's
 * "Copy JSON" flow provides the generic stdio template for those clients.
 * On older VS Code versions the provider is skipped silently (the copy flow
 * still works); this extension never writes to client settings files.
 */
export function registerMcpProvider(
  context: vscode.ExtensionContext,
  cli: CliManager,
): vscode.Disposable | null {
  // The `lm` namespace only exists on VS Code 1.103+; older hosts activate
  // the extension (for the commands) but must not crash on it.
  const lmApi = (
    vscode as unknown as {
      lm?: {
        registerMcpServerDefinitionProvider?: (provider: {
          provideMcpServerDefinitions: () =>
            Promise<McpDefinitionLike[]> | McpDefinitionLike[];
        }) => vscode.Disposable;
      };
    }
  ).lm;
  if (typeof lmApi?.registerMcpServerDefinitionProvider !== "function") {
    return null;
  }

  interface McpDefinitionLike {
    name: string;
    command: string;
    args: string[];
    env?: Record<string, string>;
  }

  const provider = {
    provideMcpServerDefinitions: async (): Promise<McpDefinitionLike[]> => {
      if (!vscode.workspace.isTrusted) {
        return [];
      }
      const session = await cli.resolve();
      if (!isCliSession(session)) {
        return [];
      }
      const refresh =
        vscode.workspace
          .getConfiguration("repoise")
          .get<boolean>("mcp.refresh") ?? false;
      const definitions: McpDefinitionLike[] = [];
      for (const folder of vscode.workspace.workspaceFolders ?? []) {
        definitions.push(
          buildMcpDefinition(session.binary, folder.uri.fsPath, {
            refresh,
            worktreeId: cli.worktreeIdFor(folder.uri.fsPath),
          }),
        );
      }
      return definitions;
    },
  };
  const disposable = lmApi.registerMcpServerDefinitionProvider(provider);
  context.subscriptions.push(disposable);
  return disposable;
}
