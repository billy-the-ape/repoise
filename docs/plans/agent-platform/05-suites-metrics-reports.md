# AP-05: task suites, independent grading and comparison reports

Status: proposed. Depends on [AP-04](04-benchmark-runner.md); co-design schema with
[AP-03](03-agent-adapters.md). Preserve existing retrieval evaluation separately.

## Two complementary evaluation layers

The frozen `scripts/eval/` harness measures retrieval properties (including
recall, provenance, policy and timing) on synthetic fixtures. It neither runs an
agent nor proves workflow improvement. Add agent trials to measure complete tasks
without deleting or relabeling those retrieval tests. Search latency and final
agent latency answer different questions.

Built-in generic prompts can smoke-test any repository, but cannot automatically
establish correctness across arbitrary codebases. Credible quality measurements
need repository-specific prompts, expected evidence or behavioral checks. Ship
the suite schema, evaluator interfaces and synthetic examples centrally. Consumer
repos contribute data/checks, not duplicated orchestration code. Suites can also
live outside a private consumer checkout.

## Suite contract

Each suite declares version/hash, corpus commit/materialization policy, task IDs,
categories, task prompt hashes, allowed changes/tools, budgets, evaluator method
and required capabilities. Separate the public task input from private expected
answers/checks. Repeated trials use exactly the same task definition. Keep
development/tuning tasks distinct from held-out tasks; creating a benchmark then
tuning to all of it invalidates a held-out claim.

| Category                     | Required assessment                                                                     |
| ---------------------------- | --------------------------------------------------------------------------------------- |
| Exact identifier lookup      | Correct source/behavior, not merely a plausible file name                               |
| Conceptual discovery         | Relevant implementation/evidence without naming the target symbol in the prompt         |
| Cross-file explanation       | Correct relationships and grounded claims across components                             |
| Debugging                    | Actual cause and evidence; check whether a proposed fix addresses it                    |
| Small implementation         | Independent behavioral checks plus permitted-change constraints                         |
| Documentation reconciliation | Real documented/code mismatch with source evidence, including legitimate no-issue cases |

Include baseline-friendly exact lookups and tasks that need no retrieval. Avoid
selecting only semantic-search-friendly questions or leaking gold identifiers in
instructions. A single generic prompt repeated twenty times measures variability
on that prompt, not usefulness across a repository.

Evaluators run outside agent visibility after the trial. They distinguish pass,
fail, not graded and evaluator error; agent assertions and exit codes are not
correctness. Deterministic tests/expected evidence are preferred. Explanation
rubrics may require blinded human review; an optional model judge records its
own model/version/prompt/cost and should be calibrated against independent review.
Do not silently use the tested agent/model to grade itself.

Alternative correct implementations/evidence must be accepted. Exact patch or
string equality is usually insufficient. Keep grading stable across conditions,
validate evaluator behavior using known-good and known-bad results, and separate
agent runtime from grading runtime. Hidden tests require enforced isolation;
merely placing them in another directory does not hide them from a shell agent.

## Per-trial measurement schema

| Group           | Fields and interpretation                                                                                                |
| --------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Outcome         | Agent finish reason; correctness/rubric; invalidity/evaluator errors; timeout/budget/approval stop                       |
| Time            | External elapsed; agent-reported time; startup/queue/setup/grading; request/tool/retrieval/embedding spans where exposed |
| Usage           | Input/output/reasoning/cache-read/cache-write tokens; request/iteration counts; raw usage semantics and provenance       |
| Exploration     | Tool calls by type, filesystem reads/searches, result bytes/estimated tokens, repeated reads, compactions/subagents      |
| Retrieval       | Requested/effective mode, coverage, result count, empty searches, stale/hash/scope errors, fallback reads, degradation   |
| Preparation     | Index/embedding time, API inputs/requests/usage, storage size, compatible-cache hits/misses                              |
| Resources       | Optional CPU/RAM/VRAM/power; model load/swap events, provider contention, sampling interval                              |
| Reproducibility | Immutable identities/config hashes defined in AP-04, telemetry availability and instrumentation version                  |

Keep every repetition individually. Use nullable fields with reasons for missing
telemetry. Preserve provider-reported raw totals and label estimates, including
Repoise's estimated retrieval output tokens. Do not compare those directly to
actual model input token totals or assume every retrieved token entered context.

Input tokens may include cached tokens; cached reads/writes have different cost
and latency implications. Never add cache reads to inclusive totals a second
time. Sum the full task's requests, including compaction and subagents when
enabled. Record cumulative versus delta usage. Unknown cost is not zero; local
inference has no meaningful provider-dollar price unless the operator defines a
separate energy/amortization model. Tokenization differs across models, so raw
cross-model token counts alone do not measure efficiency.

## Analysis and reporting

Write machine-readable raw trial records and a readable report. Preserve
measured values; compute reductions/ratios only at reporting time. Include
counts of planned, attempted, complete, invalid, cancelled, timed-out and graded
trials so no denominator is hidden. Missing cells stay visible.

Compare paired trials for the same repository/task/model/harness block. Show
correctness rates and their uncertainty beside elapsed/token changes. Report
median paired differences/ratios with clearly defined aggregation and baseline
zero handling; include means as secondary descriptive values. Do not present
ratio-of-totals as median per-task savings. Predeclare whether repetitions are
summarized within task first and how tasks/repositories are weighted.

Repeated trials of one task are correlated. Estimate uncertainty at the task
level (for example paired task-cluster bootstrap retaining repetitions); for
cross-repository claims account for repository clustering and small samples.
Record interval method, seed and sample size. Tail latency needs enough samples;
label p95 from tiny samples exploratory. Timeouts are censored observations,
not completed fast runs; report timeout rate and capped/lower-bound timing
explicitly rather than dropping them. Show successful-only timing only as a
secondary metric with its selection bias labeled.

Predeclare a practical improvement threshold and acceptable correctness margin.
"No observed decrease" is descriptive, not proof of non-inferiority in a small
sample. The existing v1 section 11 gates remain the product targets, including
20% median input-token reduction without correctness decrease, code-vector
recall improvement and separately scoped latency measurements. Do not declare
those passed by conflating a retrieval microbenchmark, grouped usage mean, or
uncontrolled live pilot with the required evidence. New acceptance criteria must
be explicit decisions, not a quiet lowering of an existing gate.

## Repository, model and harness comparisons

Report baseline-relative changes within each repository/task category first,
then aggregates. Record language mix, indexed files/chunks/bytes, eligible code,
documentation coverage, parser mode, generated/vendor exclusions and repository
age/layout characteristics when available. File count alone is not complexity.
Several repositories per category are needed before generalizing TS versus Python
or big versus small. Current TS/JS structural parsing versus Python line windows
is an implementation confounder, not proof of inherent language differences.

Keep model profiles separate from harness profiles. Same model across harnesses
can still mean different system prompts, tools, budgets and compaction. Name the
actual runtime surface/revision. Introduce one dimension at a time initially;
additional pairwise comparisons are exploratory unless predeclared, and should
not cherry-pick a winning model/task subset.

## Existing replay schema compatibility

`scripts/eval/replay-evidence.schema.json` v1 fixes one model/revision/task set,
restricts conditions to an enum and aggregates repetitions through `sampleSize`
with one task-level correctness boolean. It cannot faithfully represent the new
multi-axis per-repetition experiment. Do not squeeze new fields/condition names
into it or invent repeated success counts from `taskCorrect`.

Add a separately versioned agent-trial schema and a deliberate compatibility path.
Retain v1 import validation. If offering a v1 export, export only truly representable
complete slices, preserve usage provenance, label lossy aggregation and refuse
unsupported mappings. A legacy export cannot establish per-trial uncertainty or
the median gate. Test malformed/unknown fields, duplicate trial IDs, missing
cells, unavailable metrics and old evidence imports without model access.

## Acceptance and decisions

Synthetic reports must expose a faster-but-wrong condition, missing telemetry,
degraded hybrid, timeout, evaluator failure, zero baseline and correlated repeats.
Independent rubric fixtures must reject incorrect results and accept legitimate
alternatives. Report generation cannot execute untrusted transcript contents.

Settle schema versioning, grading plugins/isolation, interval implementation,
weighting, gate policy, model-judge allowance and redacted export/retention before
AP-06. No private corpus or new live result is committed by this PR. Update the
evaluation runbook to distinguish retrieval tests, legacy evidence and live trials.
