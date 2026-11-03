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
const MCP_PROVIDER_ID = "repoise.stdio";

export function registerMcpProvider(
  context: vscode.ExtensionContext,
  cli: CliManager,
): vscode.EventEmitter<void> | null {
  // The `lm` namespace only exists on VS Code 1.103+; older hosts activate
  // the extension (for the commands) but must not crash on it.
  const lmApi = (
    vscode as unknown as {
      lm?: {
        registerMcpServerDefinitionProvider?: (
          id: string,
          provider: {
            provideMcpServerDefinitions: () =>
              | Promise<McpStdioServerDefinitionLike[]>
              | McpStdioServerDefinitionLike[];
            onDidChangeMcpServerDefinitions?: vscode.Event<void>;
          },
        ) => vscode.Disposable;
      };
    }
  ).lm;
  if (typeof lmApi?.registerMcpServerDefinitionProvider !== "function") {
    return null;
  }

  interface McpStdioServerDefinitionLike {
    label: string;
    name: string;
    command: string;
    args: string[];
    env?: Record<string, string>;
  }

  // VS Code caches the provider's result; this event tells it to re-query
  // when trust, settings or workspace folders change (see extension.ts).
  const changeEmitter = new vscode.EventEmitter<void>();
  context.subscriptions.push(changeEmitter);

  const provider = {
    provideMcpServerDefinitions: async (): Promise<
      McpStdioServerDefinitionLike[]
    > => {
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
      const definitions: McpStdioServerDefinitionLike[] = [];
      for (const folder of vscode.workspace.workspaceFolders ?? []) {
        const definition = buildMcpDefinition(
          session.binary,
          folder.uri.fsPath,
          {
            refresh,
            worktreeId: cli.worktreeIdFor(folder.uri.fsPath),
          },
        );
        definitions.push({
          label: `Repoise (${definition.name})`,
          name: definition.name,
          command: definition.command,
          args: definition.args,
          ...(definition.env !== undefined ? { env: definition.env } : {}),
        });
      }
      return definitions;
    },
    onDidChangeMcpServerDefinitions: changeEmitter.event,
  };
  // The provider id is declared under contributes.mcpServerDefinitionProviders
  // in package.json; registration is still guarded because hosts may expose
  // the API without the contribution being recognized.
  try {
    const disposable = lmApi.registerMcpServerDefinitionProvider(
      MCP_PROVIDER_ID,
      provider,
    );
    context.subscriptions.push(disposable);
    return changeEmitter;
  } catch {
    return null;
  }
}
