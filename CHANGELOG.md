# Changelog

## Unreleased (card K4)

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
