# Benchmark runner (`repoise bench`, AP-04/AP-05)

`repoise bench` runs suite-driven agent benchmark experiments: repeated
trials over one frozen repository snapshot, in isolated disposable worktrees,
against declared retrieval conditions. The coordinator owns scheduling,
isolation, budgets, interruption and artifacts. It never implements an agent
loop: every trial's agent process is driven through the versioned agent
adapter protocol (AP-03, see [agent adapters](agent-adapters.md)) via the
checked-in Node host bridge
(`packages/agent-adapter-protocol/bin/adapter-host-run.mjs`). Native
orchestration (suite parsing, preflight, worktrees, MCP wiring, artifacts,
reports) runs without Node; Node is the optional companion that is required
only for actually launching adapter processes.

Independent post-trial grading, paired metric comparison and report
publishing are part of the runner (AP-05): each completed trial is graded
by its task's declared evaluator (`grading.json`), and the published report
(`report.json` + rendered `report.md`) carries status/grading counts, per-task
and per-condition grading summaries, the predeclared paired comparisons and
the predeclared quality gate. `bench report` re-grades recorded trials and
rebuilds the report offline. Grading is data-only: transcripts and repository
content are never executed, and a trial's own assertions or exit code are
never treated as correctness.

## Command surface

```sh
repoise bench [SUITE]
repoise bench --suite <PATH> [--json] [--dry-run]
    [--runner <PATH>] [--repeat <N>] [--seed <N>] [--output <DIR>]
```

- `SUITE` — the suite manifest (JSON, schema v1; positional or `--suite`).
- `--dry-run` — run preflight and print the frozen plan only; no worktrees,
  no agent processes, exit 0.
- `--json` — machine output: the final document (dry-run document or
  experiment report) is the only stdout content; progress lines go to stderr.
- `--runner <PATH>` — override the suite's adapter executable; the
  `REPOISE_BENCH_RUNNER` environment variable is the second fallback.
- `--repeat <N>` / `--seed <N>` — override the suite's repetition count and
  seed (the frozen plan records which overrides applied).
- `--output <DIR>` — override the artifact root (default: `repoise-bench`).

```sh
repoise bench report --experiment <EXPERIMENT_ID> [--output <DIR>] [--json]
```

- `bench report` (AP-05) rebuilds a report from the frozen `manifest.json` and
  the recorded trial artifacts under `<output>/<experimentId>/`: every trial
  with a declared evaluator is re-graded (a valid operator-written manual
  grading record is kept unchanged), and `report.json`/`report.md` are
  rewritten. It reads and writes only artifacts — no worktrees, no agent
  processes, no network. `--experiment` is required; the command exits `0`
  on success and `1` when the experiment id is missing or the experiment
  directory / artifacts are incomplete.

Environment: `REPOISE_BENCH_HOST_BRIDGE` pins the host bridge path (default:
the checked-in `bin/adapter-host-run.mjs` resolved from the source checkout
the running executable was built from — the current directory is never
consulted, and a bridge inside the benchmarked repository is rejected, since
repository content is untrusted data and must not be executed automatically;
point the override at an explicit bridge file in that case). A
source-checkout build needs no environment variables; installed npm/native
packages do not ship the host bridge and need `REPOISE_BENCH_HOST_BRIDGE`
(pointing at the bridge file in a source checkout), and
`REPOISE_BENCH_RUNNER` optionally overrides the suite's adapter executable.
The experiment cache is always `<output>/<experimentId>/cache` (relocate it
with `--output`); the coordinator's own `REPOISE_CACHE_DIR` only identifies
the root repository's cache for the snapshot dirtiness check.

Exit codes (stable; also in `repoise capabilities`): `0` completed, `1`
operational failure, `2` usage, `3` preflight/suite failure, `4` interrupted
(SIGINT/SIGTERM). The global table reuses 3 and 4 for bench-specific
outcomes (`benchPreflight`, `benchInterrupted`).

## Suite manifest

The suite is the operator-declared experiment; it is parsed strictly
(`deny_unknown_fields` at every level) so a typo fails preflight instead of
silently changing the experiment. Checked-in schema:
[schemas/bench-suite.v1.schema.json](../schemas/bench-suite.v1.schema.json).
Required fields: `id`, `root`, `tasks`, `conditions`, `adapter`, `profile`.

```json
{
  "schemaVersion": 1,
  "id": "smoke",
  "root": "/abs/path/to/repo",
  "snapshot": { "kind": "commit", "revision": "HEAD" },
  "tasks": [{ "id": "t1", "prompt": "One-line question." }],
  "conditions": [
    { "id": "baseline", "retrievalMode": "baseline" },
    { "id": "repoise-lexical", "retrievalMode": "repoise", "searchMode": "lexical" }
  ],
  "scope": "read",
  "repeat": 1,
  "seed": 7,
  "adapter": { "executable": "/abs/path/to/adapter", "args": [], "env": {} },
  "profile": { "harness": { "name": "clinetool" }, "provider": { "id": "none" } },
  "limits": { "wallClockMs": 120000, "maxIterations": 20 },
  "budgets": { "experimentWallClockMs": 3600000 },
  "output": { "dir": "repoise-bench" }
}
```

- `snapshot` — `commit` (default; clean tree required) or `patch`
  (`patchFile` with a git-apply-able patch over `revision`, recorded by hash).
- `tasks` — exactly one of `prompt` / `promptFile` per task; neither may
  contain grading answers. Optional AP-05 fields: `category` (the
  assessment taxonomy), `heldOut` (development vs held-out split),
  `allowedPaths` (write-scope change constraints, workspace-root relative),
  `expectedArtifacts` (names of files the agent should leave in the trial's
  `adapter/` directory) and `evaluator` (see below).
- ids — experiment, task and condition ids must contain only
  `[A-Za-z0-9_-]`: the experiment id names the artifact directory (so `.`/`..`
  could escape the output root) and trial ids and artifact directories are
  derived from the other ids. Generated trial ids (`t-{task}-r{rep}-{condition}`)
  are additionally checked for cross-id collisions — the `-` joiner can make task `a` +
  condition `r0-x` generate the same trial id as task `a-r0` + condition
  `x` — so validation rejects ambiguous suites before any worktree is
  created (other characters could also make ids ambiguous or let an
  artifact directory escape the output root).
- `conditions` — `baseline` (no Repoise tools or managed guidance; the
  managed overlay is removed from the disposable worktree only) or `repoise`
  (`searchMode` required: `lexical` or `hybrid`).
- `adapter` — the AP-03 adapter executable (absolute path or bare name on
  `PATH`). `env` values may be `env:VAR_NAME` references, resolved at
  preflight; only names ever reach artifacts.
- `profile` — mirrors the AP-03 request `profile` group (harness, provider
  with opt-in `credentialRef`, model, reasoning, compaction, free-form
  sampling).
- `limits` — per-trial caps forwarded to the adapter (`wallClockMs` is always
  enforced by the coordinator regardless of adapter support; the coordinator
  applies a 15-minute default — 900000 ms — when the suite omits it).
- `budgets.experimentWallClockMs` — experiment wall-time cap checked between
  trials (a trial already running still completes, bounded by its own
  `wallClockMs`); trials not started after it are recorded as `not_started`
  and the report is still published.
- `tasks[].evaluator` — how the task is graded after the trial, outside agent
  visibility. `method` is `deterministic` (default), `humanRubric` or
  `modelJudge`; `answerField` names the field of the final event's `data`
  object holding the answer text (default `answer`); `rubric` is required
  for `humanRubric`; `checks` (required for `deterministic`, forbidden for
  the declared-only methods) are literal-substring assertions: `type`
  (`containsAny` / `containsAll` / `notContains`, case-sensitive), `values`
  (non-empty literal substrings; `containsAny` accepts alternative correct
  evidence — exact string equality is deliberately not the default) and an
  optional `scope`/`artifact` pair that reads `adapter/<name>` from the
  trial's artifact directory (names only, must be listed in the task's
  `expectedArtifacts`). The haystack is the recorded answer or the produced
  artifact content; when no haystack exists (no answer, or the artifact was
  not produced), every check fails — including `notContains` — so a trial
  that produced no answer cannot pass. A task without an evaluator is never
  graded (`not_graded`).
- `gates` — predeclared quality gate (AP-05); thresholds are decided before
  the experiment, not fitted to its results. `baseline` (condition id),
  `minCorrectness` (default 1.0), `correctnessMargin` (default 0.0) and
  `improvements` (declared paired comparisons: `candidate`, `metric`
  (`agentMs` or `inputTokens`) and `threshold` in (0, 1]). Omitted: the report
  is produced without a gate (`qualityGate.status: not_configured`) and
  without paired analysis.
- `report` — report analysis configuration: `bootstrapIterations` (default
  1000) and `bootstrapSeed` (default 0) for the task-cluster bootstrap paired
  interval estimate. Omitted: the defaults apply.

The suite file, every `promptFile` and the `patchFile` of a recorded patch
snapshot must live **outside** the benchmarked repository: a trial checkout
would otherwise expose gold answers and
evaluators to the agent.

## Preflight (exit 3)

Preflight freezes the experiment and refuses to approximate:

- the suite, prompt and patch files are outside the repository root;
- the snapshot is reproducible (clean committed tree, or an explicitly
  recorded patch); the resolved Repoise cache directory (e.g. the default
  `.repoise`) and the output root (artifacts and trial worktrees) are
  Repoise's own state and never count as dirt — running from the repository
  root with the default in-repo `output.dir` works across invocations — and
  an output directory equal to the repository root is rejected;
- `node`, the host bridge (resolved from the executable's source checkout,
  never from the current directory or the benchmarked repository) and the
  adapter executable resolve;
- `env:`/`credentialRef` references resolve (names only are recorded); the
  provider's `credentialRef` variable is forwarded to the adapter's process
  environment (it need not be re-declared under `adapter.env`), and the
  embedding endpoint/API-key variables declared in the root config are
  forwarded to each trial's MCP server env, so the trial server serves the
  same embedding session the coordinator's index child built;
- `hybrid` conditions require verified vector coverage on the root index —
  a degraded lexical fallback is never counted as hybrid;
- root config/cache resolution, scope status and guidance state are captured
  for the manifest (an active watch/index writer is recorded as contention,
  never stopped).

## Trial lifecycle

Trials run sequentially, in the frozen plan's order (condition order
randomized per task/repetition block with the recorded seed). Each trial:

1. creates a disposable linked git worktree checked out detached at the
   frozen revision (the user's checkout is never reset or cleaned);
2. for `repoise` conditions, builds a trial-local index in the experiment
   cache (setup time is recorded separately from the agent interval) and
   launches a read-only `repoise mcp` server bound to the trial worktree
   (its env carries the trial cache plus the root config's embedding
   endpoint/API-key variables, so the server can serve hybrid search),
   pinned to the condition's declared `searchMode` with `--search-mode`
   (the server refuses requests naming a different mode, and setup fails
   with `infra_error` if the trial index has no published vectors for a
   hybrid/vectors-only condition, so a hybrid run can never silently
   degrade to lexical; the effective mode and any degradation are recorded
   in `trial.json` `mode`);
3. writes a redacted copy of the AP-03 request
   (`trials/<id>/request.json`; `condition.mcp.env` values are replaced with
   a placeholder so secrets stay out of shareable artifacts) and sends the
   unredacted request only to the host bridge over stdin
   (`node adapter-host-run.mjs <adapter> [args]`);
4. on SIGINT/SIGTERM sends the cooperative `cancel` line to the bridge,
   escalates to signals after the grace period, force-kills the bridge and
   its recorded process tree at the outer wall-clock deadline, and marks
   remaining trials `not_started`;
5. writes `trial.json` (status, timings `queueMs`/`setupMs`/`agentMs`/
   `totalMs`, usage, hello/adapter/harness provenance, mode, errors,
   completeness), removes the worktree (unless `output.keepWorkspaces`),
   and continues — one failed trial never aborts the experiment.

Trial statuses: `completed`, `error` (adapter/protocol failure or wall-clock
timeout), `cancelled` (cooperative cancellation), `infra_error` (worktree,
index, host-bridge or artifact failure — the workspace is always cleaned up
and the experiment continues), `not_started` (budget, interruption or an
unrecoverable coordinator failure; the report is always published). When
the coordinator's outer watchdog force-kills the bridge at the per-trial
wall-clock deadline, the trial is recorded as `infra_error` (or `cancelled`
on a coincident interrupt) with reason `host_watchdog`; the report still
counts it as a censored timeout (it keeps its recorded agentMs at or above
the cap) rather than as an infra failure.

## Grading (AP-05)

After each trial's `trial.json` is written, the coordinator grades the trial
for its task (outside agent visibility, data-only — transcript text and
repository content are matched as data and never executed) and writes
`trials/<id>/grading.json` (schema v1, checked in at
[schemas/bench-grading.v1.schema.json](../schemas/bench-grading.v1.schema.json)):

| Status | Meaning |
| ------ | ------- |
| `pass` / `fail` | deterministic checks over the recorded answer (and declared artifact files) — a trial is `fail` when the answer field is missing entirely |
| `not_graded` | no evaluator declared; a declared `humanRubric`/`modelJudge` result pending a manual record; a censored trial status (timeout, adapter failure, cancellation) — censored trials are never graded as wrong answers |
| `evaluator_error` | the grading engine itself failed (missing/corrupt evidence, an invalid manual record) |

Method handling:

- `deterministic` — checks run in-process; all checks must pass for a `pass`.
  Declared artifact files are read only when they are regular files
  (symlinks, FIFOs and devices are treated as not produced) and the read is
  size-capped; the adapter event log is read with the same guards.
- `humanRubric` — declared-only in this build: the operator writes the
  manual grading record (schema version, status `pass`/`fail`, the declared
  method, a non-empty reason) to `trials/<id>/grading.json` and the engine
  keeps it verbatim (never rewritten or relabeled as an engine record, so
  repeated `bench report` rebuilds never lose it). An operator record that is
  invalid (mismatched method or missing fields) or not parseable JSON is
  never clobbered in place: the engine moves the operator's file to
  `trials/<id>/grading.json.rejected` (bytes preserved) and records an
  `evaluator_error` in `grading.json`; that recorded error is re-emitted
  verbatim on later rebuilds (its reason never nests or repeats) until a
  valid manual record replaces it. Engine-written records carry
  `source: "engine"` and are replaced on the next grading pass.
- `modelJudge` — declared-only: this build runs no model judge (grading must
  stay independent of the tested model); the result must also be written as
  a manual grading record.

## Report (AP-05)

`report.json` (schema v1) is written at experiment end and re-rendered to
`report.md` from the same object so the two never diverge. Contents:

- `counts` — planned / attempted / completed / timedOut / cancelled /
  invalid / notStarted plus the grading outcomes (`graded`, `pass`, `fail`,
  `notGraded`, `evaluatorError`); every planned slot lands in exactly one
  status bucket, nothing is dropped.
- `grading` — overall, per-condition and per-task grading summaries with the
  correctness rate over graded trials (`heldOutTasks` / `developmentTasks`
  split included); per-condition `attempted` excludes not-started slots,
  matching the global counts and the `timeoutRate` denominator.
- `analysis` — present only when `gates` is declared: for each declared
  improvement, the median per-task paired difference and reduction (candidate
  minus baseline, over tasks with measured data on both sides; metric values
  come only from completed and timed-out trials — timeouts keep their
  recorded, capped time — while non-timeout failures, cancellations, infra
  errors and never-started slots are excluded and counted in the per-task
  exclusion reasons; zero or missing baselines yield an undefined reduction
  with an explicit note), a
  task-cluster bootstrap percentile interval (recorded iterations and seed;
  all repetitions of a resampled task stay together) and descriptive stats
  (timeout rate, capped/lower-bound timing, labelled success-only medians).
- `qualityGate` — the predeclared gate evaluated: per-condition correctness
  vs `minCorrectness`, per-improvement median paired reduction vs
  `threshold`, and the candidate-vs-baseline `correctnessMargin` floor.
  Status is `pass` only when every declared check was evaluated and passed;
  any declared check that could not be evaluated makes the overall status
  `insufficient_data` (with the reason per check), any failed evaluated
  check makes it `fail`, and `not_configured` when the suite declares no
  gate — a speed gain without correctness data is never a pass. A pass/fail
  verdict is descriptive evidence against the predeclared thresholds, not a
  product gate (the v1 section 11 gates remain the product targets).

`bench report` re-runs grading and report assembly from the frozen manifest
and trial artifacts (see the command surface above). The manifest's task,
condition and gate declarations are re-validated on the rebuild path (the
manifest is untrusted input: a copied or hand-edited manifest fails with a
configuration error instead of panicking the graders or reading outside the
trial directory). Not-started slots are not re-graded (the run never graded
them either), so the rebuilt report sees the same counts as the run; the
run's status and `failureReason` are carried over from the previous report
when one exists.

## Artifacts

Under `<output.dir>/<experiment.id>/`:

| Path | Content |
| ---- | ------- |
| `manifest.json` | Immutable frozen experiment: suite hash, overrides, snapshot, conditions, plan, adapter/host, budgets, guidance, unknowns |
| `report.json` | Final report (schema v1): status, duration, per-trial records, `counts`, `grading`, `analysis` (when gates declared) and `qualityGate` |
| `report.md` | Human-readable rendering of `report.json` (derived from the same object) |
| `trials/<id>/trial.json` | One trial's record (schema v1) |
| `trials/<id>/grading.json` | One trial's grading record (schema v1); engine records carry `source: "engine"`, operator manual records do not |
| `trials/<id>/grading.json.rejected` | Operator manual record the engine rejected (invalid or unparseable); the engine never overwrites operator bytes in place |
| `trials/<id>/request.json` | The AP-03 request with `condition.mcp.env` values redacted (names kept; the live request with real values goes only to the host bridge over stdin; a local artifact) |
| `trials/<id>/adapter/` | `events.ndjson`, `result.json`, `stderr.log` (adapter diagnostics) |
| `trials/<id>/host.log` | Coordinator-side bridge stderr |
| `cache/` | Trial index DBs and the shared family store (declared, content-addressed) |

No transcript or repository content is uploaded; everything stays under the
configured local output directory.

## Declared limitations

- Worktrees are isolation, not security sandboxes: hidden-test integrity for
  coding tasks is a declared limitation until a host-enforced mechanism is
  defined (AP-05/AP-06).
- `modelJudge` evaluators are declared-only (no model-judge runtime; no
  self-grading with the tested model); `humanRubric` results and any model-judge
  results must be written as manual grading records.
- The replay-evidence v1 import path (`scripts/eval/`) is unchanged and cannot
  represent per-repetition multi-axis experiments; no v1 export is offered.
- No `--events` NDJSON stream for `bench` (progress lines only; AP-02 stream
  remains on `index`/`watch`).
- Token/cost limits are forwarded to the adapter and enforced by the host
  only where measurable; per-trial wall time is always enforced by the
  coordinator (15-minute default when the suite omits `wallClockMs`), and the
  experiment-level cap is checked between trials.

## Quick smoke (no live inference)

With the checked-in fake adapters you can exercise the whole loop offline:

```sh
repoise bench --dry-run /abs/path/to/suite.json
repoise bench /abs/path/to/suite.json --output /tmp/bench-out
# send SIGINT mid-run to observe cooperative cancellation (exit 4)
```

The host bridge itself is tested by
`packages/agent-adapter-protocol/test/bridge.test.mjs` (successful run,
stderr mirroring, cooperative cancel, cancel in the same chunk as the
request, missing request line) alongside the AP-03 conformance suite.
