# AP-06 pilot: ai-gateway smoke experiment (status)

Status: first smoke experiment **executed** (2026-10-29/30,
America/Denver) on the AP-04/AP-05 harness against the real consumer
`ai-gateway`. The smoke chain ran 2026-10-30: all 12 trials completed and
passed deterministic grading, and the SIGINT cancellation check passed
(cooperative cancellation, exit 4) — see [Results](#results-2026-10-30).
Three trial attempts were needed; the first two failed (adapter PATH and
prompt transport, then the provider endpoint falling back to the Cline
CLI's default host) and were fixed in the worktree. The run verifies
mechanics only — one repetition, no statistics — and nothing here claims
speed, token or correctness effects. The 15–20-task expansion described in
[06-pilot-and-expansion.md](plans/agent-platform/06-pilot-and-expansion.md)
remains unexecuted.

## What was prepared

- **Consumer**: `ai-gateway` at immutable commit
  `9cfba6aa726043f4059c5cf38c11ed90c48fb247` (local working-tree HEAD;
  working tree fully clean — the consumer added `.gitignore` entries for
  `repoise.config.*` and `.repoise/*` in this commit, rebased after the
  original `0011d07` selection and before the first trial run). The
  Repoise config reaches the trial worktrees through a recorded patch
  snapshot (`snapshot.kind: "patch"`, `revision: "HEAD"`, patch sha256
  `a198850cd9577ca3a472a0d77917a38778ccbfe010d11a4ab5284ea4d17c80b8`);
  the trial worktrees are disposable and the consumer repository is never
  reset or modified.
- **Repoise build**: `repoise 0.1.0-alpha.1`, release build of main
  `5508a702823583673702166a1270050078a72089` from the source worktree
  (host bridge and Cline adapter resolved from that checkout).
- **Suite**: frozen smoke suite, sha256
  `d8d8c96af74bde0be932fd39d0ac0f703bbf7c02e4258c9342512e2cd4583978`:
  four tasks (two development, two held-out), three conditions
  (`baseline`, `repoise-lexical`, `repoise-hybrid`), one repetition,
  seed 20261029, per-trial 15-minute wall clock and 60-iteration cap,
  3-hour experiment budget, and the provider endpoint
  `http://127.0.0.1:8080/v1` (required since the Cline CLI's built-in
  default host is `localhost:1234`). A separate one-trial suite
  (sha256 `50f1209627eafb75bee2662e0ded6e765ff78c07b8bf3c89ffac158489a5e2dc`)
  is staged for the SIGINT cancellation check. Both suites' adapter env
  sets `CLINE_NO_AUTO_UPDATE=1`, but the Cline adapter's child
  environment allowlist does not currently forward that variable, so
  this is an unenforced pin (see limitations). The suite, patch and
  config live outside the benchmarked repository in the operator's
  local private directory (`~/bench-pilot/`); no suite content is
  committed to Repoise.
- **Config** (patched into trial worktrees, gitignored at the consumer
  root): preset `hybrid`, embedding scope `all` (docs+code), explicit
  input profile `embeddinggemma-2:1`, model `embeddinggemma-2`,
  768 dimensions, a lenient 10-minute per-batch timeout with 4 retries
  and a 10M-character per-build input budget, endpoint and key via
  `env:` references only, one corpus exclusion (`src/routes/chat.ts`,
  see limitations), and role mappings that place `*.json`/`*.yml`/
  `*.yaml` files in the docs corpus (`current-doc` role) — required
  because the embedding scope covers docs+code only (the config corpus
  is outside every embedding scope), so without the remap the corpus
  could never reach full vector coverage and the bench hybrid
  preflight would be structurally unreachable (see limitations).
  Config sha256
  `5aa4601c3b9c4de259365310dc80dd395a7c892c5561d2343d309f072d49c28f`.
- **Harness/profile**: Cline CLI 3.0.74 (`cline` npm package, Node
  v22.22.1), adapter `agent-adapter-cline 0.1.0-alpha.1` over the
  AP-03 host bridge (protocol 1), provider `lmstudio` against the
  local llama-swap gateway `http://127.0.0.1:8080/v1`, model
  `qwen3.8-27b-gsq-iq3s` (quantized GGUF, 262144-token context window,
  resident on the shared GPU). The CLI auto-updates in the background:
  the recorded prep-time version was 3.0.61, and the pilot machine's
  global install was 3.0.74 at first trial attempt; the suite-level
  `CLINE_NO_AUTO_UPDATE=1` does not reach the CLI (the adapter's
  child-environment allowlist drops it — see limitations), so the
  operator must confirm `cline --version` before and after the run
  (each trial's adapter events also record the exact CLI version). One
  Cline runtime/model profile, held fixed across all conditions;
  condition order randomized per task/repetition block with the
  recorded seed; trials sequential. Isolated per-trial Cline state is
  seeded from the suite profile — the provider endpoint becomes the
  per-trial provider settings `baseUrl`, so the provider never falls
  back to the CLI's default host mid-trial.
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
| Isolation (disposable worktrees, no reset of the user checkout) | Executed 2026-10-30; all 12 trials ran in disposable worktrees, the operator's checkout was untouched |
| Independently correct grading (deterministic checks outside agent visibility) | Executed; all 12 trials graded `pass` by deterministic checks |
| Usage accounting (tokens per trial, cumulative fold) | Executed; per-trial input/output tokens recorded in the report |
| Actual hybrid calls (vectors served by the trial MCP, no silent lexical degradation) | Executed; 4 hybrid trials ran in the pinned hybrid mode (runtime degradation is unobservable — the adapter declares no tool telemetry, see limitations) |
| Degradation handling | Not triggered by this run (full vector coverage); the preflight refusal of partial coverage was observed during preparation |
| MCP scope (read-only server bound to the trial worktree) | Executed; read-only trial-bound servers for all `repoise` conditions |
| Cancellation (SIGINT, cooperative `cancel`, exit 4) | Executed; cooperative cancellation recorded, exit 4 |

## Results (2026-10-30)

Executed as the `cline-agent` user (engine at `5508a70`, Cline CLI 3.0.74 on
every trial, suite hash `d8d8c96a…` as pinned above). The smoke chain
finished in 421 s (12 trials) and the cancellation check passed:

| Trial | Condition | Setup ms | Agent ms | In tokens | Out tokens | Grading |
| ----- | --------- | -------: | -------: | --------: | ---------: | ------- |
| t1 | repoise-lexical | 68400 | 44179 | 35237 | 883 | pass |
| t1 | baseline | 58 | 21891 | 38879 | 579 | pass |
| t1 | repoise-hybrid | 3464 | 17753 | 16654 | 212 | pass |
| t2 | repoise-lexical | 3442 | 41100 | 31522 | 1195 | pass |
| t2 | baseline | 58 | 30419 | 48274 | 715 | pass |
| t2 | repoise-hybrid | 3430 | 30991 | 31661 | 725 | pass |
| t3 | baseline | 56 | 19138 | 14659 | 396 | pass |
| t3 | repoise-hybrid | 3488 | 15021 | 17434 | 386 | pass |
| t3 | repoise-lexical | 3563 | 21293 | 17440 | 397 | pass |
| t4 | repoise-lexical | 3549 | 42969 | 69444 | 1272 | pass |
| t4 | repoise-hybrid | 3528 | 25318 | 35079 | 523 | pass |
| t4 | baseline | 57 | 15298 | 24219 | 369 | pass |

Condition totals (agent interval / input tokens): baseline 86.7 s / 126k,
repoise-lexical 149.5 s / 154k, repoise-hybrid 89.1 s / 101k. No trial was
censored (no errors, timeouts or cancellations); the lexical condition was
slowest on every task in this single repetition — an observation, not an
estimate (one repetition, randomized condition order).

**Cancellation check** (`run-cancel.sh`, SIGINT mid-trial): trial
`t-c1-r0-baseline` recorded `cancelled` / `host_requested` after 2475 ms of
agent time, the experiment status is `interrupted`, and the bench exited
with code 4 as documented.

## Measured vs unmeasured gates

- **Measured** (during preparation, on 2026-10-29):
  - `embeddinggemma-2` through the gateway: 768 dims; a 32-input batch of
    short inputs in ~0.2–2.7 s; co-resident with the 27B coding model.
    Per-input token limit: 512 under the default `--ubatch-size`, raised
    to 2048 and then to 8192 by two operator llama-swap changes
    (probed: ~490 tokens OK / 513 failed at the 512 limit; 1500 tokens OK
    at 2048; 4500 and 5500 tokens OK at 8192).
  - `nomic-embed-text-v1.5`: batch latency exceeded the per-batch timeout
    while the coding model was resident (and it cannot co-load at all
    under the current llama-swap configuration); replaced, not used.
  - Indexing behavior: incremental resume and the content-addressed
    embedding cache work (vectors from earlier builds are reused as cache
    hits; cache writes are transactional per build, so a killed build
    loses its in-flight vectors). A build stopped by the 1M-character
    default input budget published 908/9673 vectors with zero failures.
  - Corpus remap (generation 7, later 2026-10-29): the config's role
    mappings reclassified all 76 config files as docs (the config corpus
    became empty; docs+code = 9437 chunks fully embedded). The 142
    remapped chunks then failed against a crashing embedding upstream
    (llama-swap `upstream command exited prematurely` — `embeddinggemma-2`
    had been evicted when a concurrent `Gemma-4-E4B` coding task loaded
    alongside the resident 27B model, and it could not reload; even a
    2-token probe failed at the time). A single index run completes
    coverage once the embedding model can load again (only ~142 new
    embeddings; the rest are cache hits).
  - First trial attempt (2026-10-30): the index completed (generation 9,
    9579/9579 vectors, zero failed/pending) and the hybrid preflight
    passed, but all 12 trials recorded `spawn_failed` — `sudo`'s secure
    PATH lacks `~/.local/bin` (where `cline` lives), and the bench passes
    PATH through to the agent bridge. Fixed in the staging script (PATH
    export). Both suites now set `CLINE_NO_AUTO_UPDATE=1` in the adapter
    env — an unenforced pin, because the adapter's child-environment
    allowlist does not forward it (see limitations) — and the failed
    run's artifacts were removed so the experiment ids can be reused.
  - Second trial attempt (2026-10-30): the PATH fix worked (the CLI
    spawned), but all 13 trials failed with `no_run_result
    (exitCode=1)`: Cline CLI 3.0.74 changed prompt transport — it rejects
    stdin-only invocations ("JSON output mode requires a prompt argument
    or piped stdin"), while the adapter (verified against the 3.0.61-era
    headless reference) passed the prompt on stdin. Fixed in the adapter:
    the prompt now travels as the trailing positional argument (verified
    against the installed 3.0.74; adapter suite 13/13 and protocol
    conformance 23/23 pass). This was the first live run of the adapter
    against the real CLI. The failed runs' artifacts were removed again
    so the experiment ids can be reused.
  - Third trial attempt (2026-10-30): the prompt fix worked (the agent
    loop started on every trial), but all 13 trials failed with
    `finish_reason_error` after ~15 s with zero tokens — the Cline
    `lmstudio` provider fell back to its built-in default host
    (`localhost:1234`) instead of the gateway at `127.0.0.1:8080`, because
    the per-trial provider settings carried no endpoint and the suite had
    no field to declare one. Fixed in the engine contract: the suite
    profile's provider gained an optional `endpoint` (schemas,
    `ProviderSpec`, request forwarding), and the Cline adapter now
    materializes it in the per-trial provider settings as `baseUrl`
    (with a 10-minute per-request timeout, mirroring the operator's
    verified setting); the suites declare
    `http://127.0.0.1:8080/v1` (new hashes above), and the engine
    rebuilds from the fixed worktree commit.
  - `bench --dry-run` preflight correctly refuses hybrid conditions
    without verified corpus-wide vector coverage (observed on every
    partial-coverage generation).
- **Failed/flagged** (during preparation; first item since resolved):
  - At the 2048-token per-input limit, complete corpus coverage was
    impossible: the largest embeddable symbol chunk (`runController`,
    ~5k tokens) exceeded it. Resolved by the operator's 8192 per-input
    change; with 8192 every remaining chunk should fit (largest estimated
    ~5k tokens; not yet probed chunk-by-chunk).
  - One chunk is over the model's entire 8192-token context:
    `chatRoutes` in `src/routes/chat.ts` (57,909 chars, ~14.5k tokens).
    That file is excluded from the corpus (recorded confound).
- **Unmeasured**: model comparisons, harness comparisons, cross-repository
  effects, cold-cache behavior, and any speed/token improvement claims
  (the smoke run's single repetition cannot estimate effects).

## Run steps

The 8192 per-input limit is live and verified (4500- and 5500-token
probes accepted), and the root index is complete: generation 9,
9579/9579 vectors (the final 142 remapped chunks embedded 2026-10-30,
zero failed/pending) with the hybrid preflight passed. No prerequisite
remains; an optional pre-check that the embedding endpoint is serving:

```sh
curl -s http://127.0.0.1:8080/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model": "embeddinggemma-2", "input": ["probe"]}'
```

Executed on 2026-10-30 by the operator's `cline-agent` user (the
cancellation step via the staged `run-cancel.sh`, which sends SIGINT once
the trial's Cline CLI has spawned). The commands are recorded below for
reproduction (run as that user, e.g. via `sudo -u cline-agent`):

1. **Smoke chain** (one command; logs in `run-smoke.log` and
   `index-root.log`):

   ```sh
   sudo -u cline-agent bash /home/cline-agent/bench-pilot/run-smoke.sh
   ```

   It runs `index` (resumes from the embedding cache and embeds the
   remaining chunks), `status` (expect 0 pending vectors),
   `bench --dry-run` (the hybrid preflight must now pass), and
   `bench suite.json` (12 trials; artifacts in
   `/home/cline-agent/bench-pilot/out/ap06-smoke/`).
2. **Cancellation check**: run `bench suite-cancel.json` in the
   foreground with the same three environment variables the chain
   exports (`REPOISE_EMBED_ENDPOINT`, `REPOISE_EMBED_API_KEY`,
   `LMSTUDIO_API_KEY`), send SIGINT (Ctrl-C) mid-trial, and expect a
   cooperative cancellation and exit code 4; artifacts in
   `out/ap06-cancel/`.
3. **Record results**: `python3 /home/cline-agent/bench-pilot/summarize.py
   /home/cline-agent/bench-pilot/out ap06-smoke` (and `ap06-cancel`), then
   fill in the results sections of this document.

Benchmark artifacts are disposable local files; no repository or device
state is modified.

## Limitations

- **Smoke only, not a completed experiment**: the smoke run executed
  2026-10-30 and all 12 trials passed, but a single repetition verifies
  mechanics only — model comparisons, harness comparisons and the
  expansion run remain unmeasured.
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
- **Cline version pin not enforced**: the Cline adapter spawns the CLI
  with a fixed environment allowlist ([agent adapters](agent-adapters.md))
  that does not include `CLINE_NO_AUTO_UPDATE`, so the suites' pin does
  not reach the CLI and a background auto-update could change the
  version mid-run. Mitigations: each trial's adapter events record the
  exact CLI version (`harness_version`), and the operator confirms
  `cline --version` before and after the run. Forwarding the variable
  is a follow-up adapter change.
- **Local inference preset not fully exercised**: the evaluated preset's
  model is used, but the original `nomic-embed-text-v1.5` selection from
  the preset could not be loaded alongside the coding model under the
  current llama-swap configuration.
- **Engine gap (follow-up)**: `status` and the bench hybrid preflight
  compute vector pending across the entire corpus (docs+code+config),
  while the embedding scope covers docs+code only — so any corpus
  containing config files has permanently non-zero pending and the
  hybrid preflight is unreachable without a corpus policy (the role
  remap above) or an engine fix (scope-aware coverage). Flagged for a
  separate engine PR; it does not change retrieval behavior.
