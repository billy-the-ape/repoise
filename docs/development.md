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
installed (there are no external crates).

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
Use idiomatic ownership, explicit error propagation and bounded work; do not add async runtimes,
parser/database dependencies or micro-optimizations until a feature and measurements require them.

## CLI commands

The native CLI works without Node/npm. Commands (all accept an optional positional root):

```sh
repoise doctor [ROOT]                 # effective settings, capabilities, policy summary
repoise explain --path <REL> [ROOT]   # explainable include/exclude decision for one path
repoise index [ROOT]                  # deterministic snapshot manifest and file inventory
repoise init [ROOT]                   # idempotent, non-interactive configuration
repoise greet | help | version
```

Global options: `--json` (machine-readable output), `--config <PATH>` and
`--local-config <PATH>` (explicit config locations), `--max-file-bytes <N>`.
Init options: `--preset docs-only|docs-code-lexical|hybrid`, `--provider <NAME>`
(required for hybrid), `--adopt-managed-block <FILE>`, `--dry-run`, `--yes`.
Unsupported arguments exit 2 with the usage on stderr; I/O and service failures exit 1.
Search, persistence and MCP are not implemented yet.

Easy fixture path (no credentials, no network):

```sh
tmp=$(mktemp -d) && printf 'hello\n' > "$tmp/README.md"
repoise init --preset docs-only --adopt-managed-block README.md "$tmp"
repoise doctor "$tmp"
repoise explain --path .env "$tmp"     # excluded by secret deny
repoise index "$tmp"                   # manifest hash over the included files
```

The workspace test suite covers these behaviors end to end:
`cargo test --workspace --locked` (contract fixtures live in
`crates/repoise-core/tests/v1_0.rs` and `crates/repoise-cli/tests/cli.rs`; secret fixtures
are generated in memory and never stored in Git).

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
