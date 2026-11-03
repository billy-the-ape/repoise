# 0.1.0-alpha.2 candidate readiness record

Prepared, **not published**. This is a distinguishable candidate so that
installed CLIs stop identifying as the published 0.1.0-alpha.1 (which
predates the AP-02 automation contracts) while AP-07 and other automation
work proceeds. It is a prerelease candidate, not a stable v1 release, and no
v1 adoption gate is declared passed here.

## Candidate identity

- Version: **0.1.0-alpha.2**, chosen as the next unused prerelease after
  inspecting the available read-only evidence: the Git tags stop at
  `v0.1.0-alpha.1`, and the npm registry versions of all four packages
  (`repoise`, `repoise-linux-x64-gnu`, `repoise-darwin-arm64`,
  `repoise-win32-x64`) stop at `0.1.0-alpha.1` (plus the historical
  `0.0.0-stage`/bootstrap entries). It is not 1.0.0: compiling code does not
  satisfy the adoption gates below.
- Every coupled version is synchronized: the Cargo workspace (and lockfile),
  the root development manifest, `pnpm-lock.yaml`, the launcher manifest and
  its exact optional native pins, and the two private adapter packages.
  `verifyVersion` (`scripts/release/common.mjs`) enforces this and the
  license/schema-copy equality.
- The candidate is distinguishable by identity (`repoise --version`,
  `capabilities --json` `version`) and, authoritatively, by capability
  negotiation: the installed CLI must expose the
  [AP-07 first-slice contract](../automation.md#ap-07-first-slice-contract).
  Compatibility consumers must reject alpha.1-era handshakes with upgrade
  guidance and must not infer support from a version string.

## What this candidate adds over the published alpha.1

Everything merged on main since alpha.1: the AP-02 CLI automation contracts
(capabilities, `--events` streams, stable exit codes, signals, the
authoritative one-writer-per-scope OS lock), the pre-v1 reliability fixes,
the live phase progress reporting, AP-03 (agent adapter protocol and the
first Cline adapter), AP-04/AP-05 (`repoise bench`, suites, independent
grading, reports), the installable benchmark companion, the server-side
retrieval audit, the benchmark harness version pin, the merged scanner race
fixes and the retrieval-audit hardening. See the [Changelog](../../CHANGELOG.md)
and the completed plans under `docs/plans/complete/`.

## Evidence states (kept distinct; unknowns stay unknown)

| Claim | State |
| --- | --- |
| Source implementation on main | Implemented and CI-checked per PR; this PR adds the candidate preparation and contract fixtures |
| Packed-artifact validation | This PR's CI runs formatting, release-tool tests, Rust checks and the extended offline packaged-consumer smoke (capabilities, `index --events`, `watch` ownership/cancellation, old-CLI compatibility fixtures) against both installed forms on all three supported OSes; **unmeasured until the run completes** |
| Fresh public-registry installation | **Unmeasured.** 0.1.0-alpha.1 remains the published version; this candidate has no registry presence, tarball or tag until an authorized publication occurs (alpha.1's own Linux/Windows fresh registry installs also remain unmeasured) |
| Benchmark smoke | The AP-06 12-trial smoke (2026-10-30, see [pilot status](../pilot.md)) verified mechanics only (one repetition, no statistics) on the harness of that date; it is not evidence for this candidate |
| Adoption/performance gates | The [master plan section 11](../plans/v1_master_plan.md#11-evaluation-and-adoption-gates) targets remain proposed targets; none are measured or passed here, and the hybrid/code-vector efficiency claims remain unproven until controlled evidence supports them |

## Proposed stable-core scope (pending a deliberate owner decision)

Proposed, not decided: if a narrower V1 release is wanted while the section 11
performance/adoption gates remain open, that is a **scope decision requiring
explicit owner approval**, not a completed release. The proposed stable core
would be the offline, always-available surface: configuration, doctor,
offline docs+code lexical indexing, freshness/status, lexical search, exact
reads, check, watch with the ownership lock, the stdio MCP server (read-only
default), overlay, purge, purge-safe worktree isolation, and the
offline-capable benchmark coordinator. The opt-in surfaces (remote embeddings,
GitHub enrichment, hybrid search) would ship behind their existing
capability flags and measured budgets. A stable tag must not be minted until
the section 11 gates are either measured and passed or explicitly waived by
the owner.

## What still remains before this candidate can be published

No publication, staging registry write, tag push or workflow dispatch is
performed by this PR. The later, separately authorized publication follows
the standard runbook in [publishing](README.md):

1. Merge this PR and any dependent work into main, then tag the merged
   commit `v0.1.0-alpha.2`.
2. Dispatch **Release preparation and staging** from `main` with
   `tag: v0.1.0-alpha.2`, and `stage_npm`/`draft_native` as required.
   Validation-only dispatches publish nothing.
3. Review the immutable `release-bundle` (source SHA, version, all seven
   payload checksums), approve the staged npm jobs (natives first, launcher
   last) and the native draft stages with 2FA, then publish the draft.
4. Verify a fresh pinned public-registry install on each supported platform
   (currently measured: macOS ARM64 for alpha.1; Linux/Windows fresh
   registry installs unmeasured), and record the results in a new
   execution-record document alongside this one.

Settings/approvals required: the `npm-release` environment approvals in
Review deployments; the OIDC staging failure from alpha.1 (ENEEDAUTH)
remains unresolved — until it is, staging must use the maintainer-local
fallback recorded in [alpha-1 status](alpha-1-status.md). No new
credentials or device changes are introduced by this PR.
