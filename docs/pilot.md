# AP-06 pilot: ai-gateway smoke experiment (status)

Status: first smoke experiment **prepared and verified, execution pending**
(2026-10-29, America/Denver) on the AP-04/AP-05 harness against the real
consumer `ai-gateway`. Every component — suite, config, patch snapshot,
harness, adapter, embedding endpoint and preflight gates — is staged and
validated; a single operator-side llama-swap change (per-input batch size)
is the only remaining blocker (see
[Blocker](#blocker-pending-operator-action)). No trial has run and nothing
here claims speed, token or correctness results. The 15–20-task expansion
described in
[06-pilot-and-expansion.md](plans/agent-platform/06-pilot-and-expansion.md)
remains unexecuted.

## What was prepared

- **Consumer**: `ai-gateway` at immutable commit
  `0011d07c8137dacf50793fc454965ad3d6c863f1` (local working-tree HEAD; the
  tree was clean at selection). The Repoise config reaches the trial
  worktrees through a recorded patch snapshot
  (`snapshot.kind: "patch"`, patch sha256
  `4efe85894b16653cd8c23d49c5e9046bf2511bd9824fc663549e1333951a15a8`);
  the consumer repository itself was not committed to or modified.
- **Repoise build**: `repoise 0.1.0-alpha.1`, release build of main
  `c2e7f4a12ad8e484b568d4d83ed2cdf413af2c22` from the source worktree
  (host bridge and Cline adapter resolved from that checkout).
- **Suite**: frozen smoke suite, sha256
  `87fdb866cdbb53bf2446c48d8cb3150c29fe0367f18cf1fe3799d984ed13e4ea`:
  four tasks (two development, two held-out), three conditions
  (`baseline`, `repoise-lexical`, `repoise-hybrid`), one repetition,
  seed 20261029, per-trial 15-minute wall clock and 60-iteration cap,
  3-hour experiment budget. A separate one-trial suite
  (sha256 `47cc77dbfbccfabc21c8b84d21c9d393a3bc59f0aeb6f58ef33c4535c3e000e1`)
  is staged for the SIGINT cancellation check. The suite, patch and config
  live outside the benchmarked repository in the operator's local private
  directory (`~/bench-pilot/`); no suite content is committed to Repoise.
- **Config** (patched into trial worktrees, untracked at the consumer
  root): preset `hybrid`, embedding scope `all` (docs+code — the bench
  hybrid preflight requires complete corpus-wide vector coverage),
  explicit input profile `embeddinggemma-2:1`, model `embeddinggemma-2`,
  768 dimensions, a lenient 10-minute per-batch timeout with 4 retries
  and a 10M-character per-build input budget, endpoint and key via
  `env:` references only, and one corpus exclusion
  (`src/routes/chat.ts`, see limitations). Config sha256
  `626c1d1014abe2cb4ca1e855cdba5ef29cf93cf9c48f6a2c2d2b85c9c499a3e9`.
- **Harness/profile**: Cline CLI 3.0.61 (`cline` npm package, Node
  v22.22.1), adapter `agent-adapter-cline 0.1.0-alpha.1` over the
  AP-03 host bridge (protocol 1), provider `lmstudio` against the
  local llama-swap gateway `http://127.0.0.1:8080/v1`, model
  `qwen3.8-27b-gsq-iq3s` (quantized GGUF, 262144-token context window,
  resident on the shared GPU). One Cline runtime/model profile, held
  fixed across all conditions; condition order randomized per
  task/repetition block with the recorded seed; trials sequential.
  Isolated per-trial Cline state is pre-seeded so provider selection
  never falls back to a default provider mid-trial.
- **Embedding controls**: embedding model `embeddinggemma-2` (768 dims,
  explicit `embeddinggemma-2:1` profile from the evaluated
  local-inference preset), served through the same llama-swap gateway on
  demand; it is configured to co-reside with the coding model.
  `nomic-embed-text-v1.5` was the first candidate but cannot be loaded
  alongside the resident coding model under the current llama-swap
  configuration, so it was replaced; see limitations.

## Smoke objectives and status

| Objective | Status |
| --------- | ------ |
| Isolation (disposable worktrees, no reset of the user checkout) | Not executed; staged |
| Independently correct grading (deterministic checks outside agent visibility) | Not executed; staged |
| Usage accounting (tokens per trial, cumulative fold) | Not executed; staged |
| Actual hybrid calls (vectors served by the trial MCP, no silent lexical degradation) | Not executed; blocked on coverage |
| Degradation handling | Not executed; staged |
| MCP scope (read-only server bound to the trial worktree) | Not executed; staged |
| Cancellation (SIGINT, cooperative `cancel`, exit 4) | Not executed; suite staged |

## Measured vs unmeasured gates

- **Measured** (during preparation, on 2026-10-29):
  - `embeddinggemma-2` through the gateway: 768 dims; a 32-input batch of
    short inputs in ~0.2–2.7 s; co-resident with the 27B coding model.
    Per-input token limit: 512 under the default `--ubatch-size`, raised
    to 2048 by an operator llama-swap change mid-preparation (probed:
    ~490 tokens OK / 513 failed before; 1500 tokens OK after).
  - `nomic-embed-text-v1.5`: batch latency exceeded the per-batch timeout
    while the coding model was resident (and it cannot co-load at all
    under the current llama-swap configuration); replaced, not used.
  - Indexing behavior: incremental resume and the content-addressed
    embedding cache work (vectors from earlier builds are reused as cache
    hits; cache writes are transactional per build, so a killed build
    loses its in-flight vectors). A build stopped by the 1M-character
    default input budget published 908/9673 vectors with zero failures.
  - `bench --dry-run` preflight correctly refuses hybrid conditions
    without verified corpus-wide vector coverage (observed on every
    partial-coverage generation).
- **Failed/flagged**:
  - Complete corpus coverage is impossible at the current 2048-token
    per-input limit: the largest embeddable symbol chunk (`runController`,
    ~5k tokens) exceeds it, so several chunks in
    `src/orchestrator/`, `src/tools/github.ts`, `src/video/mediaStore.ts`
    and one test fixture would fail.
  - One chunk is over the model's entire 8192-token context:
    `chatRoutes` in `src/routes/chat.ts` (57,909 chars, ~14.5k tokens).
    That file is excluded from the corpus (recorded confound).
- **Unmeasured**: all trial-level gates (the 12 smoke trials),
  cancellation behavior, model comparisons, harness comparisons,
  cross-repository effects, cold-cache behavior, and any
  speed/token improvement claims.

## Blocker (pending operator action)

Raise the `embeddinggemma-2` llama-server in the llama-swap model config
to the context size: `--batch-size 8192 --ubatch-size 8192`
(`-c 8192` already matches). With that change every remaining chunk should
fit the per-input limit (largest estimated ~5k tokens; not yet probed at
the 8192 limit). Completion is then a single staged command on the
operator machine:

```sh
bash ~/bench-pilot/run-smoke.sh   # index -> status -> dry-run -> bench
```

followed by the one-trial cancellation suite
(`~/bench-pilot/suite-cancel.json`). Benchmark artifacts are disposable
local files; no repository or device state is modified.

## Limitations

- **Not a completed experiment**: no trial has run; this document records
  preparation, verification evidence and the exact remaining step. It is
  an execution record, not a results report.
- **Corpus exclusion confound**: `src/routes/chat.ts` is excluded from
  the Repoise corpus in all conditions (its largest symbol chunk exceeds
  the embedding model's context). The baseline condition can still read
  the file directly; no smoke task targets that file, but the asymmetry
  is recorded.
- **Private artifacts**: the suite, patch snapshot, config and staged
  runner live in the operator's local private directory and are not
  committed; the sha256 values above pin them for reproduction.
- **Single repetition, no statistics**: even once executed, the smoke run
  (4 tasks × 3 conditions × 1 repeat) verifies mechanics only; it does
  not estimate effects. The expansion run defines the measurement.
- **Shared inference resource**: trials and the operator's coding sessions
  share one GPU/gateway; embedding batches were observed to fail or slow
  under concurrent load, which is why budgets are lenient and runs are
  sequential.
- **Local inference preset not fully exercised**: the evaluated preset's
  model is used, but the original `nomic-embed-text-v1.5` selection from
  the preset could not be loaded alongside the coding model under the
  current llama-swap configuration.
