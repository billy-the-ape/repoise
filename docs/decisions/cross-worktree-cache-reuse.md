# Decision note: cross-worktree cache reuse

Status: implemented (post-alpha.1, pre-v1). Evidence:
`crates/repoise-core/tests/cross_worktree.rs` (real temporary Git repositories
and worktrees, deterministic hash embedding provider, process-level
concurrency case).

## Identity

- The reuse family is the canonical Git common directory, obtained through
  `git --git-common-dir` (argument array) and canonicalized; worktrees expose
  it via their `.git` file, which the adapter reads only as Git metadata.
  Primary checkouts and linked attached/detached worktrees therefore resolve
  to the same family; separate clones have different common directories and
  stay isolated even under one explicit external cache root.
- `repo_id`/`worktree_id` semantics are unchanged: they remain bound to the
  destination checkout root, snapshot mode, and containment. A distinct
  family identifier (`family-<sha256(anchor)>`) was added instead of widening
  existing scope contracts, so no ID migration is required.
- The knowledge lives behind `SourceAdapter::reuse_family` so shared
  indexing/search services never parse Git paths. The filesystem/fake
  adapters return no family; submodules resolve their own common directory
  and never join the superproject family. Unresolvable state falls back to
  isolated indexing with a diagnostic, never an error for ordinary indexing.

## Storage resolution

- Default shared store: `<git-common-dir>/repoise/<version>/` — a
  Repoise-owned, versioned directory always outside the corpus, requiring no
  environment setup per task. Explicit `cacheDir`/`REPOISE_CACHE_DIR` keep
  the operator's chosen root; families are namespaced under it at
  `<root>/families/<familyId>/<version>/`. Relative `cacheDir` keeps its
  documented root-relative resolution. `sharedCache: false` is the only new
  setting; it fully disables sharing for that worktree.
- Checkout-local index storage (`repos/<repoId>/worktrees/<worktreeId>/`) is
  unchanged; the checkout-local embedding cache remains the fallback for
  non-family worktrees (plain directories, opt-out, unavailable common
  directory).
- The default-vs-explicit store check canonicalizes both sides (deepest
  existing path prefix), never raw string equality: a non-canonical checkout
  spelling (macOS `/var` vs `/private/var`, Windows short paths, symlinks,
  `..` components) still selects the same family store. Fixed after the
  first cross-platform CI run, where text comparison silently split the
  store per worktree.
- The parse-artifact retention bound is enforced once per build (after the
  file loop), not on every `put`; a cold build is linear in reparsed files
  rather than quadratic in store size.

## Artifact compatibility

- Parse artifacts are immutable, content-addressed JSON documents keyed by
  (content hash, language, parser version, corpus, repository-relative
  path), written atomically (temporary file plus rename). Destination builds
  rematerialize chunks/symbols/imports into their own scope and re-resolve
  references against the destination's eligible file set; donor scope ids are
  never stored or served.
- Redaction is re-applied by the destination before publication, so a
  destination policy can never be weakened by a donor artifact; eligibility
  (include/exclude, ignores, roles, size limits, containment) is always
  enforced by the destination inventory, so donor files can only reduce
  work, never add content.
- The embedding key is unchanged in meaning: prepared-input hash plus the
  full effective profile fingerprint (provider, model, dimension, profile
  version); wrong dimension or non-finite vectors are rejected at lookup.
  The cache itself now lives in the family store for family members.
- Versioning is by directory plus an artifact `version` field: anything
  older or malformed is a safe miss rebuilt locally. Legacy checkout-local
  embedding caches are not migrated; the next embedding build re-embeds.

## Concurrency, GC, and lifecycle

- Artifact publication is immutable and atomic per file; simultaneous
  equivalent misses may duplicate work (documented), but concurrent
  publication always yields complete artifacts and the embedding database
  is serialized by SQLite (WAL), never copied live.
- GC is driven by member registration: one JSON file per live worktree
  scope records its index database; the family sweep collects embedding keys
  referenced by each member's retained generations and reclaims the rest
  once past the 1-hour grace window (recent entries are never reclaimed,
  protecting in-flight sibling builds). A member whose database is gone
  contributes no references (bounded retention); an existing database that
  cannot be opened, or malformed member data, fails the sweep closed
  (conservative retention). Parse artifacts are additionally capped per run;
  embedding reclamation is not capped but is bounded by the live members'
  retained generations plus the grace window.
- Worktree-scoped purge removes only that scope's member registration under
  the family store's `members/` directory (sibling shared data untouched);
  `purge --all` removes everything under the selected cache root, including
  `families/` only when that is where the store lives. Deletion is limited to
  Repoise-owned paths (validated scope ids, no `..`, no symlink-following
  escapes into Git metadata).
- A failed build never leaves a publishable partial destination generation
  (unchanged atomic publication) and never corrupts shared data (atomic
  artifact writes, validated reads).

Tradeoffs accepted: no remote or cross-machine sharing; no immutable-base +
overlay query engine; duplicate provider work on racing cold misses; warm
builds still scan/hash destination files and rebuild SQLite/FTS rows.
