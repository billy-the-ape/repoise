# Code structure

Repoise is a Rust 2024 workspace with resolver 3 and one shared application lockfile.
The core crate's only external dependencies are serde (configuration/JSON) and
`rusqlite` with the bundled SQLite build (persistent index); there are no parser,
provider, or network dependencies. The Git adapter shells out to `git` with argument
arrays when operating on a Git repository.

| Path | Responsibility |
| --- | --- |
| `crates/repoise-core/src/lib.rs` | Shared engine boundary; exposes the services below plus config/init file names and defaults |
| `crates/repoise-core/src/adapter/` | Neutral `SourceAdapter` contract: opaque revisions, capability flags, enumerate/read with containment; filesystem and Git adapters plus a synthetic fake-revision test adapter |
| `crates/repoise-core/src/config.rs` | Versioned `repoise.config.json` parsing, precedence (CLI > local > committed > default) and redaction-safe validation |
| `crates/repoise-core/src/ignore.rs` | Explainable include/exclude/secret-deny policy: package defaults, config, native ignore inheritance, gitignore-style matching, stable fingerprints |
| `crates/repoise-core/src/classify.rs` | Role/lifecycle classification: explicit front matter, configured rules, path inference |
| `crates/repoise-core/src/discovery.rs` | Deterministic inventory: snapshot manifest, file records, skip diagnostics |
| `crates/repoise-core/src/provenance.rs` | Versioned repository/snapshot/file provenance record shapes and remote identity sanitization |
| `crates/repoise-core/src/init.rs` and `doctor.rs` | Idempotent overlay init (config, overlay manifest, optional managed block) and effective-settings reporting |
| `crates/repoise-core/src/hash.rs` | Lowercase SHA-256 helpers used across manifests and fingerprints |
| `crates/repoise-core/src/store.rs` | SQLite (WAL) + FTS5 persistent store: transactional generation publication, retention, current/previous generation access |
| `crates/repoise-core/src/chunk/` | Structural chunkers (Markdown, plain text, config) with exact line ranges, labeled oversized splits and parent references |
| `crates/repoise-core/src/indexing.rs` | Incremental index build: reuse/reparse/tombstone reconciliation, secret redaction, optional cache-first embedding with atomic vector publication, embedding-cache GC, `state.json` publication |
| `crates/repoise-core/src/embed.rs` | Versioned embedding provider interface, profile fingerprinting, vector dimension/finiteness validation, bounded embedding client (batching, timeout, retry, cancellation, budgets), content-addressed SQLite embedding cache and reference-counted GC |
| `crates/repoise-core/src/search.rs` | Lexical search (BM25 + explainable boosts, per-file cap, bound cursors, fallback suggestions) plus hybrid RRF fusion over stored vectors with retrieval-mode/coverage reporting and lexical degradation; scope resolution and GitHub permalink validation |
| `crates/repoise-core/src/read.rs` | Exact read-back with file/content-hash and chunk-text-hash validation; `Stale` diagnostics |
| `crates/repoise-core/src/status.rs` | Scope/snapshot/index/freshness view with config and cache diagnostics |
| `crates/repoise-core/src/cache.rs` | Cache root resolution (config/env override) and scope layout paths |
| `crates/repoise-core/src/purge.rs` | Removal of generated cache data only, with scope-id validation |
| `crates/repoise-cli/src/main.rs` | Native executable, argument handling and terminal I/O (doctor/explain/index/init/status/search/read/purge adapters) |
| `crates/repoise-core/tests/` and `crates/repoise-cli/tests/` | Contract and executable behavior fixtures |
| `schemas/` | Published versioned configuration schema |
| `npm/repoise/` | Thin launcher and allowlisted npm package metadata |
| `scripts/release/` | Claim preparation, packaging, smoke tests and release staging |
| `scripts/` | Local checks, release build and installation helpers |
| `.github/workflows/` | Cross-platform checks and manually requested build artifacts |
| `docs/plans/` | Intended scope and acceptance gates, not shipped behavior |
| `docs/storage.md` | Current-state guide for the persistent index and cache layout |

The CLI depends on the core; the core must not depend on a CLI or agent client.
A future MCP adapter must call the same core services (`indexing`, `search`, `read`,
`status`) that the CLI adapters already use.
Add crates when real dependency, platform or feature boundaries justify them; avoid empty
placeholder crates.

Keep language parsers, source-control adapters, hosting enrichment, embeddings and persistence
behind explicit contracts as they are implemented. Do not let npm, GitHub, Git or a particular
language become a mandatory engine dependency. Configuration must remain data, never executed code.
The npm launcher only selects/runs an exact-version registry-delivered native package.
Other distribution adapters should use the same native executable. Registry ownership is
separate from internal Cargo package names; see [releases](releases.md).

See the [master plan](plans/v1_master_plan.md) for planned contracts and milestones.
Update this guide when crate boundaries or source entry points change.
