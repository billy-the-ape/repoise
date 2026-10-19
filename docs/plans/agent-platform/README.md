# Agent platform and evaluation roadmap

Status: proposed implementation program; no commands or packages below ship from
this planning change. Recorded October 19, 2026 (America/Denver).

## Purpose

Make Repoise easy to operate and measure from a terminal, automation, and VS Code.
Preserve the consumer's real agent loop while comparing repository retrieval
conditions, models, harnesses, and repository types. Keep indexing/search in Rust,
agent-specific integration in adapters, and editor presentation in an extension.
The first implementation is the workspace/CI foundation, before adding the new
layers. This PR records plans only; it does not migrate the workspace, run live
agents, change devices, or publish packages.

## Current evidence and boundaries

Baseline inspected: main `fbdc79b80c38ebe593b07e28976c3939356f65cf`.

- Existing Cargo workspace: `crates/repoise-core` and `crates/repoise-cli`.
- Existing npm launcher: `npm/repoise`; platform packages are assembled by
  `scripts/release/`, not independent checked-in source packages.
- Shared CLI/MCP services, scope-bound stdio MCP, explicit watch, optional vectors.
  MCP search defaults to lexical; indexing vectors does not select hybrid search.
- `scripts/eval/` already has frozen synthetic retrieval fixtures and a strict
  real-agent replay import schema. It does not launch live agents. See its
  [runbook](../../../scripts/eval/RUNBOOK.md).
- Earlier ai-gateway/Cline trials verified useful operation but did not establish
  consistent speed/token savings. Current installation/index completion is not
  benchmark evidence. No new results are asserted here.
- TS/JS has structural parsing; other code languages currently use line windows.

The [v1 master plan](../v1_master_plan.md), especially section 11, retains its
existing adoption gates. These documents extend execution planning, not readiness
claims or gate thresholds. Runtime guides remain authoritative for shipped usage.

## Work packages and order

Each row is an intended reviewable implementation PR. Read the assigned document
and its listed dependencies rather than loading every plan. Update statuses and
links as implementation lands. Split a card if its implementation exceeds a
reviewable scope, retaining the same interfaces and acceptance requirements.

| ID    | Plan                                                        | Depends on                              | Intended result                                                                   |
| ----- | ----------------------------------------------------------- | --------------------------------------- | --------------------------------------------------------------------------------- |
| AP-01 | [Workspace and CI](01-workspace-and-ci.md)                  | This plan                               | Incremental Cargo + pnpm/Turbo workspace, affected checks, packaging preservation |
| AP-02 | [CLI automation contracts](02-cli-contracts.md)             | AP-01                                   | Versioned events, capabilities, progress and cancellation for automation/UI       |
| AP-03 | [Agent adapter protocol](03-agent-adapters.md)              | AP-01, AP-02                            | External runner protocol, conformance fixtures and first Cline adapter            |
| AP-04 | [Benchmark execution](04-benchmark-runner.md)               | AP-02, AP-03                            | Isolated trials and verified retrieval conditions with budgets/artifacts          |
| AP-05 | [Suites, metrics and reports](05-suites-metrics-reports.md) | AP-04; define schema with AP-03         | Independent grading, per-trial records and comparable reports                     |
| AP-06 | [Pilot and expansion](06-pilot-and-expansion.md)            | AP-04, AP-05                            | ai-gateway pilot, then broader repository/model/harness experiments               |
| AP-07 | [VS Code extension](07-vscode-extension.md)                 | AP-01, AP-02; bench UI also AP-04/AP-05 | Thin workspace UI and supervised watch/MCP setup                                  |

AP-07 watch/status UI can land before the benchmark UI, but no editor-specific
benchmark engine should be introduced. AP-06 consumer integration belongs in a
separate consumer PR; the harness stays in Repoise.

## Program decisions

- Use one repository. Retain Cargo's Rust ownership; recommend pnpm workspaces
  with Turborepo for JS/TS task orchestration. Turbo does not replace Cargo or
  infer the Rust dependency graph. AP-01 records alternatives and migration risks.
- Implement CLI capabilities before editor features. Keep package-manager
  installation and native operation; the extension discovers an installed binary.
- Name the proposed command `repoise bench`, avoiding confusion with project
  tests. It coordinates trials, never implements a competing LLM reasoning loop.
- Use executable agent adapters with JSON requests/events. Environment variables
  may select runners or credentials; they are not the entire integration contract.
- Start with Cline, one repository and a fixed model. Add dimensions progressively.
  Normal filesystem fallback remains available in Repoise conditions.
- Grade correctness independently and preserve failures. Faster wrong answers do
  not count as a successful optimization. Report unknown metrics as unknown.
- No mandatory hosted service, GPU, paid cache, new production credentials, or
  Node dependency for native lexical indexing/search.

## Coordination with other work

At inspection these PRs were open, not assumed merged:

| Work                                                                                 | Coordination requirement                                                                                                  |
| ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------- |
| [#24 watch embeddings](https://github.com/billy-the-ape/repoise/pull/24)             | Preserve synchronous publication/coverage/cancellation behavior; structured events must reflect the merged implementation |
| [#25 release tooling](https://github.com/billy-the-ape/repoise/pull/25)              | Rebase package-manager migration against current release paths and verification scripts                                   |
| [#26 EmbeddingGemma input profile](https://github.com/billy-the-ape/repoise/pull/26) | Record exact embedding input profile, fingerprint, dimensions and degradation in trials                                   |
| [#27 installable agent skill](https://github.com/billy-the-ape/repoise/pull/27)      | Retain `skills/` discovery paths; hash guidance versions and remove only Repoise guidance from baseline trials            |

Recheck these before implementation; paths/state can change. Earlier discussion
also proposed cross-worktree content-cache reuse and indexing percentage/ETA.
Those are coordination requirements, not claims of shipped functionality. Reuse
eligible immutable cached inputs, never another worktree's mutable index identity.
AP-02 coordinates progress work rather than creating a second event design.

## Questions and decision ownership

| Question                                                   | Default / decision point                                                                           |
| ---------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Workspace tool and exact versions?                         | Recommend pnpm + Turbo; AP-01 pins compatible versions after cross-platform proof                  |
| Native or optional companion benchmark runtime?            | AP-03/AP-04 prototype; native CLI remains entry point, any Node companion is explicit and optional |
| Which Cline version and telemetry are reliable?            | AP-03 capability probe and installed-version verification; no assumed CLI/extension equivalence    |
| Which model/hardware/budgets and pilot snapshot?           | AP-06 owner-selected profile; do not reuse old pilot SHAs silently                                 |
| Exact condition enforcement mechanism?                     | AP-04 records actual mode/degradation and fails unsupported conditions                             |
| Correctness threshold, evaluator and uncertainty?          | AP-05 predeclares grading and margins; preserve v1 gates without inventing statistical proof       |
| Which agent clients share VS Code MCP registration?        | AP-07 explicit tested compatibility matrix; no universal integration claim                         |
| Extension identity, distribution and independent versions? | AP-07 release design; registration/publication needs separate authorization                        |
| Where do private suites/transcripts/results live?          | AP-04/AP-05 local configured directories, excluded from indexing and public artifacts              |

## Completion discipline

Each implementation PR updates current-state guides, this index, operational
requirements and actual validation evidence. Proposed examples must remain labeled
until implemented. Keep public fixtures synthetic, never commit indexes, private
prompts/transcripts or credentials. Device changes require a separate authorized
deployment and a final homelab-documentation update; this program authorizes none.
