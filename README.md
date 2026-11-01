# Repoise

Offline-first repository knowledge for coding agents. Repoise indexes code and
documentation, finds compact source-linked evidence, and reads indexed ranges
back with content-hash validation.

**Functional preview: 0.1.0-alpha.1 is available on npm under `next` and as
[native prerelease archives](https://github.com/billy-the-ape/repoise/releases/tag/v0.1.0-alpha.1).**
This is not stable v1. npm `latest` and `next` both resolve to 0.1.0-alpha.1,
so default installs now select the functional preview. Pin the exact version
for reproducible automation. The older 0.1.0-alpha.0 release is the CLI scaffold.
See the [publication record](docs/publish/alpha-1-status.md) for verification and limits.

## What works today

- Offline lexical search over documentation and code; no model or credentials required.
- Incremental SQLite/FTS5 indexing, scope/freshness checks, and exact source reads.
- TypeScript/JavaScript structural chunks and symbols; line-window fallback for other languages.
- Cross-worktree reuse: linked Git worktrees of one local repository reuse compatible parse artifacts and embedding vectors from a shared, versioned family store.
- Structural links (references, referenced-by, parent, children), an explicit watch loop, and managed agent guidance.
- CLI and stdio MCP interfaces over the same services; MCP is read-only by default.
- Optional OpenAI-compatible embeddings and bounded Git history retrieval.
- Suite-driven agent benchmark runner (`repoise bench`: disposable worktrees,
  frozen snapshots, declared retrieval conditions, AP-03 agent adapters).

Git working trees, committed snapshots, and plain directories are supported.
Embeddings and remote history enrichment are opt-in. Fixture content remains
searchable but receives no current-document ranking boost.

## Quick start

From the repository you want to index (Node >=22.14.0 for the npm install):

```sh
npm install --save-dev --save-exact --ignore-scripts repoise@0.1.0-alpha.1
npx --no-install repoise init --preset docs-code-lexical --agents-snippet
npx --no-install repoise doctor
npx --no-install repoise index
npx --no-install repoise check --fresh
npx --no-install repoise search --json --query "YourFunctionName" --role code --max-results 5
```

Review the generated configuration and managed AGENTS.md block. Keep
`.repoise/` and `repoise.local.json` out of version control. The cache may
contain source text; `REPOISE_CACHE_DIR` relocates it. Indexing respects
configured policy and inherited Git ignores; use `explain` to inspect
decisions.

Copy a result's `source_id` into an exact read:

```sh
npx --no-install repoise read --json --source-id "SOURCE_ID_FROM_SEARCH" --max-output-tokens 2500
npx --no-install repoise related --json --source-id "SOURCE_ID_FROM_SEARCH"
```

Lexical queries require every whitespace term to match the same chunk's
searchable fields. Start with identifiers or short terms, refine with
`--path-filter` and `--role`, and read relevant results. Excerpts are previews;
zero results do not prove that evidence is absent. A response's `next_cursor`
can be passed as the next search's `--cursor`.

## Install the preview

For a project-local installation (Node >=22.14.0):

```sh
npm install --save-dev --save-exact --ignore-scripts repoise@0.1.0-alpha.1
npx --no-install repoise version
```

Do not omit optional dependencies: they contain the platform executable.
Examples below use `repoise` as shorthand; with a project-local installation,
invoke it as `npx --no-install repoise`. Native archives require no Node/npm.
Fresh public-registry CLI and MCP checks passed on macOS ARM64; all three
targets passed the offline packaged-consumer smoke in the release workflow.

## Build from source

With [rustup](https://rustup.rs/) installed, build from a checkout of this repository.
The toolchain is pinned to Rust 1.99.0; Git is required for Git repositories.
The native executable needs no Node/npm.

```sh
bash scripts/build.sh
export PATH="$PWD/target/release:$PATH"
repoise version
```

On Windows, use `./scripts/build.ps1` and the resulting
`target/release/repoise.exe`. See [development.md](docs/development.md)
for local installation and checks.

## CLI commands

Exit codes: 0 success, 1 operational failure, 2 usage error, 3 the index is
stale or missing for search/read/status (and `bench` preflight/suite
failure), 4 coverage not met for `check` (and `bench` interruption).

Global options accepted by most commands: `--json` (machine-readable output),
`--config <PATH>`, `--local-config <PATH>`, `--max-file-bytes <N>`, and
`--committed` (use the committed Git snapshot instead of the working tree).
Most commands accept an optional positional `[ROOT]` — the repository path.
Examples use `repoise` as shorthand; for a project-local npm install, invoke
it as `npx --no-install repoise`.

### Setup and diagnostics

- `repoise init [OPTIONS] [ROOT]` — configure the repository
  non-interactively; writes the config and, optionally, the managed
  agent-guidance block. Options: `--preset <docs-only|docs-code-lexical|hybrid>`,
  `--provider <NAME>` (required for hybrid), `--agents-snippet`,
  `--adopt-managed-block <FILE>`, `--dry-run`, `--yes`.

  ```sh
  repoise init --preset docs-code-lexical --agents-snippet --dry-run
  repoise init --preset hybrid --provider openai --agents-snippet
  ```

- `repoise doctor [ROOT]` — show the effective settings, selected snapshot
  mode, config paths, and capabilities.

  ```sh
  repoise doctor --json
  ```

- `repoise explain --path <REL> [ROOT]` — explain the include/exclude policy
  decision for a repository path.

  ```sh
  repoise explain --path .env
  ```

- `repoise status [ROOT]` — show scope, snapshot, index state, and freshness.

  ```sh
  repoise status --json
  ```

- `repoise check [OPTIONS] [ROOT]` — offline coverage check with stable exit
  codes (0 ok, 1 error, 2 usage, 3 missing/stale, 4 coverage not met);
  `--fresh` also fails when the index is stale.

  ```sh
  repoise check --fresh
  ```

### Indexing and maintenance

- `repoise index [OPTIONS] [ROOT]` — build and publish the offline index
  incrementally; use `--committed` to index the committed snapshot.
  It reports live phase progress (discovery, reconciliation, optional
  embedding and history enrichment, publication) with counts, elapsed time, and
  an approximate ETA only when the observed rate is reliable. With `--json`,
  progress goes to stderr so stdout stays a single JSON document. With
  `--events`, stdout becomes an NDJSON event stream (started, progress,
  warning, heartbeat, and one terminal event); see
  [docs/automation.md](docs/automation.md).

  ```sh
  repoise index
  repoise index --committed
  repoise index --events
  ```

- `repoise watch [OPTIONS] [ROOT]` — incremental watch loop; stays running
  until cancelled (SIGINT/SIGTERM on Unix, Ctrl+C on Windows; the last
  complete generation is retained) and is not an installed background daemon.
  Options: `--interval <MS>` (default 500),
  `--debounce <MS>` (default 200), `--max-pending <N>` (default 4096).
  `--events` streams the same NDJSON protocol as `index --events`.

  ```sh
  repoise watch --interval 500 --debounce 200
  repoise watch --events
  ```

- `repoise capabilities [OPTIONS]` — version/capabilities handshake for
  automation: event-stream protocol and schema, build features, command
  machine-output matrix, benchmark availability, stable exit codes, and the
  compatibility policy (`--json` for the machine document).

  ```sh
  repoise capabilities --json
  ```

- `repoise purge` — remove generated cache data: `--all` removes everything
  under the cache root (owned scopes are skipped and reported, because a
  writer holds each scope's lock); `--repo-id <ID> --worktree-id <ID>`
  removes one scope directory.

  ```sh
  repoise purge --all
  repoise purge --repo-id <ID> --worktree-id <ID>
  ```

### Retrieval

- `repoise search --query <Q> [OPTIONS] [ROOT]` — offline lexical search over
  the current index. Options: `--path-filter <GLOB>`, `--role <ROLE>`,
  `--max-results <N>` (default 5, cap 20), `--max-output-tokens <N>`,
  `--cursor <TOKEN>` (from a previous page's `next_cursor`),
  `--mode <lexical|hybrid|vectors-only>` (default lexical), `--rrf-k <N>`
  (hybrid mode), `--lane <search|history>` (default search).

  ```sh
  repoise search --json --query "YourFunctionName" --role code --max-results 5
  repoise search --json --query "auth token" --path-filter "src/**"
  repoise search --json --lane history --query "error handling"
  ```

- `repoise read --source-id <ID> [OPTIONS] [ROOT]` — exact read-back of a
  search result with content-hash validation; `--max-output-tokens <N>`
  bounds the output.

  ```sh
  repoise read --json --source-id <ID> --max-output-tokens 2500
  ```

- `repoise related --source-id <ID> [OPTIONS] [ROOT]` — follow structural
  links from a source; `--relation <KIND>` accepts `references`,
  `referenced-by`, `children`, `parent` (repeatable; all when omitted).

  ```sh
  repoise related --json --source-id <ID> --relation references
  ```

### Interfaces and agent overlay

- `repoise mcp [OPTIONS] [ROOT]` — stdio MCP server (JSON-RPC 2.0 on
  stdin/stdout); read-only by default. `--allow-refresh` exposes the opt-in
  refresh tool. See [MCP](docs/mcp.md).

  ```sh
  repoise mcp /absolute/path/to/target-repository
  ```

- `repoise overlay <uninstall|update> [ROOT]` — remove, or 3-way-migrate, the
  managed agent-guidance overlay; `--dry-run` reports changes without writing.

  ```sh
  repoise overlay update
  repoise overlay uninstall --dry-run
  ```

- `repoise greet` — print the project greeting.
- `repoise help` / `repoise version` — usage and version information.

### Benchmarks

- `repoise bench [OPTIONS] <SUITE>` — run a suite-driven agent benchmark:
  repeated trials over a frozen snapshot in disposable worktrees, against
  declared retrieval conditions, driven through the AP-03 agent adapter
  protocol (the optional Node host bridge). Completed trials are graded by
  the task's declared evaluator (AP-05) and the published report
  (`report.json`/`report.md`) carries grading counts, predeclared paired
  comparisons and the predeclared quality gate; `bench report`
  re-grades recorded trials and rebuilds the report offline. `--dry-run`
  inspects the frozen plan without launching agents; `--json` machine
  output. Exit codes: 0 completed, 1 operational failure, 2 usage, 3
  preflight/suite failure, 4 interrupted. A source-checkout build needs no
  environment variables; installed npm/native packages do not ship the
  host bridge and need `REPOISE_BENCH_HOST_BRIDGE`. See [bench](docs/bench.md).

  ```sh
  repoise bench --dry-run /abs/path/to/suite.json
  repoise bench /abs/path/to/suite.json --output /tmp/bench-out
  repoise bench report --experiment my-experiment --output /tmp/bench-out
  ```

## Use with an agent

Install the Repoise skill for Claude Code, Codex, Cursor, and other supported
agents from the repository where you will use it:

```sh
npx skills@latest add billy-the-ape/repoise
```

The installer lets you select agents and installation scope. The skill teaches
bounded CLI/MCP retrieval and how to keep changed code, documentation, and tests
useful for indexing. It installs guidance; install the Repoise executable and
configure MCP separately if needed. See [skill setup and updates](docs/agents.md#install-the-agent-skill).

**Terminal access plus AGENTS.md guidance is enough for CLI use.** The client
does not need Repoise-specific code. The executable must be available to the
agent, and commands must target its actual checkout. Whether a client loads
AGENTS.md automatically depends on that client.

Alternatively, register a stdio MCP server in your client's MCP settings:

```json
{
  "mcpServers": {
    "repoise": {
      "command": "/absolute/path/to/repoise",
      "args": ["mcp", "/absolute/path/to/target-repository"]
    }
  }
}
```

The server exposes search, read, related, and status tools. This is a generic
configuration shape; client-specific setup varies. MCP is bound to the root
and snapshot mode selected at launch. An agent in another worktree needs a
server targeting that worktree. Detached HEAD itself is not the limitation.

See [agent setup](docs/agents.md) for startup, refresh permissions, worktrees,
and managed guidance upgrades; see [MCP](docs/mcp.md) for the protocol.

## Keep the index fresh

Run `repoise index` after relevant changes, or start `repoise watch` in a
separate terminal. Watch stays running until Ctrl+C and retains the last
complete generation on cancellation. It is not an installed background daemon.
Use `repoise check --fresh` before relying on updated evidence.

For a committed Git snapshot, use `--committed` consistently for indexing and
retrieval. Uncommitted edits are outside that snapshot. For active coding,
the default working-tree mode includes eligible edits.

## Guides and validation

- [Build/CI integration](docs/build-ci.md)
- [Architecture](docs/architecture.md) and [storage](docs/storage.md)
- [Development](docs/development.md) and [contributor instructions](AGENTS.md)
- [Publishing](docs/publish/README.md) and [changelog](CHANGELOG.md)
- [Evaluation runbook](scripts/eval/RUNBOOK.md) and [v1 plan](docs/plans/v1_master_plan.md)

The offline evaluation harness uses 60 labeled synthetic queries, including
12 held-out queries, and reports provenance, policy, retrieval, and timing
measurements. Real agent token savings and hybrid gains are not established
by the initial pilot. See the runbook for measured versus unmeasured gates.

MIT licensed.
