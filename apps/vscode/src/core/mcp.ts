import { basename } from "node:path";

/**
 * stdio MCP server definition builder (`repoise mcp`).
 *
 * A stdio MCP client launches its own server process; we only describe it
 * (command + argument array). Read-only stays the default; refresh is added
 * only when explicitly enabled. Never embed credential values — the server
 * reads the same effective Repoise config as the CLI.
 */

export interface McpServerDefinition {
  name: string;
  command: string;
  args: string[];
  env?: Record<string, string>;
}

function hash8(input: string): string {
  let h = 5381;
  for (let i = 0; i < input.length; i++) {
    h = ((h << 5) + h + input.charCodeAt(i)) >>> 0;
  }
  return h.toString(36).padStart(4, "0").slice(0, 8);
}

function sanitizeNamePart(value: string): string {
  const cleaned = value
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-+|-+$/g, "");
  return cleaned.length > 0 ? cleaned.slice(0, 48) : "root";
}

/**
 * Unique, readable server name: the worktree scope id when known, else a
 * short hash of the root path (so two folders with the same basename never
 * collide).
 */
export function mcpDefinitionName(
  worktreeId: string | null | undefined,
  root: string,
): string {
  const part =
    worktreeId !== null && worktreeId !== undefined && worktreeId.length > 0
      ? worktreeId
      : `root-${hash8(root)}-${sanitizeNamePart(basename(root))}`;
  return `repoise-${sanitizeNamePart(part)}`;
}

export function buildMcpDefinition(
  binary: string,
  root: string,
  options: {
    refresh?: boolean;
    worktreeId?: string | null;
  } = {},
): McpServerDefinition {
  return {
    name: mcpDefinitionName(options.worktreeId ?? null, root),
    command: binary,
    args: ["mcp", ...(options.refresh ? ["--allow-refresh"] : []), root],
  };
}

/** Generic stdio-client JSON (mcpServers template), for copy/paste setup. */
export function mcpServersSnippet(definition: McpServerDefinition): string {
  const entry: Record<string, unknown> = {
    command: definition.command,
    args: definition.args,
  };
  if (definition.env !== undefined) {
    entry.env = definition.env;
  }
  return JSON.stringify({ mcpServers: { [definition.name]: entry } }, null, 2);
}
