# Repoise

Offline-first repository knowledge for coding agents. Repoise indexes code and
documentation, finds compact source-linked evidence, and reads indexed ranges
back with content-hash validation.

**Functional preview: 0.1.0-alpha.1 is available on npm under `next` and as
[native prerelease archives](https://github.com/billy-the-ape/repoise/releases/tag/v0.1.0-alpha.1).**
This is not stable v1. Pin the exact version: npm `latest` still points to a
nonfunctional placeholder. The older 0.1.0-alpha.0 release is the CLI scaffold.
See the [publication record](docs/publish/alpha-1-status.md) for verification and limits.

## What works today

- Offline lexical search over documentation and code; no model or credentials required.
- Incremental SQLite/FTS5 indexing, scope/freshness checks, and exact source reads.
- TypeScript/JavaScript structural chunks and symbols; line-window fallback for other languages.
- Structural links (references, referenced-by, parent, children), an explicit watch loop, and managed agent guidance.
- CLI and stdio MCP interfaces over the same services; MCP is read-only by default.
- Optional OpenAI-compatible embeddings and bounded Git history retrieval.

Git working trees, committed snapshots, and plain directories are supported.
Embeddings and remote history enrichment are opt-in. Fixture content remains
searchable but receives no current-document ranking boost.

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

From the repository you want to index:

```sh
repoise init --preset docs-code-lexical --agents-snippet --dry-run
repoise init --preset docs-code-lexical --agents-snippet
repoise doctor
repoise explain --path .env
repoise index
repoise check --fresh
repoise search --json --query "YourFunctionName" --role code --max-results 5
```

Review the generated configuration and managed AGENTS.md block. Keep
`.repoise/` and `repoise.local.json` out of version control. The cache may
contain source text; `REPOISE_CACHE_DIR` relocates it. Indexing respects
configured policy and inherited Git ignores; use `explain` to inspect decisions.

Copy a result's `source_id` into an exact read:

```sh
repoise read --json --source-id "SOURCE_ID_FROM_SEARCH" --max-output-tokens 2500
repoise related --json --source-id "SOURCE_ID_FROM_SEARCH"
```

Lexical queries require every whitespace term to match the same chunk's
searchable fields. Start with identifiers or short terms, refine with
`--path-filter` and `--role`, and read relevant results. Excerpts are previews;
zero results do not prove that evidence is absent. A response's `next_cursor`
can be passed as the next search's `--cursor`.

## Use with an agent

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
