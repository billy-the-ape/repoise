# Persistent index and cache (current state)

The offline index is a local SQLite database (WAL mode, `rusqlite` with the bundled
SQLite build) in the resolved cache root. The default cache root is the project-local
`.repoise` directory; `repoise.cacheDir` in config or the `REPOISE_CACHE_DIR`
environment variable relocate it (an ignored, project-local cache directory is the
supported portable/container workflow). The cache root is generated data only: it
never participates in the eligible source corpus and can be removed at any time.

Layout under the cache root:

| Path | Contents |
| --- | --- |
| `repos/<repoId>/worktrees/<worktreeId>/index.sqlite` | All index tables for one scope |
| `repos/<repoId>/worktrees/<worktreeId>/state.json` | Versioned integration manifest; the database remains canonical |
| `repos/<repoId>/worktrees/<worktreeId>/owner.json` | Advisory writer-ownership marker for the scope (AP-02): owner pid, operation and last-refresh time, published atomically; exposed read-only by `status` and removed when the scope is purged (see [automation.md](automation.md)) |
| `repos/<repoId>/embedding-cache/` | Checkout-local content-addressed embedding cache (`embeddings.sqlite`), used when no reuse family is available (plain directories, sharing opted out, or fallback): input-hash plus profile-fingerprint keyed vectors, reference-counted against retained generations |
| `repos/<repoId>/host-cache/<host>.json` | Verified remote host records for history enrichment (ETag plus bounded payload per key, with an optional paged-list continuation); invalidated on remote permission denial; removed when the repository scope is purged (`purge --all`), not by a worktree-scoped purge |

`repoId` and `worktreeId` are opaque hash-derived scope ids from
`scope_for_search` (root/access scope plus worktree/dirty state); purge validates them
against `[a-z0-9-]` before touching paths.

## Cross-worktree reuse family store

Linked worktrees of one local Git repository belong to one reuse family,
identified by the canonical Git common directory (obtained through `git
--git-common-dir`, never by path layout or remote URL). Each worktree keeps its
own checkout-local index scope (`repoId`/`worktreeId` above); the family store
holds only reusable immutable artifacts shared between them:

| Path (default, versioned) | Contents |
| --- | --- |
| `<git-common-dir>/repoise/1/parse-cache/<key>.json` | Content-addressed parse artifacts (exact parse inputs: content hash, language, parser version, corpus, repository-relative path). Written atomically (temporary file plus rename); unknown-version or malformed entries are safe misses, never trusted |
| `<git-common-dir>/repoise/1/embeddings.sqlite` | Family-scoped content-addressed embedding cache (same format as the checkout-local one above), keyed by prepared-input hash plus full profile fingerprint |
| `<git-common-dir>/repoise/1/members/` | Member registration (one JSON file per worktree scope: `repoId`, `worktreeId`, snapshot mode, index database path, registration time), used by GC to retain vectors referenced by live worktree generations |

With an explicit cache location (`cacheDir` or `REPOISE_CACHE_DIR`), the family
store moves under that root instead: `<cache-root>/families/<familyId>/1/`,
where `familyId` is the anchor-derived family identifier. An explicit external
cache therefore stays family-namespaced; two independent clones under one
external root never share. `sharedCache: false` in configuration opts a
worktree out entirely (isolated indexing, no shared reads or writes). If the
canonical common directory is unavailable, indexing falls back to the
checkout-local layout with an accurate diagnostic; no shared-cache hit is ever
claimed when isolated fallback occurred.

The versioned directory (`1`) marks the shared layout and artifact format.
Older or incompatible versions are treated as misses and rebuilt; nothing is
migrated in place. Legacy checkout-local embedding caches are simply
re-embedded on the next embedding build (a cold rebuild), not migrated.

Invariants specific to the family store:

- Artifacts and vectors are immutable and content/profile-addressed; they
  carry no scope-bound chunk or source ids. Destination builds rematerialize
  them into their own scope and re-resolve all references against the
  destination's eligible files.
- Artifact publication is atomic per file, and the artifact retention bound
  is enforced once per build (after the file loop), not per write; the
  embedding database uses the usual SQLite WAL discipline (no copying of a
  live database file).
- Default-versus-explicit store selection compares canonicalized locations
  (canonicalizing the deepest existing path prefix), so equivalent
  spellings of the same checkout-local cache root — symlinked prefixes,
  Windows short paths, `..` components — select the same family store as
  the checkout's siblings.
- Family GC runs only when a member-registered store is healthy. It collects
  the embedding keys referenced by each registered member's retained
  generations and reclaims unreferenced entries once they are older than the
  1-hour grace window (`GC_GRACE_MS`); entries created within the window are
  never reclaimed, so an in-flight sibling build cannot lose vectors it is
  about to reference. A member whose index database is gone (purged or removed
  worktree) contributes no references, so its older unreferenced vectors are
  reclaimed; when such a worktree returns, the next embedding build
  re-embeds them, and its already-published generation stays valid (vectors
  are stored in the scope index as well as the shared cache). If an existing member
  database cannot be opened, or member data is malformed, the sweep is skipped
  entirely (fail closed). Parse artifacts additionally have an explicit
  per-run cap (`MAX_PARSE_ARTIFACTS`, oldest first); embedding reclamation has
  no per-run cap but is bounded by the live members' retained generations and
  the grace window.
- Worktree-scoped `purge` removes only that scope's member registration under
  the family store's `members/` directory (sibling shared data is untouched);
  `purge --all` removes everything under the selected cache root, including
  `families/` when the family store lives under an explicit cache root, but
  never the common-directory default store and never Git metadata.
- Read-only operations (search, read, status, read-only MCP) never create or
  migrate family-store data.

Tables:

| Table | Role |
| --- | --- |
| `generation` | One row per build: snapshot id/mode, revision, manifest/config/parser fingerprints, build time, optional single-profile vector fingerprint, embedding scope (`docs` or `all`), history-lane item count and the persisted history-lane coverage summary (`history_meta` JSON: enabled flag plus gaps/enrichment outcome for `status` and history search); `state` is `building` until commit |
| `file` | Per-generation file records (content hash, role, lifecycle, corpus, parser version, parser-error range count, size) |
| `chunk` | Per-generation chunk records: opaque id, parent id, path, heading ancestry, corpus, redacted text, text hash, primary symbol, split context, exact 1-based line and byte ranges |
| `chunk_fts` | FTS5 content index over chunk path/heading/symbol/context/body for lexical search |
| `chunk_vec` | Per-generation stored vectors: chunk id, input hash, profile fingerprint, dimension, encoded float32 bytes (absent for lexical-only generations) |
| `symbol` | Per-generation code symbol records: opaque id, path, name, kind, line range, parent symbol, export flag, covering chunk id |
| `reference` | Per-generation code reference edges: opaque id, path, name, kind, confidence, line, covering chunk id, optional target symbol id |
| `history_item` | Per-generation bounded history items (opt-in lane): revision/parents, redacted message, author, commit time, bounded affected paths and hunk descriptors, unverified PR hints, optional verified host association JSON |
| `history_fts` | FTS5 content index over history message/revision/paths/host metadata for the separate history search lane |
| `kv` | Key/value side data (currently the scope's current generation id) |

Invariants:

- A generation is published atomically: one `BEGIN IMMEDIATE` transaction inserts the
  `generation` row, all `file`/`chunk`/`chunk_fts`/`chunk_vec` rows, the code
  `symbol`/`reference` rows, the optional history `history_item`/`history_fts`
  rows and flips the `kv` current-id, then commits. Readers and
  search always resolve the current generation first, so a failed build never exposes a
  truncated index. Symbol parents and reference targets must exist in the same
  generation; publication rejects dangling edges rather than storing them.
- The v3 schema upgrade is additive and idempotent: missing columns are added when
  absent, and the FTS table is rebuilt in place only when it predates the `context`
  column (it serves the current generation, so no data is lost). The v4 upgrade adds
  the `history_item`/`history_fts` tables and the `generation.history` item count for
  the opt-in history lane; the v5 upgrade adds the `generation.history_meta`
  persisted lane-coverage summary column.
- Shared embedding-cache entries are reference-counted against the retained worktree
  generations of the repository scope after each publication; unreferenced entries
  older than the grace window are reclaimed (recent entries are retained, so an
  in-flight build cannot lose vectors it is about to reference), so a failed or
  superseded embedding job never serves a vector.
- Retention keeps the current generation plus the previous one; older rows are deleted
  in the same scope after each successful publish.
- Chunk ids bind scope, parser version, path, structural address and text hash, so
  unchanged sections keep identity across builds while location metadata is refreshed.
- Known secret shapes never reach stored chunk text: discovery skips secret-bearing
  files, and the indexer redacts any remaining shape from chunk text (both are
  defense-in-depth; neither guarantees complete secret detection).
- Search cursors encode the scope, query, filters and generation, so a cursor from an
  old generation or a different query is rejected, not silently reused.
- `read` re-reads the file at the snapshot and validates the file content hash and the
  chunk text hash; any mismatch is a `stale` result with refresh guidance, never a
  silent substitution.

## Vector coverage semantics

Embedding is opt-in and scope-bounded: the generation's persisted embedding
scope (`docs`, or `all` for docs + code) decides which chunks may ever
carry a vector. `status`, `check` and the bench hybrid preflight share one
scope-aware coverage computation over the published generation:

- **eligible chunks** = docs chunks plus code chunks when the stored scope
  is `all`;
- **ineligible chunks** = every other corpus (e.g. config) — lexical-only,
  never embedded and never pending;
- **pending** = eligible chunks without a vector for the generation's
  stored profile fingerprint (vectors from a different profile never
  satisfy coverage);
- coverage state is `disabled` (no stored profile, no embedding configured),
  `profile-unavailable` (embeddings configured but the served generation is
  lexical-only — a rebuild, not pending work), `empty` (stored profile over
  an empty eligible corpus — deterministic, nothing pending, and not proof
  of hybrid retrieval), `partial` (eligible chunks await vectors) or
  `complete` (every eligible chunk covered).

The judgment always uses the generation's persisted scope/profile, never
today's configuration: changing the embedding scope or profile changes the
config fingerprint (stale → rebuild required), and old vectors are never
reinterpreted under the new profile/scope. `status` exposes the machine
fields (`eligible_chunks`, `ineligible_chunks`, `vectors_pending`,
`embedding_coverage`) alongside the total `chunks` count, which stays the
lexical corpus size; no schema migration is required (the `generation` row
already stores `vector_profile` and `embedding_scope`).

Commands: `repoise index` (build/publish), `repoise status` (scope/snapshot/freshness;
exit 3 when the index is missing or stale), `repoise search`/`repoise read` (offline
lexical search and exact read-back), `repoise purge` (remove one scope or the whole
generated layout; source files are never touched). Freshness is judged from the
published content manifest and effective configuration against the live snapshot
for the declared scope; Git working-tree dirtiness is reported in the status view
but never makes the index stale. See
[development](development.md) for examples and [v1-1.md](plans/complete/v1-1.md) for the card.
