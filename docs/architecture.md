# Code structure

Repoise is a Rust 2024 workspace with resolver 3 and one shared application lockfile.
This initial scaffold has no parser, index, database, provider, or network dependencies.

| Path | Responsibility |
| --- | --- |
| `crates/repoise-core/src/lib.rs` | Shared engine boundary; currently project identity only |
| `crates/repoise-cli/src/main.rs` | Native executable, argument handling and terminal I/O |
| `crates/repoise-cli/tests/` | Executable behavior tests |
| `npm/repoise/` | Thin launcher and allowlisted npm package metadata |
| `scripts/release/` | Claim preparation, packaging, smoke tests and release staging |
| `scripts/` | Local checks, release build and installation helpers |
| `.github/workflows/` | Cross-platform checks and manually requested build artifacts |
| `docs/plans/` | Intended scope and acceptance gates, not shipped behavior |

The CLI depends on the core; the core must not depend on a CLI or agent client.
Future indexing/search/exact-read services belong in core or focused engine crates.
Add crates when real dependency, platform or feature boundaries justify them; avoid empty
placeholder crates. A future MCP adapter should call the same services as the CLI.

Keep language parsers, source-control adapters, hosting enrichment, embeddings and persistence
behind explicit contracts as they are implemented. Do not let npm, GitHub, Git or a particular
language become a mandatory engine dependency. Configuration must remain data, never executed code.
The npm launcher only selects/runs an exact-version registry-delivered native package.
Other distribution adapters should use the same native executable. Registry ownership is
separate from internal Cargo package names; see [releases](releases.md).

See the [master plan](plans/v1_master_plan.md) for planned contracts and milestones.
Update this guide when crate boundaries or source entry points change.
