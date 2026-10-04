# Development and native builds

## Requirements

Install [rustup](https://rustup.rs/). `rust-toolchain.toml` pins Rust 1.99.0 with rustfmt and
Clippy; `rust-version = "1.99"` is the initial MSRV. This is the stable release dated
2026-10-01. Change the pin and MSRV deliberately together until a broader compatibility
policy is tested. Use Edition 2024 and workspace resolver 3.
No Node, Git installation, credentials, embedding model or runtime environment variables
are required to run this scaffold. First toolchain installation requires network access;
subsequent builds are offline-capable once tools are installed (there are no external crates).

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

The CLI accepts no arguments (greeting), `-h`/`--help`, and `-V`/`--version`.
Unsupported arguments exit 2 with an error on stderr. I/O failures exit 1; closed output pipes
exit successfully. No indexing commands or persistent files exist yet.

## Installation and artifacts

`bash scripts/install.sh` (PowerShell: `./scripts/install.ps1`) uses `cargo install --path`
to install into Cargo's normal bin directory; add it to PATH. Optional Cargo install arguments
are forwarded, for example `--root /tmp/repoise-install`. Existing installs are replaced only
when Cargo permits it; explicitly pass `--force` when replacement is intended.
Uninstall with `cargo uninstall repoise-cli`, using the same `--root` if customized.
This is local development installation, not a registry release.

CI checks/tests/builds/smokes Linux, macOS and Windows. The manually dispatched
**Build native artifacts** workflow produces binaries for Linux x86_64 GNU, macOS ARM64 and
Windows x86_64 MSVC, retained for seven days. These are engineering artifacts: no installer,
checksums/signing, compatibility guarantee, npm publication or GitHub release yet.
Artifact targets are an initial test matrix, not the final distribution support policy.
No server deployment or secrets are required. Both Cargo packages have `publish = false`;
license, registry naming, release authorization and packaging must be settled before publication.

## Current references

- [Rust 1.99 release](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/)
- [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)
- [Rust 2024 edition](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)
- [rustup toolchain files](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)

Keep this guide and [AGENTS.md](../AGENTS.md) aligned with CI and actual commands.
