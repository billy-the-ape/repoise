# Changelog

## Unreleased (cards K4–K6)

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
