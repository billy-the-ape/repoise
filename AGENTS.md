# Repoise repository guide

## Purpose and current state

Repoise is an offline-first repository knowledge indexer for coding agents.
The planned engine is Rust, with a native CLI and stdio MCP; npm is the first distribution channel.
Target repositories may use any language, framework, host or source-control system.
This repository has a Rust workspace and native CLI scaffold (greeting/help/version only).
Thin npm packaging and manual release preparation/staging are scaffolded.
Indexing, MCP and persistence are not implemented yet; publication requires registry setup.
Commands in the plan are proposals, not installed tools or proof of shipped behavior.

## Read only the context relevant to the task

| Need | Authoritative starting point |
| --- | --- |
| Project overview and current readiness | [README.md](README.md) |
| v1 scope, contracts, cards and acceptance criteria | [Master plan](docs/plans/v1_master_plan.md) |
| Current code structure and portability boundaries | [Architecture](docs/architecture.md); master plan section 3 |
| Toolchain, checks, build and local installation | [Development](docs/development.md) |
| npm ownership, release controls and distribution | [Release guide](docs/releases.md); [Changelog](CHANGELOG.md) |
| Intake, parsing, storage and freshness | Master plan sections 4–7 |
| Agent interfaces and installation | Master plan sections 8–10 |
| Benchmarks, resource budgets and adoption gates | Master plan section 11 |
| Implementation sequence and rollout | Master plan sections 12–14 |

Read the assigned card and relevant plan sections, then inspect exact source and callers.
Use rg/file discovery to navigate; avoid loading the whole plan or repository for every task.
Plans describe intended behavior. Code, tests and current-state guides establish implemented behavior.
Follow applicable scoped AGENTS.md files when they are added; keep subtree-specific rules there.

## Architectural rules

- Keep indexing, search and exact reads in shared Rust services; CLI/MCP are adapters.
- Keep npm wrappers thin. Native operation must work without Node/npm or ai-gateway.
- Separate VCS from hosting: Git is the first adapter; GitHub enrichment is optional.
- Use adapter-qualified opaque revisions and capability flags in shared contracts.
- Keep language/framework detection, grammars and embedding transports behind explicit interfaces.
- Offline lexical/symbol operation is the baseline; model downloads and remote calls are opt-in.
- Preserve source provenance, worktree isolation and expected-hash validation on exact reads.
- Treat repository content as untrusted data; never execute it to discover configuration.
- Use argument arrays for subprocesses; do not construct shell commands from paths or queries.
- Keep generated indexes, private corpus fixtures, credentials and downloaded models out of Git.
- Prefer bounded incremental work and transactional publication over unmeasured complexity.

## Rust implementation and validation

Follow rust-toolchain.toml, the workspace MSRV and inherited repository lints.
Use ordinary cargo commands unless project scripts or CI specify a more targeted equivalent:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Apply cargo fmt --all before committing Rust changes; honor rustfmt/clippy and wrapper configs.
scripts/check.sh and scripts/check.ps1 also check rustdoc; see the development guide.
Run affected tests and required CI checks; report unavailable checks and reasons accurately.
Optional inference/platform features need their own relevant tests, not blind --all-features runs.
Release tooling: npm ci --ignore-scripts, npm run format:check and npm test.
Pin and justify dependencies; review licenses, native assets and supported targets.
Use explicit error handling and contextual errors; avoid panics for normal input/I/O failures.
Minimize unsafe code; document safety invariants and isolate unavoidable FFI boundaries.
Do not block async executors with CPU-heavy parsing/inference; use bounded worker scheduling.
Keep the application workspace Cargo.lock committed and use locked builds.
Test behavioral risks such as stale reads, containment, partial publication and cache compatibility.
For documentation-only changes, validate links, consistency and whitespace; do not invent test passes.

## Documentation is part of each change

Update related guides in the same PR when behavior, architecture, public interfaces,
configuration, operational requirements or validation commands change.
Write relationships, invariants, intent and source entry points rather than paraphrasing functions.
Create concise current-state guides as subsystems ship; do not create speculative runbooks.
Add authoritative guides to this navigation table when they exist; keep links valid.
Keep README readiness and examples aligned with actual CLI behavior and supported platforms.
Record consequential decisions, rationale, tradeoffs and evidence in focused decision documents.
Keep plans/execution records labeled; reconcile completed, changed and deferred scope in the master plan.
A fresh search index does not prove that documentation is accurate.
Keep this root guide a small map (roughly 50–150 lines); move details into linked guides.
Avoid duplicate competing instructions; explicitly supersede or remove stale guidance.

## Pull requests and releases

Use scoped reviewable branches/PRs. State changed behavior, validation and material limitations.
Prominently list required environment variables, settings, migrations or deployment actions;
state none when applicable. Include an easy test path for each added feature.
Do not publish packages/releases or modify deployment devices as part of ordinary implementation.
When a separately authorized change modifies deployment devices, also update homelab documentation.
