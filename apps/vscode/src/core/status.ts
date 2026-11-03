import { runCli } from "./run";

/**
 * `repoise status --json` parsing for one workspace root.
 *
 * Field names are the CLI's snake_case JSON keys (StatusView in
 * crates/repoise-core/src/status.rs). Exit code 3 ("index missing or stale")
 * is a *usable* status result — the JSON still describes the scope and
 * freshness — while exit 1/2 are failures.
 */

export interface RootStatusScope {
  adapter: string;
  root: string;
  snapshotMode: string;
  repoId: string;
  worktreeId: string;
  remoteIdentity: string | null;
}

export interface RootStatusSnapshot {
  snapshotId: string;
  revision: string | null;
  branch: string | null;
  mode: string;
  dirtyCount: number;
}

export interface RootStatusIndex {
  generationId: number;
  files: number;
  chunks: number;
  vectors: number;
  vectorsPending: number;
  embeddingScope: string;
  embeddingCoverage: string;
}

export interface RootStatusWriter {
  lockHeld: boolean;
  state: "owned" | "busy" | "stale-metadata" | "free";
  owner: {
    pid: number;
    operation: string;
    startedAtMs: number;
    refreshedAtMs: number;
    state: string;
  } | null;
}

export interface RootStatus {
  requestedRoot: string;
  scope: RootStatusScope;
  snapshot: RootStatusSnapshot | null;
  index: RootStatusIndex | null;
  freshness: { status: string; reasons: string[] } | null;
  writer: RootStatusWriter | null;
  lastError: string | null;
}

export type StatusOutcome =
  | { ok: true; status: RootStatus }
  | { ok: false; problem: string; detail: string };

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function parseOwner(raw: unknown): RootStatusWriter["owner"] {
  if (typeof raw !== "object" || raw === null) {
    return null;
  }
  const o = raw as Record<string, unknown>;
  const pid = asNumber(o.pid);
  const operation = asString(o.operation);
  if (pid === null || operation === null) {
    return null;
  }
  return {
    pid,
    operation,
    startedAtMs: asNumber(o.started_at_ms) ?? 0,
    refreshedAtMs: asNumber(o.refreshed_at_ms) ?? 0,
    state: asString(o.state) ?? "unknown",
  };
}

function parseWriter(
  raw: unknown,
  topLevelOwner: unknown,
): RootStatusWriter | null {
  if (typeof raw !== "object" || raw === null) {
    return null;
  }
  const w = raw as Record<string, unknown>;
  const state = asString(w.state);
  if (
    state !== "owned" &&
    state !== "busy" &&
    state !== "stale-metadata" &&
    state !== "free"
  ) {
    return null;
  }
  // The CLI exposes the owner marker as a top-level field next to the
  // writer summary; nested owner data is also tolerated.
  const owner = parseOwner(w.owner) ?? parseOwner(topLevelOwner);
  return { lockHeld: w.lock_held === true, state, owner };
}

export function parseStatusView(
  requestedRoot: string,
  raw: string | unknown,
): { ok: true; status: RootStatus } | { ok: false; problem: string } {
  let view: unknown;
  if (typeof raw === "string") {
    try {
      view = JSON.parse(raw);
    } catch {
      return { ok: false, problem: "invalid-json" };
    }
  } else {
    view = raw;
  }
  if (typeof view !== "object" || view === null || Array.isArray(view)) {
    return { ok: false, problem: "not-object" };
  }
  const v = view as Record<string, unknown>;
  const scope = v.scope;
  if (typeof scope !== "object" || scope === null) {
    return { ok: false, problem: "missing-scope" };
  }
  const s = scope as Record<string, unknown>;
  const scopeRoot = asString(s.root);
  if (scopeRoot === null) {
    return { ok: false, problem: "missing-scope-root" };
  }

  const status: RootStatus = {
    requestedRoot,
    scope: {
      adapter: asString(s.adapter) ?? "unknown",
      root: scopeRoot,
      snapshotMode: asString(s.mode) ?? "unknown",
      repoId: asString(s.repo_id) ?? "unknown",
      worktreeId: asString(s.worktree_id) ?? "unknown",
      remoteIdentity: asString(s.remote_identity),
    },
    snapshot: null,
    index: null,
    freshness: null,
    writer: parseWriter(v.writer, v.owner),
    lastError: asString(v.last_error),
  };

  if (typeof v.snapshot === "object" && v.snapshot !== null) {
    const sn = v.snapshot as Record<string, unknown>;
    status.snapshot = {
      snapshotId: asString(sn.snapshot_id) ?? "unknown",
      revision: asString(sn.revision),
      branch: asString(sn.branch),
      mode: asString(sn.mode) ?? "unknown",
      dirtyCount: asNumber(sn.dirty_count) ?? 0,
    };
  }

  if (typeof v.index === "object" && v.index !== null) {
    const ix = v.index as Record<string, unknown>;
    const generationId = asNumber(ix.generation_id);
    if (generationId !== null) {
      status.index = {
        generationId,
        files: asNumber(ix.files) ?? 0,
        chunks: asNumber(ix.chunks) ?? 0,
        vectors: asNumber(ix.vectors) ?? 0,
        vectorsPending: asNumber(ix.vectors_pending) ?? 0,
        embeddingScope: asString(ix.embedding_scope) ?? "unknown",
        embeddingCoverage: asString(ix.embedding_coverage) ?? "unknown",
      };
    }
  }

  if (typeof v.freshness === "object" && v.freshness !== null) {
    const f = v.freshness as Record<string, unknown>;
    const freshnessStatus = asString(f.status);
    if (freshnessStatus !== null) {
      status.freshness = {
        status: freshnessStatus,
        reasons:
          Array.isArray(f.reasons) &&
          f.reasons.every((r) => typeof r === "string")
            ? f.reasons
            : [],
      };
    }
  }

  return { ok: true, status };
}

/** Exit codes that still carry a parseable, useful status document. */
export function statusExitUsable(exitCode: number | null): boolean {
  return exitCode === 0 || exitCode === 3;
}

export function describeStatusFailure(
  exitCode: number | null,
  stderr: string,
): string {
  const detail = stderr.trim().replace(/\s+/g, " ").slice(0, 400);
  if (exitCode === 2) {
    return `repoise status exited 2 (usage error)${detail ? `: ${detail}` : ""}`;
  }
  if (exitCode === 3) {
    return "repoise status exited 3 but returned no parseable document";
  }
  return `repoise status exited ${exitCode ?? "abnormally"}${detail ? `: ${detail}` : ""}`;
}

export async function runStatus(
  binary: string,
  root: string,
): Promise<StatusOutcome> {
  const lines: string[] = [];
  const handle = runCli({
    binary,
    args: ["status", "--json", root],
    onStdoutLine: (line) => lines.push(line),
  });
  const result = await handle.promise;
  if (!statusExitUsable(result.exitCode)) {
    return {
      ok: false,
      problem: "exit",
      detail: describeStatusFailure(result.exitCode, result.stderr),
    };
  }
  const parsed = parseStatusView(root, lines.join("\n"));
  if (!parsed.ok) {
    return {
      ok: false,
      problem: "parse-error",
      detail: parsed.problem,
    };
  }
  return { ok: true, status: parsed.status };
}
