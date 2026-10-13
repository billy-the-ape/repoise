# Changelog

## Unreleased (cards K4–K6)

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
- The smoke runs in the Linux/macOS/Windows CI and release matrices. The public registry
  install path remains unvalidated pending the registry name review.

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
