# AP-06: ai-gateway pilot and broader experiments

Status: first smoke experiment prepared and verified 2026-10-29 on the
AP-04/AP-05 harness, execution pending the operator running the staged
smoke chain (root index complete, hybrid preflight passed; see the
[pilot status guide](../../pilot.md)); the expansion below remains
proposed execution/integration work, after [AP-04](04-benchmark-runner.md)
and [AP-05](05-suites-metrics-reports.md). Consumer tasks/configuration belong in
a separate ai-gateway PR or an explicitly selected private suite location. This
plan does not authorize running hundreds of agents or changing inference servers.

## First experiment

Use ai-gateway because it is an actual consumer with existing lexical/Cline
experience and a newly configured embedding index. Select a fresh immutable
consumer commit and exact installed Repoise build; earlier pilot commits and
token totals are historical context, not a substitute for a controlled baseline.

Start with a small smoke experiment (a few tasks, all three conditions) to verify
isolation, independently correct grading, usage accounting, actual hybrid calls,
degradation handling, MCP scope and cancellation. Review raw traces before scaling.

Then target 15-20 varied tasks, baseline/MCP lexical/MCP hybrid, five repetitions:
225-300 agent trials. This is an initial engineering sample, not a statistical
power guarantee. Use observed task-level variance and practical thresholds to
choose additional tasks/repetitions. Reserve some tasks from tuning. Twenty
repeats are an option after validating measurements, not a default requirement.

Hold one Cline runtime/model profile fixed initially. Match the user's actual
provider/tool/guidance configuration where practical and record differences from
the VS Code/modified Kanban workflow. Run sequentially on the local shared GPU;
randomize condition order within task/repetition blocks. No simultaneous competing
agent runs in the initial experiment. Pin configuration and archive provenance.

## Local inference controls

Record coding model artifact/quantization/context/cache settings, embedding
artifact/input profile/dimensions and hardware/driver. Keep existing model/server
settings unless separately authorized. Check whether embeddings and coding model
remain resident together; record any eviction/load delay in the user-visible
timing. No inference-stack tuning should be mixed into the retrieval comparison.

Separate ready-index task performance from cold index construction/maintenance.
Measure both. Keep prompt cache, query-embedding cache, index reuse and cross-tree
content-cache policies explicit. Fresh conversation does not imply cold model
cache; repeated identical prompts can disproportionately benefit later trials.
If cold-cache behavior cannot be controlled safely, report that limit rather
than claiming a cold comparison. Pause unrelated load or record it as a confounder.

Preflight complete compatible vector coverage. Reject/flag degraded hybrid rather
than calling it successful embedding evaluation. A faster lexical fallback during
an embedding outage is valuable operational evidence but a different condition.

## Expand in stages

Add multiple Python and TS/JS repositories, small/large eligible corpora and a
monorepo after the first experiment is stable. Candidate public corpora mentioned
in v1 include TypeScript, VS Code, CPython and Kubernetes; these remain candidates,
not commitments or completed results. Choose bounded snapshots/policies and
respect licenses and resource costs. Do not index arbitrary private files by default.

Next compare multiple models under one harness, then multiple harnesses under
matched model profiles. Keep adapter versions, system prompts, tool definitions,
reasoning and compaction settings observable. Token counts across tokenizers and
harnesses need context; correctness and elapsed time remain primary outcomes.

Avoid an accidental full factorial: four repos, three harnesses, three models,
three retrieval conditions, twenty tasks and five repeats already means 10,800
trials. Dry-run counts and total budgets must make this visible. Expand selected
dimensions deliberately; report per-repository/task effects before pooled claims.

## Acceptance and operating questions

Deliver the frozen suite/profile, redacted reproducibility manifest, complete
per-trial records, report and limitations. Distinguish measured, failed and
unmeasured gates. Review correctness and measurement gaps before enabling hybrid
by default or claiming significant token/latency improvement. Public reports
exclude private source/transcripts unless explicitly approved for publication.

Before execution the owner selects consumer SHA, model/harness versions, task
rubrics, held-out split, run budgets, allowed resource sampling and retention.
Record deployment requirements prominently in the consumer PR; no settings or
device changes are assumed. If later authorized device/service changes occur,
add the final homelab-documentation PR. Rollback removes pilot integration and
retains normal Repoise/agent operation; reports are disposable local artifacts.
