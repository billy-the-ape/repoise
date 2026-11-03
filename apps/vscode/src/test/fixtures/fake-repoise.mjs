#!/usr/bin/env node
// Deterministic fake Repoise CLI for the extension's offline unit tests.
// Never shipped; behavior selected by subcommand, `--fixture <name>` and
// FAKE_* environment variables. Protocol lines follow the AP-02 envelope.
//
// Signal handlers are armed before any stream output: the real CLI installs
// its SIGTERM/SIGINT handling at startup, and killing a child in the same
// instant its first line is observed must never race the handler setup.
import { setInterval } from "node:timers";

const argv = process.argv.slice(2);
const sub = argv[0];
const fixtureFlag = argv.indexOf("--fixture");
const fixture = fixtureFlag >= 0 ? argv[fixtureFlag + 1] : null;
const crlf = process.env.FAKE_REPOISE_CRLF === "1";

const out = (s) => process.stdout.write(s + (crlf ? "\r\n" : "\n"));

let sequenceCounter = 0;
function event(type, payload) {
  sequenceCounter += 1;
  return JSON.stringify({
    schemaVersion: 1,
    runId: `fake-${process.pid}-1`,
    sequence: sequenceCounter - 1,
    timestamp: "2026-11-03T00:00:00.000Z",
    type,
    payload,
  });
}

const scope = {
  root: "/fake/repo",
  repoId: "repo-1",
  worktreeId: "wt-1",
  snapshotMode: "git-working-tree",
};

let longInterval = null;
const cooperativeStop = () => {
  if (longInterval !== null) {
    clearInterval(longInterval);
  }
  out(event("cancelled", { scope, reason: "signal" }));
  process.exit(0);
};
const abruptStop = () => process.exit(143);
if (sub === "watch" && fixture === "long") {
  process.on("SIGTERM", cooperativeStop);
  process.on("SIGINT", cooperativeStop);
} else if (sub === "index" && fixture === "sleep") {
  process.on("SIGTERM", abruptStop);
  process.on("SIGINT", abruptStop);
}

if (sub === "capabilities") {
  const doc = {
    schemaVersion: 1,
    name: "repoise",
    version: "0.1.0",
    eventStream: {
      schema: "schemas/event-stream.v1.schema.json",
      version: 1,
      operations: ["index", "watch"],
      heartbeatIntervalMs: 15000,
    },
    features:
      fixture === "no-events"
        ? ["mcp-stdio", "exclusive-scope-lock"]
        : [
            "event-stream",
            "mcp-stdio",
            "exclusive-scope-lock",
            "remote-embedding",
            "github-enrichment",
          ],
    commands: [
      { name: "doctor", machine: ["json"] },
      { name: "status", machine: ["json"] },
      { name: "index", machine: ["json", "events"] },
      { name: "watch", machine: ["json", "events"] },
      { name: "mcp", machine: ["mcp-stdio"] },
    ],
    benchmark: { supported: true, command: "bench" },
    exitCodes: {
      ok: 0,
      operational: 1,
      usage: 2,
      indexMissingOrStale: 3,
      coverageNotMet: 4,
    },
    compatibility: {
      policy: "additive",
      supportedEventStreamVersions: fixture === "stream-v2" ? [2] : [1],
    },
  };
  process.stdout.write(JSON.stringify(doc) + "\n");
  process.exit(0);
}

if (sub === "status") {
  if (process.env.FAKE_STATUS_BAD === "1") {
    process.stdout.write("this is not a status document\n");
    process.exit(0);
  }
  const stale = fixture === "stale";
  const view = {
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
      manifest_hash: "manifest-1",
      dirty_digest: null,
      dirty_count: 2,
    },
    index: {
      generation_id: 7,
      schema_version: 1,
      files: 40,
      chunks: 120,
      built_at_ms: 1,
      manifest_hash: "manifest-0",
      config_fingerprint: "cfg",
      parser_fingerprint: "parser",
      vectors: 30,
      vector_profile: "profile",
      embedding_scope: "docs",
      eligible_chunks: 30,
      ineligible_chunks: 90,
      vectors_pending: 5,
      embedding_coverage: "partial",
    },
    history_lane: null,
    freshness: {
      status: stale ? "stale" : "fresh",
      reasons: stale ? ["snapshot changed since the last publication"] : [],
      snapshot_id: "snap-1",
      branch: "main",
      dirty_count: 2,
    },
    cache: {
      root: "/cache",
      db_path: "/cache/repos/repo-1/worktrees/wt-1/repo.db",
      db_bytes: 1000,
      wal_bytes: 0,
    },
    owner: stale
      ? null
      : {
          pid: 4242,
          operation: "watch",
          started_at_ms: 1000,
          refreshed_at_ms: 2000,
          state: "active",
        },
    writer: stale
      ? { lock_held: false, state: "free" }
      : { lock_held: true, state: "owned" },
    sharing: { shared: false, reason: null },
    last_error: null,
  };
  process.stdout.write(JSON.stringify(view, null, 2) + "\n");
  process.exit(Number(process.env.FAKE_STATUS_EXIT ?? 0));
}

if (sub === "index") {
  if (fixture === "sleep") {
    // Stays alive (busy interval) until the parent cancels it: timeout test.
    setInterval(() => undefined, 1000);
  } else if (fixture === "bad-version") {
    out(
      JSON.stringify({
        schemaVersion: 2,
        runId: "fake-1-1",
        sequence: 0,
        timestamp: "2026-11-03T00:00:00.000Z",
        type: "started",
        payload: { operation: "index", scope },
      }),
    );
    process.exit(0);
  } else {
    out(event("started", { operation: "index", pid: process.pid, scope }));
    if (fixture === "crash") {
      process.exit(137);
    }
    if (fixture === "scope-owned-fail") {
      out(
        event("failed", {
          scope,
          code: "operational",
          message: "another writer owns the scope",
          retryable: true,
          reason: "scope-owned",
        }),
      );
      process.exit(1);
    }
    if (fixture === "malformed") {
      out("this is not json");
      out(event("completed", { scope, result: { chunks: 1 } }));
      process.exit(0);
    }
    if (fixture === "unknown-type") {
      out(event("telemetry", { operation: "index", note: "ignored" }));
      out(
        event("progress", {
          operation: "index",
          phase: "discovery",
          unit: "files",
          done: 1,
          total: 2,
          elapsedMs: 5,
          etaSeconds: null,
        }),
      );
      out(event("completed", { scope, result: { chunks: 2 } }));
      process.exit(0);
    }
    out(
      event("progress", {
        operation: "index",
        phase: "discovery",
        unit: "files",
        done: 3,
        total: 10,
        elapsedMs: 40,
        etaSeconds: null,
      }),
    );
    out(
      event("progress", {
        operation: "index",
        phase: "discovery",
        unit: "files",
        done: 10,
        total: 10,
        elapsedMs: 60,
        etaSeconds: null,
      }),
    );
    out(
      event("warning", {
        operation: "index",
        message: "embedding provider unavailable; lexical only",
      }),
    );
    out(
      event("progress", {
        operation: "index",
        phase: "publication",
        unit: "chunks",
        done: 42,
        total: 42,
        elapsedMs: 80,
        etaSeconds: 1,
      }),
    );
    out(event("completed", { scope, result: { chunks: 42, vectors: 30 } }));
    process.exit(0);
  }
} else if (sub === "watch") {
  if (fixture === "long") {
    out(event("started", { operation: "watch", pid: process.pid, scope }));
    let heartbeat = 0;
    longInterval = setInterval(() => {
      heartbeat += 1;
      out(event("heartbeat", { operation: "watch", elapsedMs: heartbeat * 100 }));
    }, 100);
  } else {
    out(event("started", { operation: "watch", pid: process.pid, scope }));
    if (fixture === "scope-owned-fail") {
      out(
        event("failed", {
          scope,
          code: "operational",
          message: "another writer owns the scope",
          retryable: true,
          reason: "scope-owned",
        }),
      );
      process.exit(1);
    }
    if (fixture === "crash") {
      process.exit(139);
    }
    out(
      event("progress", {
        operation: "watch",
        generationId: 2,
        changedPaths: ["README.md"],
        filesReparsed: 1,
        forceFull: false,
        branchSwitch: false,
        configChange: false,
        vectorsTotal: 10,
        durationMs: 12,
      }),
    );
    out(event("completed", { scope, result: {} }));
    process.exit(0);
  }
} else if (sub !== "capabilities" && sub !== "status") {
  process.exit(2);
}

