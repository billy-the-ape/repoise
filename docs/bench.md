# Benchmark runner (`repoise bench`, AP-04)

`repoise bench` runs suite-driven agent benchmark experiments: repeated
trials over one frozen repository snapshot, in isolated disposable worktrees,
against declared retrieval conditions. The coordinator owns scheduling,
isolation, budgets, interruption and artifacts. It never implements an agent
loop: every trial's agent process is driven through the versioned agent
adapter protocol (AP-03, see [agent adapters](agent-adapters.md)) via the
checked-in Node host bridge
(`packages/agent-adapter-protocol/bin/adapter-host-run.mjs`). Native
orchration (suite parsing, preflight, worktrees, MCP wiring, artifacts,
reports) runs without Node; Node is the optional companion that is required
only for actually launching adapter processes.

Quality grading, metrics comparison and report publishing are AP-05. This
runner records completion and resources only; the report carries a
`qualityGate: not_evaluated` note.

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

Environment: `REPOISE_BENCH_HOST_BRIDGE` pins the host bridge path (default:
the checked-in `bin/adapter-host-run.mjs` next to the running checkout);
`REPOISE_CACHE_DIR` relocates the experiment cache like every other command.

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
  contain grading answers (that is AP-05 territory).
- ids — task and condition ids must contain only `[A-Za-z0-9_-]`: trial ids
  and artifact directories are derived from them, so other characters could
  make two distinct ids collide after sanitizing (validation rejects them
  before any worktree is created).
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
  enforced by the coordinator regardless of adapter support).
- `budgets.experimentWallClockMs` — hard cap on total experiment wall time;
  trials not started after it are recorded as `not_started` and the report is
  still published.

The suite file and every `promptFile` must live **outside** the benchmarked
repository: a trial checkout would otherwise expose gold answers and
evaluators to the agent.

## Preflight (exit 3)

Preflight freezes the experiment and refuses to approximate:

- the suite/prompt files are outside the repository root;
- the snapshot is reproducible (clean committed tree, or an explicitly
  recorded patch); the resolved Repoise cache directory (e.g. the default
  `.repoise`) is Repoise's own state and never counts as dirt;
- `node`, the host bridge and the adapter executable resolve;
- `env:`/`credentialRef` references resolve (names only are recorded);
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
   launches a read-only `repoise mcp` server bound to the trial worktree,
   pinned to the condition's declared `searchMode` with `--search-mode`
   (the server refuses requests naming a different mode, so a hybrid run can
   never silently degrade to lexical; the effective mode and any degradation
   are recorded in `trial.json` `mode`);
3. writes the full AP-03 request (`trials/<id>/request.json`) and spawns the
   host bridge (`node adapter-host-run.mjs <adapter> [args]`);
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
unrecoverable coordinator failure; the report is always published).

## Artifacts

Under `<output.dir>/<experiment.id>/`:

| Path | Content |
| ---- | ------- |
| `manifest.json` | Immutable frozen experiment: suite hash, overrides, snapshot, conditions, plan, adapter/host, budgets, guidance, unknowns |
| `report.json` | Final report: status, duration, per-trial records, `byStatus`, `qualityGate: not_evaluated` (AP-05) |
| `trials/<id>/trial.json` | One trial's record (schema v1) |
| `trials/<id>/request.json` | The exact AP-03 request sent |
| `trials/<id>/adapter/` | `events.ndjson`, `result.json`, `stderr.log` (adapter diagnostics) |
| `trials/<id>/host.log` | Coordinator-side bridge stderr |
| `cache/` | Trial index DBs and the shared family store (declared, content-addressed) |

No transcript or repository content is uploaded; everything stays under the
configured local output directory.

## Declared limitations

- Worktrees are isolation, not security sandboxes: hidden-test integrity for
  coding tasks is a declared limitation until a host-enforced mechanism is
  defined (AP-05/AP-06).
- No grading, metrics comparison or report export yet (AP-05).
- No `--events` NDJSON stream for `bench` (progress lines only; AP-02 stream
  remains on `index`/`watch`).
- Token/cost limits are forwarded to the adapter and enforced by the host
  only where measurable; wall time is always enforced by the coordinator.

## Quick smoke (no live inference)

With the checked-in fake adapters you can exercise the whole loop offline:

```sh
repoise bench --dry-run /abs/path/to/suite.json
repoise bench /abs/path/to/suite.json --output /tmp/bench-out
# send SIGINT mid-run to observe cooperative cancellation (exit 4)
```

The host bridge itself is tested by
`packages/agent-adapter-protocol/test/bridge.test.mjs` (successful run,
stderr mirroring, cooperative cancel, missing request line) alongside the
AP-03 conformance suite.
