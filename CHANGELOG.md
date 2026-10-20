# Changelog

## Unreleased

### Workspace and CI foundation (AP-01)

The development tooling became a Cargo + pnpm workspace. pnpm 12.3.4 is pinned by the root `packageManager` field; the committed `pnpm-lock.yaml` replaces `package-lock.json` for development installs (`pnpm install --frozen-lockfile --ignore-scripts`). Turborepo 2.11.9 declares the root development tasks (`format`, `format:check`, `test`) and single per-platform native bridge tasks (`check:native`, `build:native` and their `:windows` variants) with output caching disabled; Cargo remains authoritative for Rust and keeps working without Node. The published launcher stays outside the pnpm workspace, so root frozen installs never resolve its optional platform-package pins; CI jobs provision the pinned pnpm with lifecycle scripts enabled (pnpm 12's postinstall swaps in the native binary) outside the checkout, with an in-workflow version guard.

CI now selects affected check sets for pull requests through `scripts/ci/impact-map.json` and `scripts/ci/select-checks.mjs` (unknown paths, empty diffs and selection failures run the full suite), and a stable aggregate `CI gate` job enforces expected results, failures and cancellations. Pushes to main always run the full suite, and the required `Check (OS)` job names are unchanged.

Release tooling resolves the real npm CLI even when pnpm points `npm_execpath` at pnpm's own CLI. Registry packing/staging semantics, the published packages and consumer installation are unchanged; no consumer migration is required. See `docs/plans/agent-platform/01-workspace-and-ci.md` for the implementation record.

### Cross-worktree cache reuse

Linked worktrees of one local Git repository now share reusable parse
artifacts and embedding vectors through a versioned family store under the
canonical Git common directory (or under `families/<familyId>/` in an
cache root explicitly configured via `cacheDir`/`REPOISE_CACHE_DIR`). Each
worktree still publishes its own scope-bound index; search, exact reads,
and freshness always reflect the destination checkout. `repoise index` in a
new worktree at the same commit skips parsing for unchanged compatible files
and, for a completed compatible embedding profile, makes no
document-embedding provider calls; small deltas reparse only affected
files. New configuration option: `sharedCache` (default `true`) opts a
worktree out of sharing. `doctor` and `status` report the sharing state and
store location; `index` reports `files_family_reused`/`chunks_family_reused`
(JSON and human output). No new environment variables, secrets, or migration
steps are required; existing caches keep working (legacy checkout-local
embedding caches are re-embedded on the next embedding build, not migrated).
Worktree-scoped `purge` removes only that scope's member registration under the
family store's `members/` directory (sibling shared data untouched); `purge --all`
removes `families/` only when the family store lives under the explicitly
selected cache root. Independent clones, plain directories, and opted-out worktrees
remain isolated. See [storage.md](docs/storage.md#cross-worktree-reuse-family-store)
and [agents.md](docs/agents.md#linked-worktrees).

## 0.1.0-alpha.1 — 2026-10-15 (functional preview)

First functional preview of the offline repository knowledge engine. Includes
incremental docs/code indexing, lexical search, hash-validated exact reads,
related links, freshness checks, watch, managed agent guidance, stdio MCP,
and opt-in embeddings/history. See [preview readiness](docs/publish/alpha-1-readiness.md).

Published under npm `next` with native prerelease archives; the `latest`
dist-tag was promoted to this version on October 19, 2026. See the
[publication record](docs/publish/alpha-1-status.md). This is not a stable v1 release. Pilot runs verify basic operation but do not
establish consistent agent speed or token savings. Exact-identifier ranking
and agent retrieval strategy remain follow-up work. Fresh registry installation and CLI/MCP checks passed on macOS ARM64;
Linux/Windows public-install checks remain unmeasured. Cargo registry
publication stays disabled.

### Pre-v1 reliability — pilot follow-up

- `doctor` now reports the invocation it actually used: the selected snapshot
  mode (`--committed` for Git roots is honored) and the configuration paths
  actually selected (repository defaults or external `--config` /
  `--local-config` overrides), each with an existence status. Before, `doctor`
  always reported the working-tree mode and only the in-repository config
  paths, regardless of the flags used with the other commands.
- Lexical search now enforces its documented match semantics: the FTS5
  expression joined per-term field groups with top-level `AND` but left the
  per-term `OR` groups unparenthesized, so FTS5 precedence (AND binds tighter
  than OR) silently relaxed multi-term queries to a mostly-OR match. Each
  per-term group is now parenthesized (the history lane already carried this
  fix), so every whitespace query term must co-occur in one chunk again.
  Query-time only: no reindex required, but broad queries may return fewer,
  more precise results than before.
- The in-tuning eval query Q-ACME-TEST-01 was relabeled
  ("computeBatchId stable first second" → "computeBatchId first second"):
  its "stable" term occurs only inside the camelCase identifier
  `testBatchIdIsStable`, which the unicode61 tokenizer keeps as one token,
  so under the strict all-terms semantics the old query could match no
  chunk. Gold evidence and held-out set are unchanged. G3 recall uses held-out
  conceptual queries; other gates use exact-lookup queries, policy/isolation
  evidence, timings, or separately supplied agent/hybrid measurements. (Follow-up candidate:
  query-side camelCase term splitting for identifier substrings.)
- Copied fixture/test-data content (paths under `fixture(s)`, `testdata`,
  `test-data`, `test_data`, `test_fixtures`, `__fixtures__`, `__snapshots__`)
  stays fully searchable but no longer gains the current-document ranking
  boost, so copied test input can no longer claim current-document authority
  over real sources for ordinary queries. Classification is unchanged, and
  explicit path filters targeting fixtures still work. Query-time only: no
  reindex required.
- Agent retrieval guidance is now shipped in the MCP tool descriptions, the
  CLI help text and the managed `AGENTS.md` snippet (template version bumped
  to 1.2.0): strict lexical matching (every whitespace query term must
  match the same chunk's searchable fields, so broad natural-language
  queries may return zero results even when relevant evidence is indexed —
  retry with shorter identifier/symbol queries and path/role filters,
  bounded), excerpts locate evidence and are not the source (use exact
  reads of source ids); short excerpts do not mean the source is
  unavailable; paginate by passing the response's `next_cursor` value as
  the next request's cursor; empty `related` results do not establish
  missing evidence; fall back to targeted filesystem reads only when
  indexed evidence is insufficient and explain the limitation; keep
  retries bounded. Existing overlays pick up the new snippet with
  `repoise overlay update` (unchanged blocks migrate; user-modified blocks
  report conflicts, user content is preserved).

### Card K6 — package release validation

- The packed-consumer smoke now runs the same offline docs+code fixture (one Markdown doc
  plus TypeScript and JavaScript sources) against both the installed npm launcher and the
  extracted native archive, from outside the source checkout: `init --preset
  docs-code-lexical`, `index`, lexical `search` (doc and code hits), exact `read` back of
  the expected text, `status` and `check --fresh` reporting a fresh index, and a stdio MCP
  session (initialization, initialized notification, tool discovery, search/read/status
  with CLI-identical results, clean shutdown on stdin EOF) that verifies the default server
  hides and refuses `refresh_project_knowledge`.
- Native functional runs use no Node/npm on PATH; the generated cache is isolated per
  install; every subprocess is bounded by timeouts and the temporary tree is removed on
  exit, so broken MCP behavior cannot hang CI.
- The versioned config schema (`schemas/repoise.config.v1.schema.json`) ships in both
  installed forms and is verified against the committed source; tree-sitter parser grammars
  remain compiled into the native binary.
- The smoke runs in the Linux/macOS/Windows CI and release matrices. Fresh
  public-registry install checks passed on macOS ARM64; Linux/Windows
  registry installs remain unmeasured.

### Card K6 — offline lexical evaluation harness

- New reproducible offline evaluation path under `scripts/eval/`: frozen synthetic
  public-safe fixtures (`acme` docs+TypeScript/JavaScript, `docs-site` docs-only,
  `pyrepo` Python fallback) with a SHA-256 file freeze and deterministic replayed git
  history, plus a dirty-worktree phase.
- 60 labeled queries (48 in-tuning, 12 held-out) across exact identifiers, unknown
  concepts, current-vs-plan conflicts, history, code behavior, tests,
  unsupported-language fallback, docs-only, dirty/stale, secret-policy and
  cross-scope categories; gold evidence is path/range/revision.
- `node scripts/eval/harness.mjs` runs the built native CLI per corpus/preset,
  measures cold build, corpus/index sizes and warm search p50/p95 (per CLI
  invocation), validates provenance for every returned hit (path presence and
  SHA-256 content hash at the snapshot revision, line-range bounds, history
  revision/affected-path checks), compares the `docs-only` and `docs-code-lexical`
  presets on the acme corpus, and writes machine-readable `results.json` plus
  `report.md` (in git-ignored `eval-results/`) with hardware, versions and declared
  settings.
- Master-plan section 11 gates are reported as measured-passed, measured-failed or
  unmeasured: exact lookup, policy, held-out recall@5 and warm latency are measured;
  token reduction, code vectors and hybrid latency stay unmeasured — the harness
  never infers token savings from retrieval size and never runs hybrid without an
  operator-configured embedding provider.
- Validated import format for real agent replay evidence
  (`scripts/eval/replay-evidence.schema.json`, imported via
  `--replay-evidence`): fixed model/tasks/revision/budgets and per-run measured model
  input/output tokens, retrieval output tokens, tool count, elapsed time, task
  correctness; derived values and missing task x condition cells are rejected.
- Shared-cache isolation phase: the harness indexes `acme` and `docs-site` into one
  shared cache root, then verifies that foreign source ids are not readable from the
  other scope (both directions, with content inspection), that an in-scope exact read
  returns the exact snapshot text, and that an earlier source id is rejected as stale
  after the indexed file changes; uncovered cases are reported as unmeasured.
- Replay evidence is fail-closed: an invalid or malformed `--replay-evidence` file
  stops the harness with clear diagnostics and a nonzero exit before any indexing work.
  Aggregated token means divide by the summed `sampleSize` (per-sample means); the
  correct rate is labeled task-level; the mean-based reduction is labeled descriptive
  and does not establish the median-reduction gate.
- Harness tests run via `npm test` (`node --test scripts/release/*.test.mjs
  scripts/eval/*.test.mjs`), including regression tests for unequal sample sizes,
  zero-token baselines, task-level correctness, null/duplicate task entries,
  injected isolation leaks and the nonzero-exit import path; the eval JavaScript and
  JSON files are covered by `format`/`format:check` without touching the frozen
  fixture bytes. Runbook at `scripts/eval/RUNBOOK.md` (commands, held-out separation,
  pilot evidence workflow, acceptance targets, rollback).

### Card K6 — build/Actions integration examples

- `docs/build-ci.md` consumer guide for the offline `docs-code-lexical` baseline: pinned
  registry install using the explicitly labeled `REPLACE_WITH_VALIDATED_RELEASE_VERSION`
  placeholder (at that PR the functional release was not yet published; the
  build/CI guide documents the current pin), reviewed schema-version-1 config,
  a generated-cache ignore policy
  (`.repoise/` and `repoise.local.json` stay out of version control), opt-in
  owner-invoked package scripts, and the `index`-then-`check --fresh` sequence with stable
  failure-stop exit codes. It separates the agent's startup refresh (one explicit
  `repoise index` on the real checkout, then optional `watch`) from a separately started
  `watch` loop.
- `examples/ci/repoise-ci.example.yml`: a minimal GitHub Actions `push`/`pull_request`
  example — pinned install, exact checked-out SHA (`${GITHUB_SHA}`),
  `persist-credentials: false`, read-only `contents: read`, an isolated per-run cache
  (`REPOISE_CACHE_DIR`), rebuild-from-scratch with no cache restore or artifact upload,
  `index` then `check --fresh`, no `pull_request_target` and no consumer package scripts.
  It reports the revision and the shallow-history limitation and is an example, not an
  active workflow (it does not live in `.github/workflows/`). `examples/consumer/package.json`
  shows the opt-in scripts; `examples/README.md` indexes both.
- The guide documents that a CI index does not refresh an agent's local checkout and states
  the future cache-compatibility and private-corpus isolation requirements without
  implementing cache/artifact machinery.
- Validated locally (not in GitHub): the documented command sequence was run in a
  disposable synthetic consumer against the current packed package (installed npm launcher
  and the native binary packed into the native archive, no Node on PATH), offline, with an
  isolated cache; `check --fresh` exited `3` before indexing and `0` after. The GitHub
  workflow was not executed and no workflow was activated. This is documentation/examples
  only; it does not mean the live pilot or the master-plan section 11 adoption gates passed.

### Card K6 — stdio MCP server

- `repoise mcp`: stdio MCP server (JSON-RPC 2.0 on stdin/stdout) exposing
  `search_project_knowledge`, `read_project_knowledge`, `related_project_knowledge`
  and `project_knowledge_status` over the same shared services as the CLI, with
  identical versioned response schemas. Read-only by default; stdout carries protocol
  traffic only (diagnostics go to stderr); clean shutdown on stdin EOF.
- Explicitly opt-in `refresh_project_knowledge` tool: enabled by the new
  `mcp.refresh` configuration key (schema v1) or `--allow-refresh`; it runs the same
  bounded, transactional index operation as `repoise index` for the configured scope.
- `read` gained an optional output-token budget with line-boundary truncation and a
  `truncated`/`output_tokens` report (CLI `--max-output-tokens`, default 2500 for
  exact reads).
- Scope-bound server: optional `repoId`/`worktreeId`/`snapshotMode` arguments must
  match the server-configured scope (refresh included); opaque source ids remain
  references, not authorization. Tool arguments are validated against the advertised
  input schemas (required fields, types, enums, numeric bounds,
  `additionalProperties`) before any service call; violations are JSON-RPC
  `-32602` invalid-params errors.
- Client registration guide at `docs/mcp.md` (generic stdio server shape;
  client-specific examples remain unverified).

### Card K4 — offline code indexing

- Offline code indexing: pinned tree-sitter TypeScript/TSX and JavaScript/JSX structural
  chunks with exact source line ranges, enclosing scope/symbol context and labeled
  oversized splits; a deterministic line-window fallback chunker for other code languages.
- Per-generation code symbol records (function/method/class/interface/enum/type-alias
  and more, with parent symbols and export flags) and syntactic reference edges
  (imports resolved against the generation's file set; calls/references as bounded,
  honest-confidence hints).
- FTS lexical search over code chunks with new symbol/context columns; embedding scope
  (`docs` or `docs+code`) selects which corpora receive vectors.
- Index schema v3 with an additive, idempotent migration (column adds guarded, FTS table
  rebuilt only when predating the `context` column); `repoise index` reports symbol and
  reference-edge totals.

## 0.1.0-alpha.0 — 2026-10-04 (native release; npm rollout partial)

- Native Rust CLI scaffold: greeting, help, version and argument errors.
- Thin npm launcher and Linux x64 GNU, macOS ARM64, Windows x64 MSVC binary packaging.
- Manual release preparation, staged npm publishing, native release drafts and SHA-256 checksums.
- MIT license. Indexing, persistence and MCP remain unimplemented.

Native archives and Linux/macOS npm binary packages are public. Windows npm name review and
main npm launcher publication remain pending; see [publishing status](docs/publish/alpha-0-status.md).
