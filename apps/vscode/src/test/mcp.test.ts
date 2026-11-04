import assert from "node:assert/strict";
import { test } from "node:test";
import {
  buildMcpDefinition,
  mcpDefinitionName,
  mcpServersSnippet,
} from "../core/mcp";

test("read-only definition by default", () => {
  const definition = buildMcpDefinition("/opt/bin/repoise", "/fake/repo", {
    worktreeId: "wt-1",
  });
  assert.equal(definition.name, "repoise-wt-1");
  assert.equal(definition.command, "/opt/bin/repoise");
  assert.deepEqual(definition.args, ["mcp", "/fake/repo"]);
  assert.equal(definition.env, undefined);
});

test("refresh is added only when explicitly enabled", () => {
  const definition = buildMcpDefinition("/opt/bin/repoise", "/fake/repo", {
    refresh: true,
    worktreeId: "wt-1",
  });
  assert.deepEqual(definition.args, ["mcp", "--allow-refresh", "/fake/repo"]);
});

test("worktree ids are sanitized into safe names", () => {
  assert.equal(mcpDefinitionName("wt-1", "/r"), "repoise-wt-1");
  assert.equal(mcpDefinitionName("WT/2", "/r"), "repoise-wt-2");
  assert.equal(mcpDefinitionName("", "/r"), mcpDefinitionName(null, "/r"));
});

test("roots without a known worktree id still get unique names", () => {
  const a = mcpDefinitionName(null, "/repo");
  const b = mcpDefinitionName(null, "/other/repo");
  assert.notEqual(a, b);
  assert.ok(a.startsWith("repoise-root-"));
});

test("the generic stdio snippet preserves the entry shape", () => {
  const definition = buildMcpDefinition("/opt/bin/repoise", "/fake/repo", {
    worktreeId: "wt-1",
  });
  const parsed = JSON.parse(mcpServersSnippet(definition));
  assert.deepEqual(Object.keys(parsed.mcpServers), ["repoise-wt-1"]);
  assert.deepEqual(parsed.mcpServers["repoise-wt-1"], {
    command: "/opt/bin/repoise",
    args: ["mcp", "/fake/repo"],
  });
});
