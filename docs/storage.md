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

`repoId` and `worktreeId` are opaque hash-derived scope ids from
`scope_for_search` (root/access scope plus worktree/dirty state); purge validates them
against `[a-z0-9-]` before touching paths.

Tables:

| Table | Role |
| --- | --- |
| `generation` | One row per build: snapshot id/mode, revision, manifest/config/parser fingerprints, build time, optional single-profile vector fingerprint; `state` is `building` until commit |
| `file` | Per-generation file records (content hash, role, lifecycle, corpus, parser version, size) |
| `chunk` | Per-generation chunk records: opaque id, parent id, path, heading ancestry, corpus, redacted text, text hash, exact 1-based line and byte ranges |
| `chunk_fts` | FTS5 content index over chunk path/heading/body for lexical search |
| `chunk_vec` | Per-generation stored vectors: chunk id, input hash, profile fingerprint, dimension, encoded float32 bytes (absent for lexical-only generations) |
| `kv` | Key/value side data (currently the scope's current generation id) |

Invariants:

- A generation is published atomically: one `BEGIN IMMEDIATE` transaction inserts the
  `generation` row, all `file`/`chunk`/`chunk_fts`/`chunk_vec` rows and flips the `kv`
  current-id, then commits. Readers and search always resolve the current generation
  first, so a failed build never exposes a truncated index.
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
