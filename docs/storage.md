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
| `repos/<repoId>/embedding-cache/` | Shared content-addressed embedding cache (`embeddings.sqlite`): input-hash plus profile-fingerprint keyed vectors, reference-counted against retained generations |
| `repos/<repoId>/host-cache/<host>.json` | Verified remote host records for history enrichment (ETag plus bounded payload per key); invalidated on remote permission denial and purged with the repository scope |

`repoId` and `worktreeId` are opaque hash-derived scope ids from
`scope_for_search` (root/access scope plus worktree/dirty state); purge validates them
against `[a-z0-9-]` before touching paths.

Tables:

| Table | Role |
| --- | --- |
| `generation` | One row per build: snapshot id/mode, revision, manifest/config/parser fingerprints, build time, optional single-profile vector fingerprint, embedding scope (`docs` or `docs+code`); `state` is `building` until commit |
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
  the `history_item`/`history_fts` tables for the opt-in history lane.
- Shared embedding-cache entries are reference-counted against the retained worktree
  generations of the repository scope after each publication; unreferenced entries are
  reclaimed, so a failed or superseded embedding job never serves a vector.
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

Commands: `repoise index` (build/publish), `repoise status` (scope/snapshot/freshness;
exit 3 when the index is missing or stale), `repoise search`/`repoise read` (offline
lexical search and exact read-back), `repoise purge` (remove one scope or the whole
generated layout; source files are never touched). See
[development](development.md) for examples and [v1-1.md](plans/v1-1.md) for the card.
