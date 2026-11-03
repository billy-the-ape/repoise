import assert from "node:assert/strict";
import { test } from "node:test";
import { runCli } from "../core/run";
import {
  describeStatusFailure,
  parseStatusView,
  statusExitUsable,
} from "../core/status";
import { FAKE } from "./helpers";

function fakeStatusJson(): string {
  return JSON.stringify({
    version: "0.1.0-fake",
    config: { file: null, problems: [] },
    scope: {
      adapter: "git",
      root: "/fake/repo",
      mode: "git-working-tree",
      repo_id: "repo-1",
      worktree_id: "wt-1",
      remote_identity: null,
    },
    snapshot: {
      snapshot_id: "snap-1",
      revision: "git:abc1234",
      branch: "main",
      mode: "git-working-tree",
      manifest_hash: "m1",
      dirty_digest: null,
      dirty_count: 2,
    },
    index: {
      generation_id: 7,
      schema_version: 1,
      files: 40,
      chunks: 120,
      built_at_ms: 1,
      manifest_hash: "m0",
      config_fingerprint: "c",
      parser_fingerprint: "p",
      vectors: 30,
      vector_profile: "v",
      embedding_scope: "docs",
      eligible_chunks: 30,
      ineligible_chunks: 90,
      vectors_pending: 5,
      embedding_coverage: "partial",
    },
    history_lane: null,
    freshness: {
      status: "fresh",
      reasons: [],
      snapshot_id: "snap-1",
      branch: "main",
      dirty_count: 2,
    },
    cache: { root: "/c", db_path: "/c/db", db_bytes: 1, wal_bytes: 0 },
    owner: {
      pid: 42,
      operation: "watch",
      started_at_ms: 1,
      refreshed_at_ms: 2,
      state: "active",
    },
    writer: { lock_held: true, state: "owned" },
    sharing: { shared: false, reason: null },
    last_error: null,
  });
}

test("parses the full status view with snake_case keys", () => {
  const parsed = parseStatusView("/fake/repo", fakeStatusJson());
  assert.equal(parsed.ok, true);
  if (!parsed.ok) return;
  const status = parsed.status;
  assert.equal(status.requestedRoot, "/fake/repo");
  assert.equal(status.scope.repoId, "repo-1");
  assert.equal(status.scope.worktreeId, "wt-1");
  assert.equal(status.scope.snapshotMode, "git-working-tree");
  assert.equal(status.snapshot?.branch, "main");
  assert.equal(status.snapshot?.dirtyCount, 2);
  assert.equal(status.index?.generationId, 7);
  assert.equal(status.index?.vectorsPending, 5);
  assert.equal(status.index?.embeddingCoverage, "partial");
  assert.equal(status.freshness?.status, "fresh");
  assert.equal(status.writer?.state, "owned");
  assert.equal(status.writer?.owner?.operation, "watch");
  assert.equal(status.lastError, null);
});

test("tolerates missing index, owner and freshness sections", () => {
  const view = JSON.parse(fakeStatusJson()) as Record<string, unknown>;
  view.index = null;
  view.owner = null;
  view.freshness = null;
  view.writer = { lock_held: false, state: "free" };
  view.last_error = "provider timeout";
  const parsed = parseStatusView("/r", JSON.stringify(view));
  assert.equal(parsed.ok, true);
  if (!parsed.ok) return;
  assert.equal(parsed.status.index, null);
  assert.equal(parsed.status.writer?.state, "free");
  assert.equal(parsed.status.writer?.owner, null);
  assert.equal(parsed.status.freshness, null);
  assert.equal(parsed.status.lastError, "provider timeout");
});

test("rejects documents without a usable scope", () => {
  assert.equal(parseStatusView("/r", "not json").ok, false);
  assert.equal(parseStatusView("/r", "[]").ok, false);
  const parsed = parseStatusView("/r", JSON.stringify({ writer: {} }));
  assert.equal(parsed.ok, false);
  if (!parsed.ok) {
    assert.equal(parsed.problem, "missing-scope");
  }
});

test("exit codes 0 and 3 carry usable status; others are failures", () => {
  assert.equal(statusExitUsable(0), true);
  assert.equal(statusExitUsable(3), true);
  assert.equal(statusExitUsable(1), false);
  assert.equal(statusExitUsable(2), false);
  assert.equal(statusExitUsable(null), false);
  assert.ok(describeStatusFailure(2, "bad flag").includes("usage error"));
  assert.ok(describeStatusFailure(null, "").includes("abnormally"));
  assert.ok(describeStatusFailure(1, "  disk\nfull  ").includes("disk full"));
});

test("the fake CLI status output round-trips through the stream runner", async () => {
  const lines: string[] = [];
  const handle = runCli({
    binary: process.execPath,
    args: [FAKE, "status", "--json", "/fake/repo"],
    env: { ...process.env, FAKE_STATUS_EXIT: "3" },
    onStdoutLine: (line) => lines.push(line),
  });
  const result = await handle.promise;
  // Exit 3 = index missing/stale: the document is still parseable.
  assert.equal(result.exitCode, 3);
  assert.equal(statusExitUsable(result.exitCode), true);
  const parsed = parseStatusView("/fake/repo", lines.join("\n"));
  assert.equal(parsed.ok, true);
  if (parsed.ok) {
    assert.equal(parsed.status.scope.root, "/fake/repo");
  }
});

test("garbage status output is a parse error, not a crash", async () => {
  const lines: string[] = [];
  const handle = runCli({
    binary: process.execPath,
    args: [FAKE, "status", "--json", "/fake/repo"],
    env: { ...process.env, FAKE_STATUS_BAD: "1" },
    onStdoutLine: (line) => lines.push(line),
  });
  const result = await handle.promise;
  assert.equal(result.exitCode, 0);
  const parsed = parseStatusView("/fake/repo", lines.join("\n"));
  assert.equal(parsed.ok, false);
  if (!parsed.ok) {
    assert.equal(parsed.problem, "invalid-json");
  }
});
