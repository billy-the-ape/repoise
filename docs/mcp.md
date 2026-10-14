# MCP server (stdio)

Repoise ships a stdio Model Context Protocol (MCP) server as a thin adapter over
the same shared Rust services the CLI uses: `repoise mcp` serves
`search_project_knowledge`, `read_project_knowledge`, `related_project_knowledge`
and `project_knowledge_status`, plus the explicitly opt-in
`refresh_project_knowledge`. Every tool call runs the same versioned service
responses as `repoise search --json`, `read --json`, `related --json` and
`status --json`, so agent results are identical through either interface.

The server is **read-only by default**: the refresh tool is exposed only when the
operator explicitly enables it (config `mcp.refresh` or `--allow-refresh`).
Refresh runs the same bounded, transactional index operation as `repoise index`
for the configured scope; it never executes repository scripts and changes only
the generated cache.

## Launch

```sh
repoise mcp [ROOT]                  # stdio MCP server (JSON-RPC 2.0)
repoise mcp --allow-refresh [ROOT]  # additionally expose refresh_project_knowledge
```

`ROOT` defaults to the current directory; the server binds at startup to that
configured scope (repository root + snapshot mode) and never reads credentials
from the environment. All diagnostics (startup line, parse errors, shutdown) go
to **stderr**; stdout carries MCP protocol traffic only (newline-delimited
JSON-RPC 2.0 messages). When the client closes stdin, the server flushes and
exits with code 0.

## Tools

| Tool | Service | Notes |
| --- | --- | --- |
| `search_project_knowledge` | lexical (or hybrid) search | `query` required; optional `pathFilter`, `role`, `maxResults` (default 5, cap 20), `maxOutputTokens` (estimated, default 1200), `cursor`, `mode` (`lexical` default, `hybrid`, `vectors-only`), `rrfK` |
| `read_project_knowledge` | exact read-back | `sourceId` required; expected-hash validation against the live snapshot; `maxOutputTokens` (estimated, default 2500) truncates at a line boundary and marks the result `truncated` |
| `related_project_knowledge` | structural/section links | `sourceId` required; optional `relations` (`references`, `referenced-by`, `children`, `parent`), `limit`, `maxOutputTokens` |
| `project_knowledge_status` | operator status | published index, live snapshot, freshness, coverage, cache, last error |
| `refresh_project_knowledge` | incremental index publication | opt-in only; identical outcome to `repoise index` |

All tools accept the optional scope-verification arguments `repoId`,
`worktreeId` and `snapshotMode`; a provided value that does not match the
server's configured scope is rejected with a `scope mismatch` error. Opaque
source ids are references, not authorization: every operation rechecks the
configured scope, and ids from other scopes fail with a clean diagnostic.

## Retrieval workflow

The tool descriptions carry bounded guidance for agents; the CLI help text
(`repoise help`) states the equivalent rules. Semantics:

- **Excerpts locate evidence, they are not the source.** Search results
  carry short excerpts (budgeted by `maxOutputTokens`). A short excerpt is
  not evidence that the indexed source is unavailable; use
  `read_project_knowledge` on a relevant `sourceId` to inspect the exact
  implementation.
- **Strict lexical matching.** In lexical mode every whitespace query term
  must match the same chunk's searchable fields (FTS5 `AND` over per-field
  matches). Broad natural-language queries whose terms are scattered across
  chunks may therefore return zero results even when relevant evidence is
  indexed; for empty or irrelevant results, try a shorter identifier/symbol
  query and appropriate `pathFilter`/`role` filters before concluding the
  indexed evidence is insufficient. Focused queries (one identifier, one
  file) are the reliable pattern.
- **Truncation has one meaning.** `truncated: true` on a `read` result means
  the exact text exceeded `maxOutputTokens` and was cut at a line
  boundary; `truncated: true` on a `search` result marks remaining results,
  fetched by passing the response's `next_cursor` value as the next request's
  `cursor`. Neither is a sign that indexed content is missing.
- **Empty `related` results** mean no indexed relations exist for that
  source; they do not establish that source evidence is missing.
- **Bounded fallback.** When indexed evidence remains insufficient, fall
  back to targeted filesystem reads and state the actual limitation. Keep
  attempts bounded: do not repeat poor searches, and do not call Repoise
  when sufficient evidence is already available.

Errors follow the MCP conventions: protocol failures are JSON-RPC errors
(parse `-32700`, method not found `-32601`), and tool-argument validation
failures are JSON-RPC invalid-params errors (`-32602`). Tool arguments are
validated against the advertised input schemas — required fields, types,
enum values, numeric bounds and `additionalProperties: false` — before any
service is invoked; scope mismatches and unknown tools are also rejected as
`-32602`. Service-level failures (stale source, missing index, unknown
source id, disabled refresh, cursor bound to a different query) are tool
results with `isError: true` and a diagnostic text. Cursor tokens are bound
to their exact query and filters, and `read` failures report
`stale source (...)` with the reason (content hash changed since the
index) — never a panic.

## Client registration

Register the server with any MCP client that supports stdio servers, pointing
the command at the `repoise` binary with `mcp` (and the repository root):

```json
{
  "mcpServers": {
    "repoise": {
      "command": "repoise",
      "args": ["mcp", "/path/to/repo"]
    }
  }
}
```

To allow the refresh tool from the client, add the flag:

```json
{ "command": "repoise", "args": ["mcp", "--allow-refresh", "/path/to/repo"] }
```

The snippet above is the generic stdio server shape (the MCP protocol does not
define a client-specific config schema); it is a template, not compatibility
tested against a specific client. No Node/npm, ai-gateway or credentials are
required.

## Configuration

Standard Repoise configuration applies (`repoise.config.json`,
`repoise.local.json`, `REPOISE_CACHE_DIR`); the MCP server reads the same effective
config as the CLI. The optional `mcp` block (see
`schemas/repoise.config.v1.schema.json`):

```json
{ "mcp": { "refresh": true } }
```

`mcp.refresh` (default false) enables the opt-in refresh tool. Embedding
settings work as in the CLI (opt-in, `env:` credential references); without them
the server stays lexical-only, and remote history enrichment is not available
from the MCP server in this build (the history lane remains a CLI feature).

## Validation

```sh
cargo test --package repoise-cli --test mcp --locked
```

The acceptance suite covers protocol initialization, tool discovery/calls,
CLI/MCP result equivalence, argument validation (typed filters/scope arguments,
unknown properties, numeric bounds, mismatched refresh scope with no generation
published), stale/hash errors, scope enforcement, refresh permissions (flag
and config), cursor binding and clean shutdown on stdin EOF. See
[AGENTS.md](../AGENTS.md) and [development.md](development.md) for the
full check list.
