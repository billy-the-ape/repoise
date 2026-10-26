# Repoise repository guide

## Purpose and current state

Repoise is an offline-first repository knowledge indexer for coding agents.
The planned engine is Rust, with a native CLI and stdio MCP; npm is the first distribution channel.
Target repositories may use any language, framework, host or source-control system.
This repository has a Rust workspace and native CLI with configuration, discovery, init,
doctor, and the offline docs+code index: incremental SQLite/FTS5 chunking and publication,
status freshness, offline lexical search, exact read-back and purge (cards K1–K2),
plus card K3: opt-in OpenAI-compatible embeddings with a content-addressed cache,
vector coverage, hybrid RRF search, budgets and embedding-cache GC, and card K4:
TypeScript/JavaScript structural code chunks, symbols, syntactic reference edges and
the line-window fallback for other code languages (embedding scope docs or docs+code),
card K5: the opt-in offline local history lane (`search --lane history`), and card K6:
the related-knowledge service, offline `check` with stable exit codes, the incremental
`watch` loop, the non-destructive overlay lifecycle (init agents snippet, `overlay
uninstall`, `overlay update` three-way template migration) and the stdio MCP server
(lexical-first; refresh tool opt-in via `mcp.refresh` or `--allow-refresh`; see
docs/mcp.md).
Cross-worktree cache reuse lets linked Git worktrees of one local repository share immutable parse artifacts and embedding vectors through a versioned family store (default under the Git common directory; `sharedCache: false` opts out; see docs/storage.md).
Pre-v1 reliability fixes follow the first CLI/MCP pilot: doctor reports the
selected snapshot mode and the actually selected config/local-config paths,
lexical search enforces strict all-terms-in-one-chunk matching (parenthesized
FTS groups), copied fixture/test-data paths no longer gain the current-document
ranking boost, and retrieval-workflow guidance (including strict lexical
matching and `next_cursor` pagination) is shipped in the MCP tool
descriptions, CLI help and the managed AGENTS.md snippet (template 1.2.0).
`repoise index` reports live phase progress (discovery, reconciliation,
optional embedding/history, publication) to stdout, or stderr under `--json`,
with counts, elapsed time and an approximate ETA only when reliable.
The CLI automation contracts (agent platform AP-02) ship the
`repoise capabilities` handshake, an explicit `--events` NDJSON stream for
`index` and `watch` (started/progress/warning/heartbeat plus one terminal
event; checked-in schemas under schemas/), stable exit codes, documented
SIGINT/SIGTERM cancellation for `watch`, and an advisory writer-ownership
marker surfaced by `status` (docs/automation.md).
The functional 0.1.0-alpha.1 preview is published as native archives and all four
npm packages under both latest and next; the version remains an alpha preview. Public-registry CLI/MCP
checks passed on macOS ARM64; Linux/Windows registry-install checks remain
unmeasured. OIDC staging failed; maintainer-local staging/approval completed
publication without CI-generated npm provenance. See docs/publish/alpha-1-status.md. The
build/Actions integration examples are documented (consumer guide and a minimal GitHub
Actions example under docs/build-ci.md and examples/; offline docs-code-lexical,
explicitly invoked index/check, read-only, isolated cache, no active workflow and no
registry install). The offline lexical evaluation harness
(frozen synthetic fixtures, 60 labeled queries with 12 held-out, per-hit
provenance validation, recall@5/exact/policy/timing metrics, replay-evidence
import format and runbook under scripts/eval/) has landed; hybrid evaluation
remains unmeasured; an initial live lexical pilot verified functionality but did
not establish consistent agent speed/token gains. Package release
validation ships in the CI/release matrices: the packed-consumer smoke runs the
same offline docs+code fixture (init, index, search, read, status, check, stdio MCP
with read-only refresh refusal, bundled schema assets, no-Node native runs) against
both the installed launcher and the extracted native archive on Linux, macOS and
Windows. The local inference embedding preset is evaluated, not shipped; OIDC staging
requires authentication diagnosis.
Commands in the plan are proposals, not installed tools or proof of shipped behavior.

The repository is a Cargo + pnpm workspace: pnpm (pinned by the root
`packageManager` field, lockfile `pnpm-lock.yaml`) owns JS/TS development
dependencies, and Turborepo (`turbo.json`) declares development tasks plus
single per-platform native bridge tasks (`check:native`, `build:native`) with
output caching disabled; Cargo commands still work without Node. CI selects
affected check sets per PR through `scripts/ci/impact-map.json` and
`scripts/ci/select-checks.mjs`, with an aggregate `CI gate` job enforcing
expected results (unknown paths, empty diffs and selection failures run the
full suite).

Follow-on planning for external agent adapters, real-agent benchmarks and the
VS Code UI lives in the [agent platform roadmap](docs/plans/agent-platform/README.md);
AP-01 (workspace/CI) and AP-02 (CLI automation contracts) are implemented.
Start there for implementation order, open questions and per-PR acceptance
criteria. The remaining cards are proposed work; no benchmark command or
editor extension ships from that plan.

## Read only the context relevant to the task

| Need                                               | Authoritative starting point                                                                                           |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| Project overview and current readiness             | [README.md](README.md)                                                                                                 |
| v1 scope, contracts, cards and acceptance criteria | [Master plan](docs/plans/v1_master_plan.md)                                                                            |
| Per-PR implementation specs (cards K1–K6 + consumer) | [PR documents](docs/plans/): completed cards K1–K5 in [complete/](docs/plans/complete/), K6/consumer in `v1-5.md`/`v1-6.md` |
| Current code structure and portability boundaries  | [Architecture](docs/architecture.md); master plan section 3                                                            |
| Toolchain, checks, build and local installation    | [Development](docs/development.md)                                                                                     |
| npm ownership, release control and distribution      | [Publishing guides](docs/publish/README.md); [alpha status](docs/publish/alpha-0-status.md); [alpha-1 readiness](docs/publish/alpha-1-readiness.md); [alpha-1 status](docs/publish/alpha-1-status.md); [Changelog](CHANGELOG.md) |
| Intake, parsing and freshness planning             | Master plan sections 4–7                                                                                               |
| Persistent index, cache layout and purge           | [Storage guide](docs/storage.md)                                                                                     |
| MCP server (stdio), tools and client registration | [MCP guide](docs/mcp.md)                                                                                             |
| CLI automation (capabilities, `--events`, exit codes, signals, ownership) | [Automation contracts](docs/automation.md)                                                              |
| Agent adapter protocol, conformance and the Cline adapter | [Agent adapters](docs/agent-adapters.md)                                                          |
| Agent setup, CLI/MCP retrieval workflow and worktrees | [Agent setup](docs/agents.md)                                                                                     |
| Portable consumer skill and one-command installation | [Agent skill](skills/repoise/SKILL.md); [installation](docs/agents.md#install-the-agent-skill) |
| Build/CI integration and consumer guide            | [Build/CI guide](docs/build-ci.md)                                                                                   |
| Agent interfaces and installation                  | Master plan sections 8–10                                                                                              |
| Benchmarks, resource budgets and adoption gates    | Master plan section 11; [evaluation runbook](scripts/eval/RUNBOOK.md) |
| Implementation sequence and rollout                | Master plan sections 12–14                                                                                             |

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
Release tooling: pnpm install --frozen-lockfile, pnpm run format:check and
pnpm test (pnpm pinned via the root `packageManager` field); registry packing
and staging still invoke real npm, never pnpm.
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

## Editing

- Tool parameter inputs, especially editor, should be kept below 5000 characters
- Use rustfmt to format files instead of ruminating over formatting
