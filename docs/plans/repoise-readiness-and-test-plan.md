# Repoise readiness review and testing plan

Reviewed November 8, 2026 through the connected GitHub account. Source pinned to [`6454f5c039454d2fff111cc92c868224aa0ead96`](https://github.com/billy-the-ape/repoise/commit/6454f5c039454d2fff111cc92c868224aa0ead96), current main at review time. No repository, device, model or service changes were made.

## Decision

Start heavier functional testing now. Before investing in a large benchmark experiment, complete three focused reliability/measurement PRs below. Make the extension installable in a fourth PR before asking ordinary users to evaluate it. Do not wait for the extension benchmark UI, additional agent adapters or a service/daemon.

The architecture is in a good state for testing: shared Rust services, thin CLI/MCP/extension adapters, transactional publication, exact-read validation, isolated mutable worktree scopes, shared immutable caches, authoritative writer locks, bounded code chunks, live config reload, version-pinned agents and server-side retrieval auditing. This review found no reason for a broad architectural rewrite. It is a source/CI review, not an exhaustive audit or a new live runtime certification.

The remaining uncertainty is mostly operational experience and measured effectiveness. The previous pilot was four tasks × three conditions × one repetition. It established mechanics, not a reliable speed/token advantage. In that run lexical was slower on every task; that observation is a reason to measure carefully, not a conclusion about the product.

## Current usability

| Surface | Assessment | Practical implication |
| --- | --- | --- |
| Native CLI / stdio MCP | Ready for controlled functional testing | Use a release build pinned to the reviewed/fixed SHA. Follow init → doctor → index → check → search → read. |
| Published install | Behind main | The latest GitHub release is alpha.1. The in-repo alpha.2 candidate contains later contracts and fixes. The documented alpha.1 npm quick start does not test the current candidate. |
| VS Code operations extension | Implemented, suitable for development testing | Status, indexing progress, watch supervision and MCP definitions exist. CLI is separately installed and capability-checked. |
| Ordinary extension installation | Incomplete | No VSIX packaging command/artifact or complete install-and-use walkthrough. No live extension-host certification established by this review. |
| Extension agent integration | Client-dependent | VS Code MCP registration does not automatically configure Cline, Codex or every other agent. Verify the actual client's tool calls. |
| Extension benchmarks UI | Deferred | Not needed for CLI-driven benchmarks or core V1 readiness. |
| Offline retrieval evaluator | Useful, lexical only | Frozen fixtures and 60 queries cover provenance, policy, exact lookup, recall and latency. It does not establish hybrid recall or agent gains. |
| Live benchmark runner | Substantial implementation; safeguards below need work | Frozen suites, randomized sequential trials, grading, paired task statistics and reports exist. |

## Findings before serious benchmark runs

### 1. Current main has a failed Windows CI job

[Run 42146150531](https://github.com/billy-the-ape/repoise/actions/runs/42146150531): Linux, macOS and Docs passed; Windows failed in JS/TS tests, so later Windows steps were skipped. The extension's 66 tests, Cline adapter's 23 tests and root's 71 tests passed in that job. Protocol conformance passed 28/29.

Failure: `host sweeps the recorded child group after a protocol violation`, assertion `heartbeat ran before the protocol-violation kill`, `test/conformance.test.mjs:254`.

The fixture spawns a detached heartbeat process, waits a fixed 200 ms and emits an oversized protocol line. The heartbeat itself waits for a 50 ms interval before its first write. The failed assertion checks startup before the kill, not whether a live grandchild survived cleanup. A startup race is the likely explanation, not a proven Windows process cleanup defect. Resolve it with an explicit readiness handshake and retain the cleanup assertion.

Source: `packages/agent-adapter-protocol/test/{conformance.test.mjs,fixtures/fake-protocol-leak.mjs,fixtures/heartbeat.mjs}`.

### 2. Benchmark indexing setup is outside cancellation/watchdog protection

`run_trial` calls `build_trial_index` before entering `run_host`. `build_trial_index` uses blocking `Command::output()` without the interrupt flag or a setup deadline. The agent watchdog starts later in `run_host`; the experiment budget is checked between trials. A stuck setup child can therefore hold a run beyond the agent limit and delay a handled interruption. Provider request timeouts reduce some cases but do not bound the setup process itself.

Keep setup outside agent timing, but bound and cancel it explicitly before unattended or long runs. Audit other setup subprocess waits in the same scoped change.

Source: `crates/repoise-core/src/bench/runner.rs` (`run_trial`, `build_trial_index`, `run_host`) and `bench/mod.rs` scheduler.

### 3. The quality gate can pass on insufficient or unverified evidence

`quality_gate` evaluates correctness over graded trials and improvements over available paired task means. It has no minimum paired task/repetition coverage check and does not consume the retrieval audit. Thus a completed fast/correct subset can pass while other tasks error or remain ungraded. The existing partial-candidate test fails because the surviving speed gain is only 5% against a 10% threshold; it does not prevent a pass if the surviving gain is 50%. This is a source-derived counterexample, not a newly executed regression test.

Likewise, the audit records runtime degradation and missing tool use, but those facts alone do not block a quality-gate pass. Retain descriptive statistics, but require explicit evidence sufficiency before treating the result as support for a performance claim.

Do not solve this by deleting no-tool trials: offering a tool that the agent ignores is a real workflow outcome. Report all assigned trials as the primary workflow comparison; label an observed-use subset separately. A hybrid engine claim additionally requires actual hybrid searches without silently pooled lexical fallback. Unknown audit evidence stays unknown.

Source: `bench/analysis.rs` (`paired_analysis`, `condition_grading`, `quality_gate`) and `bench/runner.rs` (`summarize_mcp_audit`, `mode_record`).

### 4. Easy extension testing requires packaging, not more core features

`apps/vscode/package.json` exposes build and test scripts only. `docs/vscode.md` describes development testing but defers VSIX packaging, remote-host checks and live client compatibility. Add a reproducible local VSIX and a real extension-host smoke before calling installation straightforward. Marketplace publication is a later distribution choice.

## PR order

| Order | PR | Gate |
| --- | --- | --- |
| 1 | Make protocol cleanup fixtures deterministic | Before treating current cross-platform CI as green |
| 2 | Bound and cancel benchmark setup | Before long/unattended benchmark runs |
| 3 | Require sufficient, correctly labeled benchmark evidence | Before relying on automatic benchmark/adoption verdicts |
| 4 | Package and smoke-test the VS Code extension | Before normal user extension testing; independent of PRs 2–3 |

Merge each focused PR before rebasing the next. PR 4 can be implemented independently after PR 1 if extension testing is the immediate priority. No publication, release tag or device changes are included in these prompts.

## Agent prompts — one prompt per PR

### PR 1 prompt: deterministic protocol cleanup fixtures

Work in `billy-the-ape/repoise`, starting from current main. Follow AGENTS.md and scoped instructions. Create one focused Ready for Review PR targeting main using the connected OAuth workflow.

Fix the current Windows conformance failure from run https://github.com/billy-the-ape/repoise/actions/runs/42146150531/job/127657996195. It fails `host sweeps the recorded child group after a protocol violation` at the assertion that a heartbeat existed before cleanup.

Inspect `packages/agent-adapter-protocol/test/conformance.test.mjs`, `fixtures/fake-protocol-leak.mjs`, `fixtures/heartbeat.mjs` and related cleanup fixtures. The protocol-leak fixture currently waits 200 ms after spawning a detached heartbeat before emitting an oversized line. Replace startup timing assumptions with an explicit readiness signal emitted only after a successful heartbeat write. Give readiness a bounded failure path and useful diagnostics. Prefer a private pipe/IPC handshake or a bounded fixture-local signal; do not add timing-sensitive sleep assertions.

Preserve both obligations: demonstrate that the heartbeat really ran, and demonstrate that it stops after protocol-error cleanup. Do not weaken the test to accept an absent heartbeat, skip Windows, increase a sleep as the fix, or silently change production kill behavior. Inspect production cleanup if the deterministic test reveals an actual defect. Ensure fixture failures cannot leave detached processes behind.

Run protocol conformance and affected bridge tests, root formatting, and the required repository checks. Include real Windows CI evidence; if unavailable locally, say so and leave verification to CI. Use a bounded repeated run of the affected test to check stability, not an endless retry loop. Update docs only if behavior changes.

PR description must state the cause/evidence, behavior, validation and an easy reproduction command. Prominently state environment/settings/migrations/deployment requirements (none unless discovered). Do not publish packages, tags or releases. Do not poll CI; the reviewer workflow handles subsequent feedback.

### PR 2 prompt: bounded, cancellable benchmark setup

After PR 1 merges, branch from current main in `billy-the-ape/repoise`. Follow AGENTS.md and create one Ready for Review PR targeting main through OAuth.

Fix benchmark setup subprocesses that can block cancellation indefinitely. Begin with `crates/repoise-core/src/bench/runner.rs`: `run_trial` invokes `build_trial_index` using blocking `Command::output()` before the interrupt-aware host watchdog exists. Inspect `bench/workspace.rs`, `preflight.rs`, and scheduler waits for the same issue and explicitly document the boundary of this fix.

Add a finite documented setup limit, separate from the agent interval, with a validated additive suite setting if needed. Preserve the existing meaning of agent `wallClockMs`; do not charge setup time to agent time. Freeze any effective default/override in the manifest. Pass cancellation/deadline state into setup work. Start owned subprocesses so they and their descendants can be terminated on Unix and Windows. Drain stdout/stderr while waiting so full pipes cannot deadlock; bound captured diagnostics and sanitize secrets. Reuse existing process ownership conventions rather than broad process-name kills.

On interrupt, terminate and reap the setup process tree, release the scope lock, clean the trial workspace, record a stable cancelled/setup reason, mark remaining slots not started, publish the report, and preserve bench interruption exit 4. On setup timeout, record an infrastructure/setup timeout distinct from an agent timeout and continue or stop according to a documented bounded policy. Do not fold setup timeouts into agent latency statistics. Check interruption again before starting agent work. Make any change to the experiment wall-budget semantics explicit rather than silently repurposing that setting.

Add deterministic tests with a setup child that signals readiness then stalls, a child producing enough output to fill pipes, and a descendant heartbeat. Assert bounded cancellation/timeout, no later agent launch, process cleanup, ability to reacquire the scope, consistent report/rebuild facts and retained setup-vs-agent timing. Test successful setup too. Update schemas and `docs/bench.md` in the same PR, plus relevant architecture/automation docs.

Run affected Rust/CLI suites and repository formatting, clippy, workspace tests and rustdoc; add platform CI coverage for process semantics. State unavailable checks accurately. Include required settings/env vars, compatibility, defaults and easy success/cancel/timeout test paths in the PR. No releases, model/device changes or CI polling.

### PR 3 prompt: benchmark evidence sufficiency and condition fidelity

After PR 2 merges, branch from main in `billy-the-ape/repoise`. Follow AGENTS.md and open one focused Ready for Review PR targeting main via OAuth.

Harden performance verdicts, preserving raw measurements. Review `bench/analysis.rs`, `runner.rs`, `mod.rs`, `suite.rs`, report/suite schemas and `docs/bench.md`. The current quality gate can pass from a fast/correct surviving subset while other planned cells error or remain ungraded. It also ignores runtime retrieval audit evidence. Reproduce this with two tasks: baseline completes both; candidate errors on one and is 50% faster/correct on the other, exceeding a 10% improvement threshold. Preserve the existing honest distinction between a wrong answer and infrastructure failure.

Introduce a clearly declared evidence-sufficiency policy: minimum task/repetition coverage and grading completeness, with conservative documented defaults for claim-bearing gates. Explicitly distinguish planned, attempted, completed, graded and paired coverage. Use the frozen task/condition/repetition plan, not the surviving records, as the denominator. Prevent an overall claim-bearing pass when the required coverage is unmet; use `insufficient_data` with exact reasons. Retain useful descriptive paired statistics and censored-timeout semantics. Do not convert infrastructure errors or unknown grades to incorrect answers or zero-valued measurements.

Add a condition-fidelity summary from the existing server audit: configured condition, successful search count, actual modes, degradation, clean no-search, and missing/incomplete/malformed evidence. Keep all assigned trials in the primary workflow result: an agent ignoring Repoise is an outcome, not a trial to delete. Separate any observed-use subset and label its selection bias. A clean no-search trial can inform an offered-tool workflow comparison, but cannot prove hybrid engine effectiveness. A declared hybrid execution claim must fail/be insufficient on fallback or unknown evidence according to a predeclared policy. Do not require Repoise search for baseline or tasks explicitly designated as retrieval controls.

Choose the smallest coherent schema/API extension; preserve compatibility for old reports, explicitly labeling absent new evidence as unknown. Run and offline `bench report` rebuild must produce the same sufficiency/fidelity decisions. Missing records must not shrink the planned denominator. Keep the existing quality metrics distinct from evidence validity if that is clearer, but ensure the combined claim verdict cannot say pass when validity fails.

Tests must include the two-task survivor counterexample, missing repetitions, partial grading, not-started slots, timed-out vs infra-failed trials, absent/corrupt audit, clean no-search, observed true hybrid, observed lexical fallback, mixed successful modes, baseline controls and full valid data. Assert raw counts and descriptive stats remain available, and rebuild reproduces the decisions. Update schemas, guide, changelog and sample suite declarations. No new live model calls are needed.

Run required Rust/CLI/report tests and repository checks. PR description: concrete before/after, compatibility/default policy, required settings/env vars, and easy offline tests. No publication, server changes or CI polling.

### PR 4 prompt: installable VS Code preview and extension-host smoke

Branch from current main after PR 1 in `billy-the-ape/repoise`; rebase on subsequent merged changes as necessary. Follow AGENTS.md. Create one Ready for Review PR via OAuth.

Make the existing AP-07 first-slice extension straightforward to install and test as a user. This is packaging, host integration validation and onboarding, not implementation of benchmark UI or a new backend.

Inspect `apps/vscode/package.json`, `.vscodeignore`, extension activation, commands, MCP registration, `docs/vscode.md`, `docs/automation.md` compatibility checks and CI impact selection. Add a pinned, reproducible VSIX packaging command with an explicit build prerequisite and a consistent preview version/identity. Do not assume the current publisher string is an owned Marketplace account; avoid claiming it is registered. Do not publish or bundle/download a native CLI automatically.

Ensure the VSIX contains compiled runtime modules and appropriate license/README metadata but excludes tests, fixtures, caches, source workspaces and unrelated files. Inspect/list the actual archive and test activation from the packaged artifact rather than relying only on source unit tests. Use a real VS Code extension-host smoke with the existing fake CLI seam where practical: activation, registered commands, status rendering, index event completion, watch start/stop and disposal. Keep trust gating and CLI capability rejection exercised; document any assertions that remain manual. Preserve existing Windows launch and process-tree rules.

Write a short install/use walkthrough: build/install the VSIX; install or point `repoise.cliPath` at the matching alpha.2-or-newer capability-compatible candidate; trust/open a test repository; initialize it through the CLI; show status, index, start watch, edit a fixture, verify freshness, stop watch, and remove/disable the extension. Describe missing/stale/old-CLI errors and recovery. Explain that VS Code MCP discovery and Cline/Codex registration are separate, and demonstrate verifying actual tool use in a selected client. Do not promise all clients are configured automatically.

Add CI packaging/host-smoke coverage to the appropriate affected checks without making ordinary offline CLI tests depend on downloading VS Code. Record local desktop vs Remote SSH/WSL as measured or unmeasured. Update AP-07 status and current guides without claiming the benchmark UI or Marketplace distribution is done.

Validate TypeScript/unit tests, packaging archive contents, host activation and repository formatting. State each platform/host limit. Prominently list required tools/settings and a copy/paste install/test path in the PR description. No releases, marketplace publication, device changes or CI polling.

## Test configurations

Do not run the full cross-product. Separate retrieval quality, agent usefulness and operational reliability.

### Retrieval conditions

| ID | Configuration | Purpose | When |
| --- | --- | --- | --- |
| A | Existing curated docs + normal filesystem/rg tools, no managed Repoise guidance or MCP | Real workflow baseline | Every initial agent experiment |
| B | Same full docs+code corpus; MCP pinned lexical; same underlying index/config as C | Incremental value of lexical Repoise | Initial experiment |
| C | Same corpus; MCP pinned hybrid; embeddings scope docs+code, fixed model/input profile/dimensions | Additional value of vectors | Initial experiment |
| D | Docs-only lexical index | Value of documentation alone | Retrieval evaluation first; later agent ablation |
| E | Full lexical corpus, embeddings scope docs only | Whether code vectors justify their cost | After B/C, on frozen code/concept queries |
| F | Vectors-only retrieval | Diagnose vector quality vs fusion | Diagnostic query evaluation, not initial user default |
| G | History lane off vs opt-in local history | Value on change-history tasks | Separate bounded experiment |

B and C can share a hybrid-capable index, with MCP search mode pinned differently; note that B's setup may still build/reuse embeddings. This isolates query retrieval mode, not lexical-only installation/indexing cost. Measure a genuinely embedding-disabled configuration separately for offline deployment cost.

The current suite conditions select retrieval/search mode, not a different corpus/config per condition. D and E require separately frozen configurations/snapshots and separate suites (or a later intentionally designed config axis). Do not invent a per-condition preset flag. Keep baseline corpus access equal through ordinary tools, remove managed Repoise-only instructions from baseline, and inspect unmanaged instructions/other MCP servers for contamination.

### Operational matrix

| Axis | Minimum configurations | Measurement |
| --- | --- | --- |
| Installation | Pinned native archive; pinned npm launcher; packaged VSIX + selected CLI | Fresh install/version/capabilities and CLI/MCP smoke |
| OS | macOS ARM64, Linux x64 GNU, Windows x64 | Functional portability; benchmark effects primarily on one fixed host |
| Working state | Working tree, committed snapshot, detached linked worktree | Scope/freshness and expected-hash read validation |
| Caches | Empty index + empty family cache; new worktree + warm family cache; existing index no-op; one edit/rename/delete | Setup wall time, RSS, disk, provider requests/cache hits |
| Watch | Idle; single edit; burst; branch/config change; invalid config/recovery; second writer; kill/restart | Idle CPU/RSS, edit-to-publication latency, no partial generations, lock recovery |
| Embeddings | Healthy; missing credentials; outage; wrong dimension/profile; partial budget | Visible degradation/refusal and audit classification; never pool with successful hybrid |
| Extension | Single root; multiple roots; two windows; missing/old CLI; restricted workspace; close/remove root | Watch ownership/cleanup, status and actual MCP client wiring |
| Corpus | ai-gateway TS/JS first; Repoise Rust/fallback second; bounded public Python repo; later a larger monorepo | Per-corpus quality and scaling; do not assume structural parsing for Rust/Python |
| Model/harness | One pinned Cline + coding model first | Add a second model and then a second adapter only after measurements work |

For caches, record parse/vector family-cache state separately from mutable index state. A new worktree is not a cold cache. Record inference prompt-cache and model residency separately too. No global cache purges are needed: use disposable isolated locations.

## Staged execution after preparation

1. **Functional acceptance and frozen candidate.** Fix PRs 1–3 and pin the resulting source SHA. Use a release build; retain version/capabilities, build options and artifact checksum. Run required checks and offline lexical evaluation. Complete one fresh CLI/MCP path and the operational smoke on a disposable consumer checkout. PR 4 gates the equivalent extension acceptance. Expected: no policy/provenance failures, exact lookup 100%, no orphaned writers, useful diagnostics.
2. **Retrieval-only quality.** Re-run frozen lexical fixtures; add a separately frozen, manually labeled real-repository query set including exact names, paraphrases, cross-file concepts, config, giant symbols, duplicates, docs vs plans, absent answers and secret denial. Fix gold paths/ranges before inspecting candidate rankings. Compare B/C and later D/E/F with the same questions. Record recall@5, exact lookup, provenance, degradation, end-to-end CLI and persistent MCP timing separately. Measure query embedding separately when possible; do not infer its latency by subtraction from unrelated runs. Existing harness is lexical only: do not present a lexical run as hybrid evidence.
3. **New live mechanics smoke.** Four tasks × A/B/C × one repetition = 12 trials, plus a separate setup-cancel and agent-cancel check. Use fresh tasks where the original smoke answers are now tuning knowledge. Review raw requests, grading and audit, not just the report header. Require true hybrid searches on the designated hybrid probe; keep an easy no-retrieval control. Check report rebuild consistency. Do not reuse old pilot results as evidence for the new candidate.
4. **Engineering pilot.** Eight varied tasks × A/B/C × three repetitions = 72 sequential trials. Include held-out tasks and at least two baseline-friendly tasks. Inspect variance, failure rate and tool uptake. This estimates budget/variance; it is not a guaranteed statistical power level. Do not tune on held-out results.
5. **Main controlled experiment.** After the pilot is sound, 15–20 tasks × A/B/C × five repetitions = 225–300 trials. Freeze tasks, correctness rubrics, held-out split, condition ordering seed, practical thresholds, coverage policy and budgets before starting. Use task-cluster intervals and per-task effects. If using 20% token reduction/no correctness decrease as the adoption target, declare it in advance; speed improvement is a separate outcome, not an automatic requirement that every condition win. Report held-out results separately from tuning results.
6. **Focused expansion and V1 decision.** Repeat the most informative comparison on another corpus, then a model variation, then another harness if justified. Test actual packaged installs on supported OSes. Decide stable scope from measured reliability and product gates. The extension benchmark UI, history enrichment and additional languages/adapters can remain explicitly deferred.

This is a sequence to step through, not authorization to launch hundreds of model calls now. At execution time select the actual consumer SHA, endpoint/model/harness pins and run budget. Keep suites/rubrics/artifacts outside agent-visible repositories. No device settings need to change to prepare the experiment.

## Experiment controls and interpretation

- Prefer ai-gateway first because it is a real consumer, but select a fresh immutable commit and remove obsolete exclusions/role-remapping workarounds after validating current scope coverage. Preserve the historical pilot unchanged.
- Pin exact CLI/Cline/adapter/model artifacts, quantization, context, reasoning, compaction, input templates, vector dimensions and fusion settings. Record hardware/driver/server configuration. Your dual-A6000 system is a new hardware baseline relative to the old smoke; do not compare old and new wall times as a retrieval effect.
- Run sequentially and pause unrelated inference during controlled measurements. Record model load/eviction and residency; do not mix server tuning with retrieval changes. Fresh conversations do not guarantee cold prompt caches.
- Keep normal fallback tools available across A/B/C. Change only the intended retrieval availability/guidance. Record tool descriptions and prompt budgets. Do not install new instructions midway through a frozen experiment.
- Evaluate answer correctness using deterministic checks where sufficient and blinded human rubrics for semantic explanations. Current deterministic grading is substring-based; it does not prove a patch compiles or fixes behavior. For coding tasks, retain workspaces and independently run trusted tests outside the timed trial using an explicitly approved safe process; the current runner does not implement a hidden executable-test sandbox. Start with read-only tasks to avoid conflating this missing mechanism with retrieval quality.
- Report setupMs, agentMs and totalMs separately, with indexing/provider costs, completion rate, correct-completion rate, timeouts and infrastructure errors. Keep usage counters labeled measured/estimated; unknown cost/tool telemetry stays unknown. Do not optimize solely for fewer tokens at lower correctness.
- Analyze all assigned trials for workflow effectiveness. Observed-tool/hybrid subsets are supplementary. Missing audit, no calls and lexical fallback are different states. Do not silently drop difficult failures or rerun only losing conditions.
- Current gate percentages and bootstrap intervals are descriptive. A small p-value is not supplied by the runner, and a qualityGate pass alone is not proof of V1 readiness. Use measured coverage, failure rates, uncertainty and practical effect sizes.

## V1 exit criteria

Core correctness: no observed cross-scope/stale/secret leaks; exact reads/provenance correct; supported installation forms work; watch/config/recovery/ownership behavior passes; retained benchmark evidence is reproducible.

Retrieval targets from the existing runbook: exact lookup 100%; zero policy leaks; held-out conceptual recall@5 at least 85%; lexical warm CLI p95 at most 500 ms. Hybrid targets remain separately measured: code vectors +10 recall@5 points where their extra cost is justified, and p95 search at most 2 s excluding query-embedding requests. State the timing boundary and host for every target.

Agent adoption target: predeclared token reduction (currently 20% median per-task input reduction in the plan) without correctness loss, with adequate coverage and task-level uncertainty. A reliable lexical core can still be a useful release if embeddings do not win; changing the original product gate/scope is an explicit decision, not relabeling a failed experiment as passed.

## Source map

- [Current source](https://github.com/billy-the-ape/repoise/tree/6454f5c039454d2fff111cc92c868224aa0ead96)
- [VS Code guide](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/docs/vscode.md)
- [Benchmark guide](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/docs/bench.md)
- [Benchmark runner](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/crates/repoise-core/src/bench/runner.rs)
- [Benchmark analysis](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/crates/repoise-core/src/bench/analysis.rs)
- [Offline evaluation runbook](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/scripts/eval/RUNBOOK.md)
- [Historical pilot](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/docs/pilot.md)
- [Candidate release status](https://github.com/billy-the-ape/repoise/blob/6454f5c039454d2fff111cc92c868224aa0ead96/docs/publish/alpha-2-candidate.md)

Validation limits: no source edits, new compiled test run, installed VS Code interaction, live inference benchmark or device inspection were performed in this review. CI results above are observed remote evidence. Benchmark counterexamples and setup risks are source-derived findings to turn into regression tests in the proposed PRs.
