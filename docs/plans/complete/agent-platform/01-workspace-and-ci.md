# AP-01: workspace foundation and affected CI

Status: implemented (2026-10-20); see the [implementation record](#implementation-record-2026-10-20)
at the end of this document for the shipped design, validation and deferred items.
Parent: [program index](../../agent-platform/README.md).

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

## Implementation record (2026-10-20)

Implemented as one reviewable PR against `main`
([#36](https://github.com/billy-the-ape/repoise/pull/36)). Pinned versions: pnpm 12.3.4
(root `packageManager`, Corepack-managed), Turborepo 2.11.9 and Prettier 3.9.9
as root dev dependencies, Node 24 in CI; Rust stays pinned at 1.99.0.

Shipped changes:

- `pnpm-workspace.yaml` — explicit membership: the root development package
  (listed so Turbo schedules its scripts; pnpm already treats the root as a
  member) and the reserved `apps/*`/`packages/*` homes. The published launcher
  (`npm/repoise`) deliberately stays outside the workspace: its optional
  platform-package pins only resolve once those versions are published, so a
  root frozen install (bump PRs and the release build job) must never depend
  on them. Generated staging (`dist/`), evaluation fixtures and examples stay
  out.
- Root `package.json` — `packageManager` pin, Turbo task scripts
  (`format`, `format:check`, `test`, `check:native`, `build:native` and their
  `:windows` variants), and Prettier globs extended to `scripts/ci`,
  `turbo.json` and `pnpm-workspace.yaml`.
- `turbo.json` — top-level `globalDependencies` covering Cargo
  manifest/lockfile/toolchain, `crates/**`, `schemas/**` and the
  check/build scripts (Turbo 2.x removed the per-task `globalDependencies`
  key); every declared task uses `cache: false` (verification tasks
  included, so a stale cache can never skip a format/test/check run), so no
  Turbo task-output cache exists for any task.
- `pnpm-lock.yaml` replaces `package-lock.json`; `.npmrc` sets
  `ignore-scripts=true` for development installs.
- `scripts/ci/impact-map.json` (version 1) — versioned component map with
  `check`/`docs` job groups; reserved adapter/extension homes and the
  `root` component default to broad checks until their packages ship.
- `scripts/ci/select-checks.mjs` — PR merge-base diff via `git diff -z
  --name-status` (rename/copy/delete/type-change entries, paths with
  spaces), component classification, `GITHUB_OUTPUT` (`ok`, `run_check`,
  `run_docs`, `components`, `reason`). Type changes (`T`) classify like
  modifications. Empty diffs, unknown paths, missing merge bases
  and any script failure select the full suite; failures also exit nonzero.
- `scripts/ci/gate.mjs` — stable aggregate gate: a skipped selection means
  the full suite was required; a selection error fails the gate (the full
  suite still runs); required jobs must have succeeded and non-required jobs
  must have been skipped.
- `.github/workflows/ci.yml` — `Select checks` (pull requests only), the
  three `Check (OS)` jobs with unchanged display names, `Docs`, and `CI
  gate`. Each conditional job declares `needs: [select]` (required so its
  `if:` can read the selection outputs) and is guarded by a quoted
  `!cancelled() &&` expression so superseded runs under `cancel-in-progress`
  do not start macOS/Windows runners; `CI gate` keeps `always()`. Every job
  that uses pnpm installs it with `npm install --global pnpm@12.3.4`
  (lifecycle scripts enabled, run from `runner.temp` outside the checkout)
  after `setup-node`: pnpm 12's postinstall swaps the shebang-less
  placeholder bin for the native `@pnpm/exe` binary, and a follow-up step
  asserts `pnpm --version` equals the pinned version so a broken or no-op
  shim can never report green. Skipping scripts was tried first and failed:
  the placeholder is unrunnable under direct `execve` (Turbo's task spawn;
  `Exec format error` on Linux) and a silent exit-0 no-op on Windows, while
  `pnpm/action-setup@v4` shims broke with pnpm 12 on Windows. `setup-node`
  uses no pnpm store cache. Frozen
  `pnpm install`, Turbo for format, tests and the per-platform native bridge
  tasks, and separate `actions/cache` entries for `target` and `~/.cargo`
  keyed by OS/arch/`Cargo.lock` (Turbo caches no task outputs).
- `.github/workflows/release.yml` — build job installs with frozen pnpm;
  registry packing/staging still invoke real npm.
- `scripts/release/common.mjs` — `resolveNpmCli` honors `npm_execpath` only
  when it resolves to `node_modules/npm/bin/npm-cli.js`, otherwise searches
  PATH (Windows `npm.cmd` maps to `npm-cli.js`); tests cover a pnpm-style
  execpath, a real npm execpath, the Windows shim and the missing-npm error.
- Tests: `scripts/ci/select-checks.test.mjs` (parser, classification
  fixtures, and end-to-end selection against a temporary Git repository
  including renames, spaced paths, missing bases and empty diffs) and
  `scripts/ci/gate.test.mjs` (selection errors, skipped selection, failures,
  cancellations, stray non-required jobs, missing outputs).
- Docs: AGENTS.md, `docs/development.md`, `docs/publish/README.md`,
  `docs/publish/npm.md`, CHANGELOG and the program index.

Validated locally (Linux x86_64, Node 24, pnpm 12.3.4, pinned Rust 1.99):
fresh frozen `pnpm install`; `pnpm run format`, `pnpm run format:check`,
`pnpm test` (56 tests, 15 new); Turbo dry-run proving root tasks execute and
every declared task is declared with caching disabled and full Rust input
coverage; live selection runs against repository history (multi-component,
empty-diff and missing-base behavior); `pnpm run release:claim` proving the
real-npm fallback under a `pnpm run` environment where `npm_execpath`
points at pnpm's own CLI; full `bash scripts/check.sh` (fmt, Clippy, tests,
rustdoc) and `bash scripts/build.sh` (release build) for the native path.

Deferred / not shipped with this PR:

- Turbo remote caching and cross-PR local cache reuse; the plan's
  cache-key verification gate is deferred with the cache itself.
- Required-check configuration in repository settings for the three
  `Check (OS)` contexts plus the `CI gate` job (administrative step; no
  workflow change is required for it).
- Live GitHub PR runs demonstrating skipped vs executed jobs; selection and
  gate semantics are covered by the unit/E2E tests above.
- Live confirmation of the scripts-enabled `npm install --global
  pnpm@12.3.4` provisioning path on the hosted Linux and Windows runners
  (earlier attempts observed the `pnpm/action-setup@v4` shim failure and,
  with scripts skipped, Turbo's `Exec format error` on Linux and silent
  exit-0 pnpm no-ops on Windows; the current install is verified locally on
  Linux via a raw-`execve` run of `pnpm exec turbo`).
- Hybrid evaluation and the AP-02–AP-07 scope.
