# AP-06 pilot: ai-gateway smoke experiment (status)

Status: first smoke experiment **prepared and verified, execution pending**
(2026-10-29, America/Denver) on the AP-04/AP-05 harness against the real
consumer `ai-gateway`. Every component — suite, config, patch snapshot,
harness, adapter, embedding endpoint and preflight gates — is staged and
validated (the embedding endpoint's 8192 configuration is probe-verified;
the `embeddinggemma-2` server itself was evicted by a concurrent coding
task and must be loadable again before the run). The blocking llama-swap
change (per-input batch size 8192) was applied by the operator on
2026-10-29 and verified by probe (4500- and 5500-token inputs
accepted); the run is ready to execute once `embeddinggemma-2` is
loadable again (see [Run steps](#run-steps)). No trial has run and nothing
here claims
speed, token or correctness results. The 15–20-task expansion described in
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
| Actual hybrid calls (vectors served by the trial MCP, no silent lexical degradation) | Not executed; staged (needs full vector coverage from the `index` step) |
| Degradation handling | Not executed; staged |
| MCP scope (read-only server bound to the trial worktree) | Not executed; staged |
| Cancellation (SIGINT, cooperative `cancel`, exit 4) | Not executed; suite staged |

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
- **Unmeasured**: all trial-level gates (the 12 smoke trials),
  cancellation behavior, model comparisons, harness comparisons,
  cross-repository effects, cold-cache behavior, and any
  speed/token improvement claims.

## Run steps

The 8192 per-input limit is live and verified (4500- and 5500-token
probes accepted), and 9437/9579 chunks are embedded (docs+code complete;
the 142 remapped config chunks are the only gap). One prerequisite
remains before the chain: the `embeddinggemma-2` server must be loadable
again — it was evicted by a concurrent `Gemma-4-E4B` coding task and
currently crashes on load. Pause that task (or restart the gateway),
then verify with a tiny probe:

```sh
curl -s http://127.0.0.1:8080/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model": "embeddinggemma-2", "input": ["probe"]}'
```

Everything is staged; the remaining steps run on the operator machine
(all artifacts live under `/home/cline-agent/bench-pilot/` and must be
run as that user, e.g. via `sudo -u cline-agent`):

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

- **Not a completed experiment**: no trial has run; this document records
  preparation, verification evidence and the exact remaining steps. It is
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
- **Engine gap (follow-up)**: `status` and the bench hybrid preflight
  compute vector pending across the entire corpus (docs+code+config),
  while the embedding scope covers docs+code only — so any corpus
  containing config files has permanently non-zero pending and the
  hybrid preflight is unreachable without a corpus policy (the role
  remap above) or an engine fix (scope-aware coverage). Flagged for a
  separate engine PR; it does not change retrieval behavior.
