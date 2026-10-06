# Repoise

An offline-first repository knowledge indexer for coding agents, designed to help them find
relevant code, documentation and history with compact, source-linked context.

**Status: configuration and explainable discovery.** The native CLI supports `init`,
`doctor`, `explain`, and `index` (deterministic snapshot manifest and file inventory).
Search, persistence and MCP are not implemented yet. Native `0.1.0-alpha.0` archives are
published, along with Linux/macOS npm binary packages. The main npm launcher and Windows
npm package are pending a registry name review; `npx repoise` is not yet a usable
installation path.

- [v1 master plan](docs/plans/v1_master_plan.md) — design, implementation cards and benchmark gates.
- [Code structure](docs/architecture.md) — crate responsibilities and portability boundaries.
- [Development](docs/development.md) — toolchain, checks, build and installation.
- [Publishing](docs/publish/README.md) — npm staging, approvals, native releases and current status.
- [Agent contributor guide](AGENTS.md) — navigation, Rust practices and documentation maintenance.

The planned native tool works independently of Node, npm, any particular language/framework,
or repository host. npm is the first package-manager distribution channel; Git and filesystem snapshots
are the first source adapters. Embeddings and remote history enrichment are optional.

MIT licensed. [Native `0.1.0-alpha.0` downloads](https://github.com/billy-the-ape/repoise/releases/tag/v0.1.0-alpha.0)
are available for Linux x64 GNU, macOS ARM64 and Windows x64 MSVC.
See the [alpha publishing record](docs/publish/alpha-0-status.md) for npm availability.

With rustup installed:

```sh
cargo run --package repoise-cli -- --help
cargo run --package repoise-cli -- doctor            # effective settings and policy
cargo run --package repoise-cli -- init             # idempotent, non-interactive setup
cargo run --package repoise-cli -- explain --path README.md
cargo run --package repoise-cli -- index            # deterministic manifest + inventory
```

Rust 1.99.0 is pinned. See the development guide for Unix/Windows scripts and CI artifacts.
