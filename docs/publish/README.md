# Publishing Repoise

Two independent channels distribute the same Rust CLI: native GitHub Release archives and
a thin npm launcher with platform binary packages. Publishing a scaffold prerelease does
not complete the v1 indexing acceptance criteria.

| Guide                                     | Purpose                                                          |
| ----------------------------------------- | ---------------------------------------------------------------- |
| [npm publication](npm.md)                 | Account setup, name claims, OIDC staging, approvals and recovery |
| [Native releases](native.md)              | Build artifacts, checksums, draft review and publication         |
| [Initial alpha status](alpha-0-status.md) | Completed operations, evidence and remaining bootstrap steps     |

The functional [0.1.0-alpha.1 preview](alpha-1-status.md) is published under npm
`next` and as native prerelease archives. Read execution records before any
recovery operation; do not replay completed publications. The alpha.0 record
is historical.
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
Both Cargo crates have `publish = false`. crates.io, Homebrew, containers and other package
managers remain future work. Rust is the engine boundary; npm/GitHub are distribution adapters.
The native engine provides the offline docs+code lexical index (init, doctor, explain,
index, status, search, read, related, check, watch, overlay, purge) and the stdio MCP server;
embeddings and remote history enrichment are opt-in. The published `0.1.0-alpha.0` archives
predate that engine work and remain greeting/help/version only; 0.1.0-alpha.1 ships the functional engine as a preview.

## Workflow and validation

[release.yml](../../.github/workflows/release.yml) is manually dispatched from `main`.
Pushing a tag alone does not start it.

| Input          | Meaning                                                     |
| -------------- | ----------------------------------------------------------- |
| `tag`          | Existing `v<version>` at a reviewed commit merged into main |
| `stage_npm`    | Stage all four npm packages through OIDC; default false     |
| `draft_native` | Create an unpublished native release draft; default false   |

Both options false performs validation/build/assembly only — validation runs never
publish: no npm package is published and no GitHub release is created. `stage_npm`
explicitly stages the four npm packages through OIDC, and `draft_native` explicitly
creates an unpublished native release draft. Channels can run independently.
Tag validation checks synchronized versions/main ancestry and resolves an immutable commit.
Builds use pinned Rust, Cargo.lock, Node 24 and fixed hosted runner labels. They run formatting,
release-tool tests, Clippy, Rust tests and packed-consumer smoke on all three platforms.
The smoke installs tarballs offline with install scripts disabled in a temporary path with
spaces, then runs the same synthetic offline fixture (one Markdown doc plus TypeScript and
JavaScript sources) from outside the source checkout against both the installed launcher and
the extracted native archive: `init --preset docs-code-lexical`, `index`, lexical `search`
finding the expected doc and code hits, exact `read` back of the expected text, `status` and
`check --fresh` reporting a fresh index, and a stdio MCP session (protocol initialization,
initialized notification, tool discovery, search/read/status calls with CLI-identical
results, clean shutdown on stdin EOF) that confirms the default server hides
`refresh_project_knowledge` and refuses an attempt to invoke it. Native functional runs use
no Node/npm on PATH; the generated cache is isolated per install; every subprocess is bounded
by timeouts and the temporary tree is removed on exit, so broken MCP behavior cannot hang the
run. The versioned config schema (`schemas/repoise.config.v1.schema.json`) ships in both
installed forms and must match the committed source; tree-sitter parser grammars are compiled
into the native binary, so no parser asset resolves from the source checkout. Fresh public-registry installation and CLI/MCP checks passed on macOS ARM64
for alpha.1. Linux/Windows public-registry installs remain unmeasured.

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

## Toolchain and platform requirements

- Native engines: Rust 1.99.0 pinned by `rust-toolchain.toml` (workspace MSRV 1.99),
  `--locked` Cargo builds, bundled SQLite. Supported targets: Linux x64 glibc,
  macOS ARM64 and Windows x64 MSVC (the npm targets table above).
- npm launcher: Node >=22.14.0 (`engines`); CI builds and smoke runs use Node 24.
  The native binaries need no Node/npm at runtime; the smoke proves functional
  operation (index/search/read/status/check/MCP) with an empty `PATH`.
- Smoke runs require only the packed artifacts and the Node/npm used to assemble
  them; no credentials, embeddings, network access or repository secrets are needed.

## Limitations and rollback

- Offline smoke validates packed artifacts. Alpha.1 additionally passed fresh
  registry-install checks on macOS ARM64; see [execution evidence](alpha-1-status.md).
- Windows name clearance is complete. CI OIDC staging failed authentication;
  alpha.1 used maintainer-local staging and approval without npm CI provenance.
- Runner baselines are the pinned labels in the targets table; other OS/arch/libc
  combinations are unsupported and the launcher reports a clear error for them.
- Artifact-only runs do not stage or create releases. Explicitly enabled
  staging/draft jobs write npm stages and an unpublished native draft; they do
  not approve stages, publish the draft, tag commits, or change versions.
- Public versions and tags are immutable release history. Pin a verified
  functional version or stop using the preview; the scaffold alpha is not a
  functional fallback. Publish corrections as a new version. Smoke caches are
  disposable and removed with the temporary fixture.
