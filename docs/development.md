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
repoise init [ROOT]                   # idempotent, non-interactive configuration
repoise purge                         # remove generated cache data (--all or one scope)
repoise greet | help | version
```

Global options: `--json` (machine-readable output), `--config <PATH>` and
`--local-config <PATH>` (explicit config locations), `--max-file-bytes <N>`,
`--committed` (use the committed Git snapshot instead of the working tree).
Search options: `--path-filter <GLOB>`, `--role <ROLE>`, `--max-results <N>`
(default 5, cap 20), `--max-output-tokens <N>`, `--cursor <TOKEN>`,
`--mode <lexical|hybrid|vectors-only>` (default lexical), `--rrf-k <N>`.
Purge options: `--all` or both `--repo-id <ID>` and `--worktree-id <ID>`.
Init options: `--preset docs-only|docs-code-lexical|hybrid`, `--provider <NAME>`
(required for hybrid), `--adopt-managed-block <FILE>`, `--dry-run`, `--yes`.
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
`crates/repoise-cli/tests/cli.rs`; secret fixtures are generated in memory and
never stored in Git). See [storage.md](storage.md) for the persistent index layout.

## Optional hybrid search

`search --mode hybrid|vectors-only` and `index` use an embedding provider only when
the effective config has a complete profile: `embedding.provider`, `model`,
`dimensions`, `endpoint` (must be an `env:` reference) and `apiKeyEnv`. Endpoint and
API-key values resolve from the referenced environment variables at runtime and never
enter config or caches; missing values degrade to the lexical baseline with an
explicit note. Budgets (`batchSize`, `timeoutMs`, `maxRetries`,
`maxRequestsPerBuild`, `maxInputCharsPerBuild`) and `search.rrfK` are configurable.
The HTTPS transport is built into the CLI behind the default `remote-embedding`
feature; `cargo check -p repoise-cli --no-default-features` produces the lexical-only
binary. `doctor` additionally warns (without failing) when `endpoint` resolves to a
non-TLS `http://` remote host: source text and the API key would travel in cleartext.

## Optional history lane

`index` records a bounded local history lane only when the effective config sets
`history.enabled` (default false). The lane covers first-parent (mainline) commits
within `history.horizon` (default 500), with bounded changed-path summaries
(`history.maxPathsPerCommit`, default 20) and optional bounded diff hunk
descriptors (`history.diffHunks`, default false). Shallow clones and horizon cuts
are reported as explicit gaps, never silently omitted. History publishes in the
same generation as the docs/code index but is a separate search lane:
`search --lane history` ranks history items (message, revision, affected paths,
host metadata) with FTS5/BM25; the default lane never returns history items, and
`read` does not accept history items. `status` reports the published lane state
(on/off, item count, head revision, gaps, enrichment stops), and each history
search response carries the same lane coverage; enrichment items skipped by a
budget or early stop are counted as unattempted rather than left silent.

GitHub PR enrichment is opt-in inside the lane via `history.enrichment`
(`host: "github"`, `tokenEnv`, `maxPrsPerBuild`, `maxRequestsPerBuild`,
`maxBodyChars`). Commit-message `#123` references stay unverified hints until the
host API confirms the commit belongs to that change request; only then is a
verified association (title, bounded body, state, permalink, fetched time)
stored. The token is read from the named environment variable at runtime and
never enters config or caches; a missing token leaves hints unverified without
affecting local indexing. The transport is built into the CLI behind the default
`github-enrichment` feature (`ureq` read-only requests to `api.github.com`);
`cargo check -p repoise-cli --no-default-features` produces the no-enrichment
binary. Host responses are ETag-cached in a repository-scoped JSON cache under
the cache root (removed by `purge`), requests are bounded per build, rate limits
stop the enrichment pass, and a permission denial fails closed by invalidating
the cached remote content.

## Installation and artifacts

`bash scripts/install.sh` (PowerShell: `./scripts/install.ps1`) uses `cargo install --path`
to install into Cargo's normal bin directory; add it to PATH. Optional Cargo install arguments
are forwarded, for example `--root /tmp/repoise-install`. Existing installs are replaced only
when Cargo permits it; explicitly pass `--force` when replacement is intended.
Uninstall with `cargo uninstall repoise-cli`, using the same `--root` if customized.
This is local development installation, not a registry release.

Rust-native checks/builds need no Node. Release tooling additionally uses Node 24 in CI,
with npm and a pinned Prettier dev dependency; install using npm ci --ignore-scripts.
Run npm run format before committing release JS/JSON/workflow changes, then
npm run format:check and npm test. These development dependencies are not installed by
consumers of the published launcher.
Git attributes keep text checkouts at LF on every platform, including Windows.

CI checks/tests/builds/smokes Linux, macOS and Windows. The manually dispatched
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
