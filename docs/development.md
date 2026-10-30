# Development and native builds

## Requirements

Install [rustup](https://rustup.rs/). `rust-toolchain.toml` pins Rust 1.99.0 with rustfmt and
Clippy; `rust-version = "1.99"` is the initial MSRV. This is the stable release dated
2026-10-01. Change the pin and MSRV deliberately together until a broader compatibility
policy is tested. Use Edition 2024 and workspace resolver 3.
No Node, Git installation, credentials, embedding model or runtime environment variables
are required to run this scaffold. Git must be available only when operating on a Git
repository (the Git adapter shells out to `git` with argument arrays). First toolchain
installation requires network access; subsequent builds are offline-capable once tools are
installed. `rusqlite` uses the bundled SQLite build, so no system SQLite is needed.

## Local workflow

```sh
cargo run --package repoise-cli -- --help
cargo fmt --all
bash scripts/check.sh
bash scripts/build.sh
./target/release/repoise
```

On Windows use `./scripts/check.ps1`, `./scripts/build.ps1` and
`./target/release/repoise.exe` in PowerShell. The wrappers work from any current directory.
Checks enforce formatting, Clippy without warnings, workspace tests and rustdoc without warnings.
Cargo.lock is committed; builds/tests use `--locked`. Workspace crates inherit package metadata
and lints. Unsafe code is forbidden until an explicitly reviewed boundary requires a policy change.
Use idiomatic ownership, explicit error propagation and bounded work; do not add async runtimes
or micro-optimizations until a feature and measurements require them. Code chunking relies on
the pinned, offline tree-sitter TypeScript/TSX and JavaScript/JSX grammars; new grammars
join only behind the `chunk::code` boundary with their own version, fixtures and license
review.

## CLI commands

The native CLI works without Node/npm. Commands (all accept an optional positional
root; `purge` operates on the current directory's resolved cache root):

```sh
repoise doctor [ROOT]                 # effective settings, capabilities, policy summary
repoise explain --path <REL> [ROOT]   # explainable include/exclude decision for one path
repoise index [ROOT]                  # build and publish the offline index (incremental)
repoise status [ROOT]                 # scope, snapshot, index and freshness (exit 3 when missing/stale)
repoise search --query <Q> [ROOT]     # offline lexical search over the current index
repoise read --source-id <ID> [ROOT]  # exact read-back of a search result
repoise related --source-id <ID> [ROOT]  # structural links from a source chunk
repoise check [OPTIONS] [ROOT]        # offline coverage/freshness check (stable exit codes)
repoise init [ROOT]                   # idempotent, non-interactive configuration
repoise watch [OPTIONS] [ROOT]        # incremental watch loop (SIGINT/SIGTERM stop it)
repoise overlay <uninstall|update> [ROOT]  # manage the agent-guidance overlay
repoise purge                         # remove generated cache data (--all or one scope)
repoise mcp [ROOT]                    # stdio MCP server (JSON-RPC 2.0; --allow-refresh to expose refresh)
repoise capabilities                 # machine handshake: version, event stream, features, exit codes
repoise greet | help | version
```

Global options: `--json` (machine-readable output), `--config <PATH>` and
`--local-config <PATH>` (explicit config locations), `--max-file-bytes <N>`,
`--committed` (use the committed Git snapshot instead of the working tree),
`--events` (NDJSON event stream on stdout for `index` and `watch`; see
[automation.md](automation.md)).
Search options: `--path-filter <GLOB>`, `--role <ROLE>`, `--max-results <N>`
(default 5, cap 20), `--max-output-tokens <N>`, `--cursor <TOKEN>`,
`--mode <lexical|hybrid|vectors-only>` (default lexical), `--rrf-k <N>`,
`--lane <search|history>` (default search).
Check options: `--fresh` (also fail when the index is stale).
Purge options: `--all` or both `--repo-id <ID>` and `--worktree-id <ID>`.
Init options: `--preset docs-only|docs-code-lexical|hybrid`, `--provider <NAME>`
(required for hybrid), `--agents-snippet`, `--adopt-managed-block <FILE>`,
`--dry-run`, `--yes`.
Unsupported arguments exit 2 with the usage on stderr; I/O and service failures
exit 1; a missing or stale index exits 3 for `status`, `search` and `read`.

Easy fixture path (no credentials, no network):

```sh
tmp=$(mktemp -d) && printf 'hello\n' > "$tmp/README.md"
repoise init --preset docs-only --adopt-managed-block README.md "$tmp"
repoise doctor "$tmp"
repoise explain --path .env "$tmp"     # excluded by secret deny
repoise index "$tmp"                   # chunk, store and publish the index
repoise status "$tmp"                  # freshness: fresh (exit 0)
repoise search --query hello "$tmp"    # offline lexical hit with source id
repoise read --source-id <ID> "$tmp"   # exact text with hash validation
```

The workspace test suite covers these behaviors end to end:
`cargo test --workspace --locked` (contract fixtures live in
`crates/repoise-core/tests/v1_0.rs`, `crates/repoise-core/tests/v1_1.rs` and
`crates/repoise-cli/tests/cli.rs`, `crates/repoise-cli/tests/mcp.rs` for the stdio MCP
server; secret fixtures are generated in memory and
never stored in Git). See [storage.md](storage.md) for the persistent index layout and
[mcp.md](mcp.md) for the MCP server.

## Optional hybrid search

`search --mode hybrid|vectors-only` and `index` use an embedding provider only when
the effective config has a complete profile: `embedding.provider`, `model`,
`dimensions`, `endpoint` (must be an `env:` reference) and `apiKeyEnv`. Endpoint and
API-key values resolve from the referenced environment variables at runtime and never
enter config or caches; missing values, timeouts, partial batches and invalid responses
degrade to the lexical baseline with an explicit note. Budgets (`batchSize`,
`timeoutMs`, `maxRetries`, `maxRequestsPerBuild`, `maxInputCharsPerBuild`) and
`search.rrfK` are configurable. The HTTPS transport is built into the CLI behind the
default `remote-embedding` feature; `cargo check -p repoise-cli --no-default-features`
produces the lexical-only binary. `doctor` additionally warns (without failing) when
`endpoint` resolves to a non-TLS `http://` remote host: source text and the API key
would travel in cleartext.

### Model input profiles

`embedding.inputProfile` selects the explicit, versioned input template for the
selected endpoint: `raw` (default — chunk and query text pass through unchanged, the
generic OpenAI-compatible behavior) or `embeddinggemma-2` (task/template formatting
for the local Google EmbeddingGemma 2 endpoint served by llama.cpp). A template
version can be pinned explicitly (`raw:2`, `embeddinggemma-2:1`); the profile is
never inferred from the model name, because local serving aliases may vary.

With `embeddinggemma-2` the exact inputs are:

- Document chunks: `title: {title} | text: {content}`, where the title is the
  heading when available, else the file name, else the repository path.
- Code chunks: `title: {path} | text: symbol: {symbol} | context: {context} | code: {content}`,
  including the existing symbol and context metadata only where available.
- Document-search queries: `task: search result | query: {query}`.
- Code-search queries: `task: code retrieval | query: {query}`.

For an unfiltered hybrid query both query vectors are produced in one provider batch:
the document-search vector ranks non-code chunks and the code-retrieval vector ranks
code chunks, and the two corpus-scoped cosine ranks are each fused with the lexical
rank using the standard RRF approach. A request filtered to one corpus computes only
the relevant query vector. The input-profile identity is folded into the embedding
fingerprint and the embedding-cache key is the exact formatted input, so changing the
title/context formatting or the profile never reuses or compares vectors produced
under a different template. The initial EmbeddingGemma 2 support uses 768 dimensions;
output-dimension truncation is not claimed because the serving adapter must return
and normalize the selected dimension correctly. Code chunks are embedded only when
`embedding.scope` is `all` (docs and code).

### Local EmbeddingGemma 2 endpoint (llama.cpp)

EmbeddingGemma 2 ships as a GGUF embedding model; serve it with llama.cpp's
`llama-server` in OpenAI-compatible mode. Download the 2B instruct GGUF (Q8_0) from
the [ggml-org EmbeddingGemma-2b-it-GGUF model card](https://huggingface.co/ggml-org/EmbeddingGemma-2b-it-GGUF),
then start a local endpoint (768 dimensions, no API key required locally — a
placeholder value still satisfies the transport's header):

```sh
llama-server \
  --model /path/to/EmbeddingGemma-2b-it-Q8_0.gguf \
  --embedding \
  --port 8080
export EMBEDDING_ENDPOINT=http://127.0.0.1:8080
export EMBEDDING_API_KEY=local
```

Point the repo config at the endpoint with the explicit input profile:

```json
{
  "schemaVersion": 1,
  "embedding": {
    "provider": "openai-compatible",
    "scope": "all",
    "inputProfile": "embeddinggemma-2:1",
    "endpoint": "env:EMBEDDING_ENDPOINT",
    "model": "EmbeddingGemma-2b-it",
    "dimensions": 768,
    "apiKeyEnv": "EMBEDDING_API_KEY"
  }
}
```

Manual smoke procedure: with the endpoint running, run `repoise index`, then
`repoise status` (vector coverage should report every in-scope chunk vectorized),
then `repoise search --mode hybrid "how do I configure the indexer"` — hybrid results
carry RRF explanations attributing lexical and corpus-scoped vector ranks. A stopped
endpoint degrades the same search to the lexical baseline (the response reports
`lexical`) without failing.

## Optional history lane

`index` records a bounded local history lane only when the effective config sets
`history.enabled` (default false). The lane covers first-parent (mainline) commits
within `history.horizon` (default 500), with bounded changed-path summaries
(`history.maxPathsPerCommit`, default 20) and optional bounded diff hunk
descriptors (`history.diffHunks`, default false). Shallow clones and horizon cuts
are reported as explicit gaps, never silently omitted. Git is required for the
history lane (tested against Git 2.43+); the adapter deliberately avoids
output framing that changed between Git versions (whitespace-split `rev-list`
instead of `-z`, NUL framing only where Git guarantees it), so older packaged
Gits cannot silently empty the lane. History publishes in the
same generation as the docs/code index but is a separate search lane:
`search --lane history` ranks history items (message, revision, affected paths,
host metadata) with FTS5/BM25; the default lane never returns history items, and
`read` does not accept history items. `status` reports the published lane state
(on/off, item count, head revision, gaps, enrichment stops), and each history
search response carries the same lane coverage; enrichment items skipped by a
budget or early stop are counted as unattempted rather than left silent.

GitHub PR enrichment is opt-in inside the lane via `history.enrichment`
(`host: "github"`, `tokenEnv` (required, a valid environment variable name),
`maxPrsPerBuild`, `maxRequestsPerBuild`, `maxBodyChars`). Commit-message `#123`
references stay unverified hints until the host API confirms the commit belongs
to that change request (the commit's PR list is fetched once per commit,
bounded and paginated, and matched against the hints); only then is a
verified association (title, bounded body, state, permalink, fetched time)
stored. The token is read from the named environment variable at runtime and
never enters config or caches; a missing or empty token disables enrichment
entirely — no unauthenticated requests are sent — without affecting local
indexing. The transport is built into the CLI behind the default
`github-enrichment` feature (`ureq` read-only requests to `api.github.com`);
`cargo check -p repoise-cli --no-default-features` produces the no-enrichment
binary. Host responses are ETag-cached in a repository-scoped JSON cache under
the cache root (removed by `purge --all`), requests are bounded per build, rate
limits (primary and secondary/`Retry-After`) stop the enrichment pass, and a
permission denial fails closed by invalidating the cached remote content.

## Installation and artifacts

`bash scripts/install.sh` (PowerShell: `./scripts/install.ps1`) uses `cargo install --path`
to install into Cargo's normal bin directory; add it to PATH. Optional Cargo install arguments
are forwarded, for example `--root /tmp/repoise-install`. Existing installs are replaced only
when Cargo permits it; explicitly pass `--force` when replacement is intended.
Uninstall with `cargo uninstall repoise-cli`, using the same `--root` if customized.
This is local development installation, not a registry release.

Rust-native checks/builds need no Node. Release, evaluation and CI-selection
tooling additionally use Node 24 in CI, with pnpm (pinned by the root
`packageManager` field and committed `pnpm-lock.yaml`) and pinned Prettier and
Turborepo dev dependencies; install using `pnpm install --frozen-lockfile
--ignore-scripts`. The Prettier globs and `pnpm test` (`node --test`) cover
scripts/release, scripts/eval and scripts/ci, plus the npm wrapper sources,
the workspace packages under `packages/` (agent adapter protocol and Cline
adapter), the root and launcher manifests, `turbo.json`, `pnpm-workspace.yaml`
and workflow YAML. Run `pnpm run format` before committing those changes, then
`pnpm run format:check` and `pnpm test`. Turbo tasks (`pnpm exec turbo run
format:check|test|check:native|build:native`, plus the `:windows` variants)
orchestrate the same steps; the adapter packages' `test` scripts
(`node --test test/*.test.mjs`) join the root `test` task through Turbo, so
CI (`pnpm exec turbo run test` in the Check jobs) runs them too. The native
bridge tasks run single-threaded per platform with Turbo output caching
disabled. The offline lexical evaluation
harness runs separately: cargo build --package repoise-cli, then
node scripts/eval/harness.mjs (results in the git-ignored eval-results/
directory); see the [eval runbook](../scripts/eval/RUNBOOK.md).
These development dependencies are not installed by consumers of the published launcher.
Git attributes keep text checkouts at LF on every platform, including Windows.

CI checks/tests/builds/smokes Linux, macOS and Windows. Pull requests select
affected check sets through `scripts/ci/impact-map.json` and
`scripts/ci/select-checks.mjs`; pushes to main, unknown paths, empty diffs and
selection failures run the full suite, and an aggregate `CI gate` job enforces
the expected results (see the
[agent platform workspace plan](plans/complete/agent-platform/01-workspace-and-ci.md)).
The manually dispatched
**Build native artifacts** workflow produces binaries for Linux x86_64 GNU, macOS ARM64 and
Windows x86_64 MSVC, retained for seven days. These are engineering artifacts: no installer,
checksums/signing, compatibility guarantee, npm publication or GitHub release yet.
Artifact targets are an initial test matrix, not the final distribution support policy.
No server deployment or secrets are required. Both Cargo packages have `publish = false`;
MIT licensing is selected. Registry ownership and release activation remain separate steps.
See [release guide](releases.md) for staged npm publishing and native archive workflows.

## Current references

- [Rust 1.99 release](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/)
- [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)
- [Rust 2024 edition](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)
- [rustup toolchain files](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)

Keep this guide and [AGENTS.md](../AGENTS.md) aligned with CI and actual commands.
