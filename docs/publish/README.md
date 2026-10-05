# Publishing Repoise

Two independent channels distribute the same Rust CLI: native GitHub Release archives and
a thin npm launcher with platform binary packages. Publishing a scaffold prerelease does
not complete the v1 indexing acceptance criteria.

| Guide                                     | Purpose                                                          |
| ----------------------------------------- | ---------------------------------------------------------------- |
| [npm publication](npm.md)                 | Account setup, name claims, OIDC staging, approvals and recovery |
| [Native releases](native.md)              | Build artifacts, checksums, draft review and publication         |
| [Initial alpha status](alpha-0-status.md) | Completed operations, evidence and remaining bootstrap steps     |

Read the status record before continuing `0.1.0-alpha.0`; do not replay completed operations.
Update it after verified registry changes. For later versions, follow the repeatable runbooks.

## Targets and readiness

| npm package             | Target                        | Tested runner       |
| ----------------------- | ----------------------------- | ------------------- |
| `repoise`               | Node launcher; Node >=22.14.0 | All three below     |
| `repoise-linux-x64-gnu` | `x86_64-unknown-linux-gnu`    | Ubuntu 24.04        |
| `repoise-darwin-arm64`  | `aarch64-apple-darwin`        | macOS 15            |
| `repoise-win32-x64`     | `x86_64-pc-windows-msvc`      | Windows Server 2025 |

Runner baselines do not establish compatibility with every older OS. Linux musl/ARM64,
macOS Intel and other npm targets are unsupported; build Rust source for other targets.
Native binaries need no Node/npm. The launcher selects an exact-version optional binary
dependency and preserves arguments, streams and exit status. There are no install hooks,
model downloads, repository scans or source-build fallbacks during install/invocation.
Current behavior is greeting/help/version and argument errors; indexing/MCP are unimplemented.

Both Cargo crates have `publish = false`. crates.io, Homebrew, containers and other package
managers remain future work. Rust is the engine boundary; npm/GitHub are distribution adapters.

## Workflow and validation

[release.yml](../../.github/workflows/release.yml) is manually dispatched from `main`.
Pushing a tag alone does not start it.

| Input          | Meaning                                                     |
| -------------- | ----------------------------------------------------------- |
| `tag`          | Existing `v<version>` at a reviewed commit merged into main |
| `stage_npm`    | Stage all four npm packages through OIDC; default false     |
| `draft_native` | Create an unpublished native release draft; default false   |

Both options false performs validation/build/assembly only. Channels can run independently.
Tag validation checks synchronized versions/main ancestry and resolves an immutable commit.
Builds use pinned Rust, Cargo.lock, Node 24 and fixed hosted runner labels. They run formatting,
release-tool tests, Clippy, Rust tests and packed-consumer smoke on all three platforms.
The smoke installs tarballs offline with install scripts disabled in a temporary path with
spaces; it checks help/version/argument errors and native invocation without Node in PATH.
It does not yet validate indexing or MCP, or the public registry install path.

The `release-bundle` Actions artifact retains for 14 days. It contains four npm tarballs,
three native archives, SHA256SUMS and release.json. Metadata records source SHA/version,
package order and dist-tag. Privileged jobs validate exact filenames, checksums and source SHA
before writes. Long-term published assets belong on npm/GitHub Releases. SHA-256 checksums
detect corruption; independent native signing/attestations remain future work.

| Source                                                                                               | Responsibility                                     |
| ---------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| [common.mjs](../../scripts/release/common.mjs), [check-tag.mjs](../../scripts/release/check-tag.mjs) | Versions, targets, tag validation                  |
| [pack.mjs](../../scripts/release/pack.mjs), [smoke.mjs](../../scripts/release/smoke.mjs)             | Packaging and installed-consumer verification      |
| [assemble.mjs](../../scripts/release/assemble.mjs), [bundle.mjs](../../scripts/release/bundle.mjs)   | Bundle metadata/integrity                          |
| [publish.mjs](../../scripts/release/publish.mjs)                                                     | Native-first, launcher-last staging                |
| [github-draft.mjs](../../scripts/release/github-draft.mjs)                                           | Native draft/assets                                |
| [claim.mjs](../../scripts/release/claim.mjs)                                                         | Pack the nonfunctional ownership bootstrap locally |

Checks for a reviewed checkout:

```sh
npm ci --ignore-scripts
npm run format:check
npm test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

See [development](../development.md) for rustdoc/local build checks. The raw engineering
artifact workflow is separate from the release pipeline. Runtime secrets are not required.
