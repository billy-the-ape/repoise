# AP-01: workspace foundation and affected CI

Status: proposed; first implementation PR after the planning PR. No layout,
lockfiles, workflow filters or package versions change in the planning PR.
Parent: [program index](README.md).

## Outcome and recommendation

Keep related products in one repository with explicit boundaries and dependent
checks. The repo is already a Rust workspace with a Node development/release layer;
the transition is incremental, not a rewrite or a move of everything into npm.

Recommend root Cargo workspace plus pnpm workspaces and Turborepo. Cargo remains
authoritative for Rust compilation/features/tests. pnpm owns JS/TS dependencies;
Turbo schedules declared tasks. Its package graph is based on package manifests,
not Cargo metadata, subprocess calls or executable compatibility contracts.

| Option                                       | Fit and tradeoff                                                                                           |
| -------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| Cargo + pnpm + Turbo                         | Recommended: familiar JS/TS workflow, explicit tasks, Cargo unchanged; Rust impact needs a bridge          |
| Cargo + pnpm + explicit CI scripts           | Viable simpler fallback if Turbo adds no useful orchestration yet; same boundary/impact contracts required |
| Nx                                           | Candidate for richer project graph/plugin tooling; more setup than this first foundation needs             |
| Bazel/Pants or another polyglot build system | Reconsider if hermetic cross-language builds become a demonstrated need; not required for this transition  |

Do not claim a performance win from adopting a task runner. Verify incremental
selection and packaging correctness before enabling task-output caching.

## Target homes

Paths are proposed stable homes, not instructions to create empty packages.
Create real packages when they have code; document reserved homes now.

| Path                                                         | Ownership                                                                                              |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `crates/` | Existing Rust workspace/core/CLI; keep locations and native build entry points                         |
| `npm/repoise/`                                               | Existing published launcher; preserve package name, bin, exact optional-native pins and file allowlist |
| `apps/vscode/`                                               | Future extension application, packaging and editor tests                                               |
| `packages/agent-adapter-cline/`                              | First optional agent runner implementation                                                             |
| `packages/bench-contracts/`                                  | JS types/validation derived from canonical schemas when needed; no competing handwritten schema        |
| `packages/bench-runtime/`                                    | Reserved only if AP-04 selects an optional Node coordinator companion                                  |
| `schemas/`                                                   | Canonical versioned public contracts, including events/bench protocol when introduced                  |
| `scripts/ci/`                                                | Explicit component-impact mapping and aggregate-check helpers                                          |
| `scripts/release/`, `scripts/eval/`                          | Existing release tooling and frozen retrieval evaluation; avoid gratuitous moves                       |
| `skills/`, `docs/`, `examples/`                              | Agent guidance, documentation and synthetic consumer examples                                          |

Workspace globs should be explicit (`apps/*`, `packages/*`, `npm/repoise` as
appropriate); exclude generated native-package staging, fixtures, consumer
examples and nested project manifests unless intentionally participating.
Internal npm names and publication policy must be chosen before adding packages;
default new packages to private. Do not rename published packages or add a runtime
workspace dependency to the native launcher merely for graph bookkeeping.

## Migration work

1. Rebase on main and inventory open PRs listed in the program index. Preserve
   their source/skill/release paths and update any changed migration assumptions.
2. Pin supported pnpm and Turbo versions and declare the package manager. Convert
   the development lockfile once; one authoritative JS development lockfile after
   migration, with documented exceptions for standalone consumer test fixtures.
3. Keep direct Cargo commands working without Node. Keep `scripts/check.sh` and
   PowerShell equivalents as supported native entry points. Root JS commands may
   invoke them without becoming required for native users.
4. Migrate development/CI installs to frozen pnpm installation. Keep registry
   packaging via npm where release tooling requires npm pack/staging semantics.
   pnpm for contributors must not impose pnpm on npm consumers.
5. Audit `scripts/release/common.mjs`: `npm_execpath` under pnpm can identify the
   pnpm CLI, not npm. Explicitly resolve the correct npm executable; do not pass
   npm commands to pnpm accidentally. Preserve Windows shim behavior.
6. Preserve source/version checks, schema/license copies, root-relative paths,
   staged publication approvals, native archives and no-Node smoke coverage.
   Avoid teaching all future private packages to share the native release version:
   existing version validation currently checks every Cargo package.

## Task and change-impact graph

Start conservatively. A versioned explicit component map bridges Cargo paths and
non-package inputs into CI. Treat unknown paths, missing merge bases or failed
selection as full-check cases, never as no-op success. Turbo filters alone are
insufficient. Read the complete PR diff, including renames/deletions and spaces;
do not silently truncate a paginated file list. Use the correct event base/head
and fetch the merge base; an absent base is not an empty diff.

| Changed area                                           | Minimum impacted checks                                                                                                       |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| `crates/repoise-core/**`                               | Rust workspace checks + CLI/MCP contracts + packaged consumer smoke + dependent adapter/extension contract tests once present |
| `crates/repoise-cli/**`                                | CLI/MCP tests + native packaging smoke + consumers of changed CLI protocols                                                   |
| `npm/repoise/**`, release tooling                      | Launcher/release tests + three-platform packed-install/native archive smoke                                                   |
| Public schemas/protocols                               | Producer validation + every supported consumer contract test + schema-copy checks                                             |
| Agent adapter                                          | Adapter checks + benchmark integration using fake agents; affected reports/runtime dependents                                 |
| Benchmark coordinator/reporting                        | Runner/evaluation tests + CLI facade/extension consumers as applicable                                                        |
| Extension only                                         | Extension build/test/package checks; CLI compatibility smoke, not unrelated Rust rebuilds by default                          |
| Docs/skills only                                       | Applicable formatting, links/examples, skill discovery validation when affected                                               |
| Lockfiles/toolchains/root configs/workflows/impact map | Broad relevant checks; default full suite until a narrower rule is proven                                                     |

Make selection inspectable: output chosen components and reasons. Validate the
impact map against fixtures for core, CLI, extension, adapter, schema, lockfile,
docs, unknown path and rename/delete changes. Declare workspace dependency edges
for JS imports; do not hide cross-package imports as relative filesystem paths.

For native orchestration, prefer one explicit bridge task calling Cargo workspace
checks rather than duplicate concurrent tasks sharing `target/`. If private
package manifests are used as bridges, they must be non-publishing and their
inputs must cover root Cargo manifests/lock/toolchain, crates, schemas, scripts,
feature flags and target identity. Verify task graph/dry runs for cross-language
changes; no fake runtime dependency on the npm launcher.

## CI and cache correctness

- Keep the PR workflow triggered for every PR. Select jobs within it, then run a
  stable aggregate gate with `always()` and explicit checks for selection errors,
  expected job results, failures and cancellation. A skipped required workflow
  can remain pending. Do not blindly accept all skipped jobs.
- Current CI reports `Check (OS)` on three platforms. Preserve required contexts
  until repository rules are inspected and an owner coordinates any replacement;
  a new aggregate name is not automatically a branch-protection migration.
- Keep manual full-suite execution and full native release/platform validation.
  A docs-only PR must not make published artifact checks disappear from releases.
- Cache Cargo dependencies/compiler work separately from task results. Initially
  disable Turbo output caching for native bridge tasks until OS, architecture,
  toolchain, features and output restoration have verified keys.
- Never cache live benchmark execution, watch, MCP sessions, index mutations,
  publication or deployment tasks. Never put private corpus/index/transcript data
  in shared task caches. Remote caching is optional and disabled initially.
- Do not restore absolute-worktree-path artifacts into another checkout. Keep
  Repoise content caches, mutable index scopes and Turbo task caches distinct.
  Cache environment allowlists must preserve opt-in provider behavior without
  persisting secret values in logs/artifacts.

## Acceptance, validation and rollback

Fresh frozen JS install and direct locked Cargo commands pass. Existing release
tests and three-platform packed consumer/native archive checks pass, including
optional-binary resolution, schema copies and operation without Node for native
archives. Inspect packed file lists for workspace symlinks/private packages.
Prove impact selection and a stable gate for docs-only, core, adapter/extension
and failed/cancelled dependent jobs. Verify POSIX and PowerShell commands.

Update development/build/publishing guides and navigation to the implemented
layout. Do not publish, register an extension, modify repository rules or deploy
as part of this migration. Record exact tool versions, commands and limitations.
Rollback restores the former lockfile/scripts/workflows; no consumer index
migration or npm package-name change should be needed.

## References and remaining decisions

Primary references checked October 19, 2026:
[Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html),
[pnpm workspaces](https://pnpm.io/workspaces),
[pnpm filtering](https://pnpm.io/filtering),
[Turbo structure](https://turborepo.dev/docs/crafting-your-repository/structuring-a-repository),
[Turbo task configuration](https://turborepo.dev/docs/reference/configuration),
[required checks](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

Implementation must settle exact tool versions, bridge representation, required
check rollout, dependency installation policy and which packages are ever public.
Recommendation is pnpm + Turbo; if the bridge prototype adds complexity without
benefit, record evidence and use explicit CI orchestration rather than silently
substituting an unreviewed build system.
