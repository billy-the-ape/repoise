# Code structure

Repoise is a Rust 2024 workspace with resolver 3 and one shared application lockfile.
The core crate's external dependencies are serde (configuration/JSON), `rusqlite` with
the bundled SQLite build (persistent index) and the offline tree-sitter TypeScript and
JavaScript grammars (code chunking); there are no provider or network dependencies. The
Git adapter shells out to `git` with argument arrays when operating on a Git repository.

| Path | Responsibility |
| --- | --- |
| `crates/repoise-core/src/lib.rs` | Shared engine boundary; exposes the services below plus config/init file names and defaults |
| `crates/repoise-core/src/adapter/` | Neutral `SourceAdapter` contract: opaque revisions, capability flags, enumerate/read with containment; filesystem and Git adapters plus a synthetic fake-revision test adapter |
| `crates/repoise-core/src/config.rs` | Versioned `repoise.config.json` parsing, precedence (CLI > local > committed > default) and redaction-safe validation |
| `crates/repoise-core/src/ignore.rs` | Explainable include/exclude/secret-deny policy: package defaults, config, native ignore inheritance, gitignore-style matching, stable fingerprints |
| `crates/repoise-core/src/classify.rs` | Role/lifecycle classification: explicit front matter, configured rules, path inference |
| `crates/repoise-core/src/discovery.rs` | Deterministic inventory: snapshot manifest, file records, skip diagnostics |
| `crates/repoise-core/src/provenance.rs` | Versioned repository/snapshot/file provenance record shapes and remote identity sanitization |
| `crates/repoise-core/src/init.rs` and `doctor.rs` | Idempotent overlay init (config, overlay manifest, optional managed block and AGENTS snippet) and effective-settings reporting |
| `crates/repoise-core/src/overlay.rs` | Non-destructive overlay lifecycle: uninstall of unchanged managed files/blocks, three-way template update with installed-byte baselines, conflict reporting |
| `crates/repoise-core/src/hash.rs` | Lowercase SHA-256 helpers used across manifests and fingerprints |
| `crates/repoise-core/src/store.rs` | SQLite (WAL) + FTS5 persistent store: transactional generation publication, retention, current/previous generation access |
| `crates/repoise-core/src/chunk/` | Structural chunkers (Markdown, plain text, config) with exact line ranges, labeled oversized splits and parent references, plus the pinned tree-sitter TypeScript/TSX and JavaScript/JSX code grammars and a line-window fallback for other code languages |
| `crates/repoise-core/src/indexing.rs` | Incremental index build: same-worktree and cross-worktree family reuse, reparse/tombstone reconciliation, destination-scoped rematerialization of shared artifacts, secret redaction, code symbol and reference-edge resolution, optional cache-first embedding with atomic vector publication, embedding-cache GC, optional phase/progress reporting, `state.json` publication |
| `crates/repoise-core/src/progress.rs` | Structured index progress contract: phase enum, reporter trait (phase start, completed-unit counters, phase end) and the no-op reporter |
| `crates/repoise-core/src/shared.rs` | Cross-worktree reuse family: Git common-directory family identity (adapter seam), versioned shared-store resolution (default common-directory or explicit cache root), atomic content-addressed parse-artifact cache, member registration, family embedding-cache reference collection and bounded GC, sharing opt-out and fallback diagnostics |
| `crates/repoise-core/src/embed.rs` | Versioned embedding provider interface, profile fingerprinting, vector dimension/finiteness validation, bounded embedding client (batching, timeout, retry, cancellation, budgets), content-addressed SQLite embedding cache and reference-counted GC |
| `crates/repoise-core/src/history.rs` | Bounded local history lane: adapter `HistoryProvider` seam, first-parent horizon sets with explicit shallow/horizon gaps, PR-hint parsing, stable item ids, separate FTS5/BM25 history search lane, and the optional verified-host enrichment session (provider seam, repository-scoped ETag host cache, per-build budgets, fail-closed revocation) |
| `crates/repoise-core/src/search.rs` | Lexical search (BM25 + explainable boosts, per-file cap, bound cursors, fallback suggestions) plus hybrid RRF fusion over stored vectors with retrieval-mode/coverage reporting and lexical degradation; scope resolution, the shared current-generation entry preamble of the search/history/read/related lanes, the shared FTS MATCH expression builder, and GitHub remote/SHA validation (shared with the history lane) |
| `crates/repoise-core/src/read.rs` | Exact read-back with file/content-hash and chunk-text-hash validation; optional output-token budget with line-boundary truncation (`truncated`/`output_tokens`); `Stale` diagnostics |
| `crates/repoise-core/src/status.rs` | Scope/snapshot/index/freshness view with config and cache diagnostics |
| `crates/repoise-core/src/check.rs` | Offline freshness/coverage check over the status view with stable categories for automation (no provider calls) |
| `crates/repoise-core/src/related.rs` | Bounded related-knowledge service: syntactic reference edges, reverse references and parent/child links over labeled chunk splits from a source chunk |
| `crates/repoise-core/src/watch.rs` | Incremental watch loop: debounced scanning, coalesced per-path content, bounded pending-path publication through the shared index operation (including the configured embedding session, built once per watch process), branch-switch rescan and cooperative cancellation (the CLI installs the SIGINT/SIGTERM handlers; in `--events` mode a `cancelled` terminal event ends the stream) |
| `crates/repoise-core/src/bench/` | Benchmark coordinator (AP-04/AP-05): suite manifest validation (including AP-05 evaluator, gates and report fields), preflight (reproducible snapshot, output-root dirtiness exclusion, prerequisites, vector-coverage gates), seeded trial plan, disposable-worktree trials with trial-local index builds and the pinned read-only MCP argv, budgets, interruption, data-only post-trial grading (deterministic checks, manual records, censored-trial handling), report assembly (counts, grading summaries, task-cluster bootstrap paired analysis, predeclared quality gate) and offline report rebuild; the agent loop itself is delegated to the AP-03 host bridge |
| `crates/repoise-core/src/cache.rs` | Cache root resolution (config/env override) and scope layout paths |
| `crates/repoise-core/src/purge.rs` | Removal of generated cache data only, with scope-id validation |
| `crates/repoise-core/src/ownership.rs` | Advisory writer-ownership marker (`owner.json`) for one worktree scope (AP-02): best-effort atomic acquire/refresh/release, concurrent and stale-owner detection, read-only `inspect` view surfaced by `status`; removed when the scope is purged |
| `crates/repoise-cli/src/main.rs` | Native executable, argument handling and terminal I/O (doctor/explain/index/init/status/search/read/purge/related/check/watch/overlay/mcp/bench/capabilities adapters; optional `--events` NDJSON stream for index and watch; SIGINT/SIGTERM cancellation installed for watch) |
| `crates/repoise-cli/src/mcp.rs` | stdio MCP server (JSON-RPC 2.0 over stdin/stdout): `search_project_knowledge`, `read_project_knowledge`, `related_project_knowledge`, `project_knowledge_status` and the opt-in `refresh_project_knowledge`; scope-bound, read-only by default, stdout carries protocol traffic only; optional `--search-mode` pin refusing requests that name a different mode (used by `bench` trials) |
| `crates/repoise-cli/src/github_enrichment.rs` | Optional (`github-enrichment` CLI feature) read-only GitHub transport for history enrichment: commit-to-PR verification, ETag-cached change-request fetch, request budgets, rate-limit/permission classification |
| `crates/repoise-cli/src/progress.rs` | Terminal rendering of index progress: throttled in-place line on a TTY, appended lines otherwise, stderr under `--json`, honest ETAs (the ETA helpers are shared with the event reporter), and one printed final line per phase |
| `crates/repoise-cli/src/capabilities.rs` | AP-02 `capabilities` handshake document: version, event-stream protocol and schema, build features, command/machine-output matrix, stable exit codes and compatibility policy |
| `crates/repoise-cli/src/events.rs` | AP-02 `--events` NDJSON stream: versioned event envelopes, run lifecycle (`started`, exactly one terminal event), the event progress reporter and heartbeat |
| `crates/repoise-core/tests/` and `crates/repoise-cli/tests/` | Contract and executable behavior fixtures |
| `schemas/` | Published versioned schemas: configuration, event stream, capabilities, agent adapter request, bench suite and bench trial grading |
| `npm/repoise/` | Thin launcher and allowlisted npm package metadata |
| `packages/agent-adapter-protocol/` and `packages/agent-adapter-cline/` | AP-03 agent adapter protocol: the checked-in Node host bridge (`adapter-host-run.mjs`, driven by the bench coordinator), fake conformance adapters and the first real adapter (Cline); no-Node operation is unaffected |
| `scripts/release/` | Claim preparation, packaging, smoke tests and release staging |
| `scripts/` | Local checks, release build and installation helpers |
| `.github/workflows/` | Cross-platform checks and manually requested build artifacts |
| `docs/plans/` | Intended scope and acceptance gates, not shipped behavior |
| `docs/storage.md` | Current-state guide for the persistent index and cache layout |
| `docs/mcp.md` | Current-state guide for the stdio MCP server: launch, tools, client registration |
| `docs/bench.md` | Current-state guide for the benchmark runner: suites, preflight, trial lifecycle, grading, reports, `bench report` and artifacts |

The CLI depends on the core; the core must not depend on a CLI or agent client.
The MCP stdio adapter (`crates/repoise-cli/src/mcp.rs`) calls the same core services
(`indexing`, `search`, `read`, `related`, `status`) that the CLI adapters already use,
and serves the identical versioned response schemas.
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
