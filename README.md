# Repoise

An offline-first repository knowledge indexer for coding agents, designed to help them find
relevant code, documentation and history with compact, source-linked context.

**Status: native CLI scaffold.** Greeting, help and version work. Indexing, persistence,
MCP and npm packaging are not implemented yet.

- [v1 master plan](docs/plans/v1_master_plan.md) — design, implementation cards and benchmark gates.
- [Code structure](docs/architecture.md) — crate responsibilities and portability boundaries.
- [Development](docs/development.md) — toolchain, checks, build and installation.
- [Agent contributor guide](AGENTS.md) — navigation, Rust practices and documentation maintenance.

The planned native tool works independently of Node, npm, any particular language/framework,
or repository host. npm is the first planned distribution channel; Git and filesystem snapshots
are the first source adapters. Embeddings and remote history enrichment are optional.

License and release packaging remain to be selected before distribution.

With rustup installed:

```sh
cargo run --package repoise-cli -- --help
cargo run --package repoise-cli
```

Rust 1.99.0 is pinned. See the development guide for Unix/Windows scripts and CI artifacts.
