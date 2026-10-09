# Repoise

An offline-first repository knowledge indexer for coding agents, designed to help them find
relevant code, documentation and history with compact, source-linked context.

**Status: offline docs+code index with opt-in hybrid search.** The native CLI supports
`init`, `doctor`, `explain`, `index` (incremental chunking and persistent SQLite/FTS5
publication), `status` (scope/snapshot/freshness), `search` (offline lexical search over
docs and code chunks, plus hybrid search over an operator-configured OpenAI-compatible
embedding provider) and `read` (exact read-back with hash validation), plus `purge` for
generated cache data. `related` follows structural/section links from an indexed source
(reference edges, referencers, parent/child chunks), `check` reports stable
freshness/coverage exit codes for automation, and `watch` runs the incremental watch loop
with safe Ctrl+C cancellation. `overlay uninstall`/`overlay update` manage the
non-destructive overlay lifecycle (byte-level conflict reporting and three-way template
migration). Code indexing covers TypeScript/TSX and JavaScript/JSX structural
chunks with symbols and syntactic reference edges, and a line-window fallback for other
code languages; embeddings apply to code only when the embedding scope includes it.
MCP is not implemented yet. An opt-in local history lane (bounded mainline commits
with separate `search --lane history` retrieval and optional verified GitHub PR
enrichment) is available. Native `0.1.0-alpha.0`
archives are published, along with Linux/macOS npm binary packages. The main npm
launcher and Windows npm package are pending a registry name review; `npx repoise`
is not yet a usable installation path.

- [v1 master plan](docs/plans/v1_master_plan.md) — design, implementation cards and benchmark gates.
- [Code structure](docs/architecture.md) — crate responsibilities and portability boundaries.
- [Persistent index and cache](docs/storage.md) — storage layout, generations and invariants.
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
cargo run --package repoise-cli -- index            # build and publish the offline index
cargo run --package repoise-cli -- status           # scope, snapshot and freshness
cargo run --package repoise-cli -- search --query README
cargo run --package repoise-cli -- related --source-id <ID>  # follow links from a result
cargo run --package repoise-cli -- check                     # stable freshness/coverage exit codes
cargo run --package repoise-cli -- watch                     # incremental watch loop (Ctrl+C)
```

Rust 1.99.0 is pinned. See the development guide for Unix/Windows scripts and CI artifacts.
