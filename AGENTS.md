# Repoise repository guide

## Purpose and current state

Repoise is an offline-first repository knowledge indexer for coding agents.
The planned engine is Rust, with a native CLI and stdio MCP; npm is the first distribution channel.
Target repositories may use any language, framework, host or source-control system.
This repository currently has planning documentation only: no Cargo workspace or working CLI.
Commands in the plan are proposals, not installed tools or proof of shipped behavior.

## Read only the context relevant to the task

| Need | Authoritative starting point |
| --- | --- |
| Project overview and current readiness | [README.md](README.md) |
| v1 scope, contracts, cards and acceptance criteria | [Master plan](docs/plans/v1_master_plan.md) |
| Architecture and portability boundaries | Master plan section 3 |
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

Once Cargo.toml exists, follow the pinned toolchain/MSRV and repository configurations.
Do not add a toolchain pin or scaffold as part of an unrelated documentation change.
Use ordinary cargo commands unless project scripts or CI specify a more targeted equivalent:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Apply cargo fmt --all before committing Rust changes; honor rustfmt/clippy and wrapper configs.
These commands are not runnable in the current documentation-only repository.
Run affected tests and required CI checks; report unavailable checks and reasons accurately.
Optional inference/platform features need their own relevant tests, not blind --all-features runs.
Pin and justify dependencies; review licenses, native assets and supported targets.
Use explicit error handling and contextual errors; avoid panics for normal input/I/O failures.
Minimize unsafe code; document safety invariants and isolate unavoidable FFI boundaries.
Do not block async executors with CPU-heavy parsing/inference; use bounded worker scheduling.
Commit the application workspace Cargo.lock when the CLI is scaffolded.
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
