# Code structure

Repoise is a Rust 2024 workspace with resolver 3 and one shared application lockfile.
The core crate currently has no parser, index, database, provider, or network dependencies;
the Git adapter shells out to `git` with argument arrays when operating on a Git repository.

| Path | Responsibility |
| --- | --- |
| `crates/repoise-core/src/lib.rs` | Shared engine boundary; exposes the services below plus config/init file names and defaults |
| `crates/repoise-core/src/adapter/` | Neutral `SourceAdapter` contract: opaque revisions, capability flags, enumerate/read with containment; filesystem and Git adapters plus a synthetic fake-revision test adapter |
| `crates/repoise-core/src/config.rs` | Versioned `repoise.config.json` parsing, precedence (CLI > local > committed > default) and redaction-safe validation |
| `crates/repoise-core/src/ignore.rs` | Explainable include/exclude/secret-deny policy: package defaults, config, native ignore inheritance, gitignore-style matching, stable fingerprints |
| `crates/repoise-core/src/classify.rs` | Role/lifecycle classification: explicit front matter, configured rules, path inference |
| `crates/repoise-core/src/discovery.rs` | Deterministic inventory: snapshot manifest, file records, skip diagnostics |
| `crates/repoise-core/src/provenance.rs` | Versioned repository/snapshot/file provenance record shapes and remote identity sanitization |
| `crates/repoise-core/src/init.rs` and `doctor.rs` | Idempotent overlay init (config, overlay manifest, optional managed block) and effective-settings reporting |
| `crates/repoise-core/src/hash.rs` | Lowercase SHA-256 helpers used across manifests and fingerprints |
| `crates/repoise-cli/src/main.rs` | Native executable, argument handling and terminal I/O (doctor/explain/index/init adapters) |
| `crates/repoise-core/tests/` and `crates/repoise-cli/tests/` | Contract and executable behavior fixtures |
| `schemas/` | Published versioned configuration schema |
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
