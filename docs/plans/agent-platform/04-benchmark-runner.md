# AP-04: benchmark orchestration and trial isolation

Status: implemented (2026-10-27); see the [implementation
status](#implementation-status) below for the shipped design, validation and
deferred items. Depends on [CLI contracts](02-cli-contracts.md) and
[agent adapters](03-agent-adapters.md); coordinate schemas with
[grading/reporting](05-suites-metrics-reports.md).

## Implementation status

Shipped as the `repoise bench` command in `crates/repoise-core/src/bench/`
with the Node host bridge
`packages/agent-adapter-protocol/bin/adapter-host-run.mjs` (tested by
`packages/agent-adapter-protocol/test/bridge.test.mjs`). The current-state
guide is [docs/bench.md](../../bench.md). Shipped behavior:

- Suite manifest (schema v1, `schemas/bench-suite.v1.schema.json`), strictly
  parsed; suite/prompt files must live outside the benchmarked repository.
- Preflight (exit 3): reproducible snapshot (clean commit or recorded patch,
  with the resolved Repoise cache directory excluded from dirt), node/bridge/
  adapter resolution, `env:`/`credentialRef` resolution (names only), hybrid
  vector-coverage gate, root status and guidance captured into the immutable
  `manifest.json`.
- One disposable linked git worktree per trial at the frozen detached
  revision; the user's checkout is never reset. `repoise` conditions get a
  trial-local index build (setup timed separately) plus a read-only
  `repoise mcp` server bound to the trial worktree and pinned to the
  condition's declared `searchMode` with `--search-mode` (a tool request
  naming a different mode is refused, so a hybrid run cannot silently
  degrade); the effective mode and any degradation are recorded. Baseline
  trials remove only the managed
  overlay from the disposable worktree.
- Trials run through the AP-03 host (bridge) only; the coordinator sends the
  cooperative `cancel` line on SIGINT/SIGTERM, escalates after grace, and
  force-kills the bridge and its recorded process tree at the outer
  wall-clock deadline. Remaining trials are recorded `not_started`; exit 4.
- Sequential scheduling with per-block seeded condition order; per-trial
  limits forwarded to the adapter (wall clock always enforced by the
  coordinator); `experimentWallClockMs` budget with published report.
- Artifacts: `manifest.json`, `report.json` (`qualityGate: not_evaluated`),
  per-trial `trial.json`/`request.json`/`adapter/` (`events.ndjson`,
  `result.json`, `stderr.log`) and `host.log`.

Design deltas from this plan document: the actual surface is
`repoise bench [SUITE]` (the suite manifest file replaces the proposed
`--conditions/--repeat` flags; `--dry-run`/`--json`/`--runner`/`--repeat`/
`--seed`/`--output` flags exist as documented in `docs/bench.md`), and the
"native or optional companion" question resolved as native orchestration
with the AP-03 host as the only Node piece. Post-review hardening
(2026-10-27, two rounds): task/condition ids are restricted to
`[A-Za-z0-9_-]` (trial ids and artifact directories derive from them), a
failed patch snapshot removes the partially created worktree,
post-workspace failures (request write, host bridge, trial-record write)
record an `infra_error` trial with cleanup so the report is always
published, and `report.json` `startedAtUtc` is captured at experiment
start. Second round: searches omitting `mode` on a hybrid/vectors-only
pinned MCP server use the pinned mode (only explicitly different modes
are refused); the seeded condition shuffle advances its splitmix64
stream (one draw per swap); and the experiment `id` is restricted to
`[A-Za-z0-9_-]` so it cannot escape the artifact output root. Third round:
a hybrid/vectors-only trial whose trial index has no published vectors
fails setup as `infra_error` (no silent lexical degradation — `repoise
index` exits 0 even when embeddings fail or stay pending, and the pinned
MCP server would otherwise answer lexically); the host bridge scans the
remainder of the first stdin chunk for `cancel` (a request and a cancel
line arriving together no longer lose the cancellation); the coordinator
applies a 15-minute default when the suite omits `limits.wallClockMs`
(always enforced), and `experimentWallClockMs` is documented as checked
between trials rather than a hard cap.

Deferred to AP-05/AP-06: grading, metrics comparison, report export,
CLI-transport conditions, natural-agent-choice experiments, host-enforced
isolation for hidden-test integrity, CPU/RAM/VRAM/power sampling.

## Product behavior

Offer repeatable trials from any consumer checkout after explicit setup, with the
same implementation usable by CLI, CI and the editor. Proposed syntax, not an
installed command:

```sh
REPOISE_BENCH_RUNNER=/absolute/path/to/agent-adapter \
repoise bench --suite /absolute/path/to/suite.json \
  --conditions baseline,mcp-lexical,mcp-hybrid --repeat 5
```

The executable override only selects an adapter; requests/results use AP-03 JSON.
Support an inspectable dry run listing trials, prerequisites, expected scope,
budgets and estimated maximum work before launching agents. Actual time/cost
estimates must identify assumptions, not promise a completion time.

The coordinator owns trial scheduling/isolation/artifacts, never the agent loop.
Choose native Rust orchestration or an optional Node companion after an adapter
prototype. Preserve the `repoise bench` facade, explicit optional installation,
and no-Node native lexical operation. Keep benchmark execution separate from
ordinary `repoise index`, startup, package installation and project test hooks.

## Conditions and experiment manifest

| Condition            | Tools and purpose                                                                                        |
| -------------------- | -------------------------------------------------------------------------------------------------------- |
| Baseline             | Normal agent filesystem search/reads with normal project guidance, without Repoise tools/guidance/access |
| MCP lexical          | Same baseline tools plus Repoise with lexical mode enforced                                              |
| MCP hybrid           | Same baseline tools plus Repoise with hybrid mode enforced and verified coverage/profile                 |
| CLI lexical/hybrid   | Later optional conditions isolating transport/invocation overhead from retrieval strategy                |
| Natural agent choice | Separate later experiment where the agent chooses when/how to use Repoise                                |

MCP enabled/disabled alone does not identify embedding impact. Existing MCP
defaults lexical. Controlled conditions require an explicit validated mechanism
(adapter policy or an instrumented benchmark MCP boundary) selecting allowed
search mode — shipped as the server-side `--search-mode` pin (added during
AP-04 review), exercised by a CLI integration test that parses the exact
argv the coordinator generates. Log requested/effective mode, degradation, vector coverage, profile
fingerprint, embedding request timings and whether Repoise was used at all.
Do not silently count degraded lexical calls as successful hybrid execution.

Enforcement wrappers must preserve other search/read semantics, record their
version/overhead and not leak benchmark gold answers. Verify baseline has no
Repoise MCP, CLI use, managed guidance/skill or pre-injected search results.
Remove only Repoise-specific guidance; record intentional prompt/tool-definition
differences and their context cost. A condition the adapter cannot enforce is
unsupported, not an approximate baseline. Keep filesystem fallback in Repoise
conditions and count its use. No tool call is required when evidence already
suffices; zero Repoise use is a valid observation for natural-use experiments.

Freeze the experiment before execution: suite/criteria hashes, repository
commit/patch, repetition/seed/order, condition definitions, guidance hashes,
Repoise version/features/config/schema, actual parser coverage, index generation,
adapter/harness version, model/artifact/quantization/sampling/context/reasoning,
embedding profile/input template/dimensions, cache policy, hardware/driver and
budgets. Record unknown settings explicitly. A model alias alone is inadequate
when the served weights may change. Reject unrecorded mid-run configuration drift.

## Workspace and process isolation

Use an explicit snapshot and fresh disposable worktree/copy per trial. Default
to a clean committed snapshot; reject dirty inputs unless the operator explicitly
selects a recorded patch snapshot. Do not reset/clean the user's active checkout.
Respect symlinks, submodules, LFS and large ignored dependencies with an explicit
materialization policy. Record failures to reproduce the snapshot.

MCP starts with the exact trial worktree/snapshot root. Detached HEAD is valid;
an MCP server targeting a different checkout is not. Trial output, gold answers,
evaluators and previous transcripts must be outside the searchable workspace;
also block the agent's filesystem access to private grading material. A gitignore
entry alone is not isolation when the agent has terminal tools.

Immutable content/embedding caches may be reused when compatible and declared.
Mutable indexes and worktree identity remain isolated. New worktrees may need
local index publication even when most content is cached. Inspect current cache
implementation; do not assume the proposed cross-tree reuse is already shipped.

Prepare required dependencies and indexes outside the timed agent interval and
record setup time separately. Preflight root/hash/freshness, vector coverage and
endpoint availability. For initial read/explain trials freeze the corpus and
avoid a live watcher mutating it. Coding trials need the same explicit freshness
policy across repetitions (coordinator-managed watch or recorded refresh steps).
Keep watch/provider contention visible, including setup work needed by each
condition. Never stop an unrelated user-managed watcher/model server.

Separate checkouts are not security sandboxes. Run agent and evaluator with
appropriate filesystem/process isolation so hidden checks/other trials cannot
be read. Define the supported isolation mechanism before grading coding tasks;
if a host cannot enforce it, label the limitation and do not claim hidden-test
integrity. Tests/commands from a suite run only through explicit execution, never
while discovering a repository/configuration. No publishing, pushing, deployment
or global configuration changes are part of a trial.

## Scheduling, timing and budgets

Run sequentially on shared local inference hardware initially. Within each
task/repetition block randomize condition order with a recorded seed. Do not run
all baseline trials first then all hybrid trials. Keep fresh conversation/state
per trial; repeated prompt/KV caches may still be warm. Separate declared
cold-start and ready-to-use experiments, including model load and query embedding
cache state. Do not secretly flush or reconfigure the user's serving stack.

Record process startup/queue/agent/evaluator/setup time separately and define
the primary end-to-end boundary. Embedding latency and model swaps belong in the
user-visible interval when they delay the agent. Break them out diagnostically;
do not subtract them to make hybrid appear faster. Optional CPU/RAM/VRAM/power
sampling must identify its own overhead and unavailable sensors.

Apply per-trial time/iteration/call/token/cost limits where measurable, plus an
experiment-level cap. Unsupported token/cost controls must be declared, with wall
time/process controls as a fallback, not a claim of exact enforcement. Warm-up
runs are declared and excluded before execution. Do not selectively retry failures;
record every attempt and use a predeclared retry policy. Distinguish infrastructure
failures, invalid trials, incorrect answers, timeouts, budget stops and cancellations.

## Artifacts and interruption

Write an immutable experiment manifest, append-only per-trial events/results,
stdout/stderr logs, final answers, patches and evaluator evidence under a configured
local output directory outside the indexed scope. Publish summaries only on
explicit export; never auto-upload private code/transcripts. Redact credentials
and auth-bearing endpoints. Provide retention/cleanup guidance without deleting
user-owned paths. Store content hashes and completeness state.

On interruption, preserve completed records and mark in-flight trials incomplete;
terminate owned agent/MCP/watch subprocess trees with bounded grace. Resuming is
optional later: preserve the original manifest, prevent duplicate trial IDs and
never silently merge changed settings into the same experiment. Benchmark runs
must always execute, never replay Turbo task-cache outputs.

Settle exit behavior: usage/preflight/infrastructure failures are nonzero;
interruption is distinguishable; a completed experiment with unsuccessful tasks
still produces a report and exposes its quality-gate result. AP-05 defines which
failures enter which denominators. No success-only summaries.

## Acceptance and open decisions

Use synthetic fake agents/MCP/provider fixtures to verify scheduling, seed replay,
root binding, clean workspaces, baseline leakage refusal, hybrid degradation,
partial events, slow/failed child processes, cancellation, budget caps, artifacts
and configuration drift. Verify baseline and treatment task prompts/normal tools
are identical except the declared integration. Validate POSIX and Windows paths.

Before live runs settle runtime home, isolation mechanism, dirty snapshot policy,
dependency provisioning, mode enforcement, readiness versus timed boundaries,
default limits and missing-coverage policy. Update implemented bench/development
guides and schemas. The initial command must run a fake-agent suite end to end
without any model/provider dependency; real inference remains an explicit pilot.
