# Offline lexical evaluation runbook (card K6, item 6)

Reproducible offline evaluation for the lexical-first deployment: frozen synthetic
fixtures, labeled queries with gold evidence, provenance validation, preset
comparison, and machine-readable results. The hybrid presets and the real agent
pilot are explicitly out of scope here (unmeasured, not approximated).

## What is measured

- Cold build time, corpus/index sizes, warm search latency (p50/p95, per
  CLI invocation, 21 iterations per query by default).
- Evidence recall@5 on held-out conceptual queries (search and history lanes).
- Exact lookup success (100% required) with per-hit provenance: path presence
  at the snapshot revision, SHA-256 content hash equality, line-range bounds,
  and history-lane revision/affected-path validation.
- Policy failures: secret material, cross-scope leaks, stale (dirty) text.
- Shared-cache scope isolation: both corpora indexed into one cache root, then
  foreign source ids are rejected from the other scope (both directions,
  content inspected), an in-scope exact read must return the exact snapshot
  text, and an earlier source id must be rejected as stale after the indexed
  file changes. Cases the fixtures cannot exercise are reported as unmeasured.
- Docs-only vs docs-code-lexical preset comparison on the shared acme corpus.
- Master-plan section 11 gates, each reported as measured-passed,
  measured-failed, or unmeasured.

## Prerequisites

- Node >= 22 and the Rust toolchain pinned by `rust-toolchain.toml`.
- No network access, provider credentials, or private content.
- `git` available (the harness replays the fixture commit history in a temp dir).

## Reproducible commands

```sh
cargo build --package repoise-cli        # native CLI (debug build is fine)
node scripts/eval/harness.mjs            # full run -> eval-results/ (git-ignored)
node scripts/eval/harness.mjs --iterations 5 --out /tmp/eval-quick   # quick loop
npm test                                 # release + eval tests (node --test)
node --test scripts/eval/*.test.mjs      # eval tests only
```

Useful options: `--out DIR`, `--iterations N` (warm samples per query),
`--binary PATH` (or `REPOISE_BIN`), `--replay-evidence FILE` (import a
validated real-agent replay document; see below).

Every run writes `results.json` (machine-readable: environment, versions,
declared settings, per-run metrics, per-query results, gates) and `report.md`
(concise summary). Results stay out of Git (`eval-results/` is git-ignored).

## Fixtures and held-out separation

- `fixtures/` holds three synthetic public-safe corpora: `acme` (docs +
  TypeScript/JavaScript + committed config), `docs-site` (docs only), and
  `pyrepo` (Python, exercised through the line-window fallback). All content
  is fictional; the `.env`/`secrets/` material is a fake sentinel string used
  to verify denial.
- `fixtures/manifest.json` freezes every fixture file by SHA-256 and declares
  the deterministic commit history (fixed dates, author, messages, per-commit
  file contents and one override commit), plus the dirty worktree operations.
  The harness rebuilds each corpus as a fresh temp git workspace and refuses
  to run if any fixture file diverges from the freeze. The replayed commit
  SHAs are therefore reproducible on a given git version and recorded in
  `results.json`.
- `queries.json` labels 60 queries: 48 in-tuning and 12 held-out, covering
  exact-identifier, unknown-concept, current-vs-plan, history, code-behavior,
  tests, unsupported-language (Python fallback), docs-only, dirty-stale,
  secret-policy, and cross-scope categories.
- Held-out separation: queries with `"heldOut": true` were not used to tune
  anything in this PR; recall@5 is computed only over held-out conceptual
  queries. Do not fold held-out queries into tuning when iterating on the
  engine; add new queries instead.

## Preset comparison

The harness runs acme under both `docs-only` and `docs-code-lexical` and
reports cold build, file/chunk/db sizes, warm latency, and fixed code probes
(`computeBatchId`, `reconcileLedger`, `legacyShimV2 items`) whose code hits
must stay zero under docs-only.

## Real-agent replay evidence (import format)

`replay-evidence.schema.json` defines the validated import format for the
owner-approved ai-gateway pilot (collected later). One document per run:

- `fixed`: model name + revision, the task set with correctness criteria, the
  repository revision, declared budgets, and prompt version — the conditions
  every run shares.
- `conditions`: the declared matrix (e.g. curated-docs-rg, lexical,
  docs-code-lexical, hybrid presets).
- `runs`: one entry per task x condition, each holding measured values only:
  model input/output tokens, retrieval output tokens, tool call count,
  elapsed ms, task correctness, retrieval errors, stale evidence, and sample
  size.

Validation is strict and fail-closed: unknown fields are rejected (derived
values such as token-savings percentages cannot be imported), non-integer
token counts are rejected, null task entries and duplicate task ids are
rejected, and every task x condition cell must be present. An invalid or
malformed `--replay-evidence FILE` stops the harness with clear diagnostics
and a nonzero exit before any indexing work.
`harness.mjs --replay-evidence FILE` imports a valid file, summarizes
measured per-condition values, and reports any reduction figure as
*derived at report time from measured totals* — it never treats a reduction
as a raw measurement. Aggregated token means divide by the summed
`sampleSize` (per-sample means), the correct rate is labeled task-level
(`taskCorrect` is a task-level boolean; successful-repeat counts are never
inferred), and the mean-based reduction is labeled descriptive: it does not
establish the planned median-reduction adoption gate (G5). Missing
measurements stay missing: gates G5/G6/G7 remain unmeasured until
real evidence exists.

## Acceptance targets (master plan section 11)

| Gate | Target | Source |
| --- | --- | --- |
| G1 exact lookup | 100% with valid provenance | harness `metrics.exact` + `metrics.provenance` |
| G2 policy | zero cross-scope / stale / secret leaks | harness `metrics.policy` |
| G3 held-out recall | >= 85% recall@5 | harness `metrics.recall` |
| G4 warm search | p95 <= 500 ms (per CLI invocation) | harness `metrics.warmP95Ms` |
| G5 token reduction | >= 20% median input-token reduction, no correctness decrease | real replay evidence only |
| G6 code vectors | >= +10 recall@5 points vs code lexical, acceptable cost | operator-configured hybrid run |
| G7 hybrid latency | p95 <= 2 s excluding embedding requests | operator-configured hybrid run |

This PR measures G1–G4 on the frozen fixtures; G5–G7 are unmeasured by design
until the owner-approved ai-gateway snapshot and a live agent pilot provide
the required measurements.

## Rollback

The harness is additive: fixtures, queries, scripts and tests under
`scripts/eval/` plus a `.gitignore` entry. Reverting the introducing commit
removes it without touching the engine, configuration schema, or release
tooling. No migrations, environment variables, or deployment actions are
required. Generated results under `eval-results/` can be deleted at any time.

## Known engine characteristics measured (not asserted away)

- Lexical ranking is BM25 density-driven: short files with shared identifiers
  in their path can outrank longer docs that match more terms. Role
  assertions therefore check classification (roles present and correct) and
  top-5 gold presence rather than strict current-over-plan ordering.
- Multi-word FTS queries are OR-per-term across columns (the shipped
  `fts_match_expression` builds unparenthesized OR chains joined by AND), so
  policy queries are authored with terms absent from the whole corpus.
- `search` pages default to 5 results; the harness requests the cap (20) to
  expose the full ranking and evaluates the top 5.
