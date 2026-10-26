# AP-03: consumer-owned agent runner protocol

Status: implemented (2026-10-26). Depends on [AP-01](01-workspace-and-ci.md) and
[AP-02](02-cli-contracts.md). Agree the trial schema with AP-04/AP-05 first.

## Implementation status

Shipped as protocol v1 in `packages/agent-adapter-protocol/` (host plus the
checked-in NDJSON schemas under `schemas/`) and the Cline adapter in
`packages/agent-adapter-cline/`, with 22 protocol conformance tests and 13
adapter tests (stub-CLI driven through the real host). Implemented decisions
and deviations from the proposals below are documented in
[docs/agent-adapters.md](../../agent-adapters.md):

- The adapter executable override is `CLINE_EXECUTABLE` (not the proposed
  `REPOISE_BENCH_RUNNER`, which remains a runner-level proposal for AP-04).
- Credentials are accepted as `env:` references only and travel to the CLI
  through its documented per-run key flag; they never enter the request,
  the effective-config artifact or events.
- The Cline hello declares `tool_telemetry` and `read_only_restriction`
  false; `read_only_restriction` therefore fails preflight when required,
  and `maxToolCalls` produces a warning instead of a silent approximation.
- Completion is keyed solely on the Cline `run_result`; a CLI failure
  without `run_result` is `adapter_error/no_run_result`.
- Cancellation is cooperative and cross-platform: the host keeps the
  request stdin open and sends a `cancel` control line on cancellation
  (the only graceful channel on Windows, where signals cannot reach
  console-less Node processes; POSIX adapters additionally receive a group
  SIGINT), escalating to a forced kill after the grace window. The Cline
  child runs in its own process group on POSIX so the child's `aborted`
  result cannot race the adapter's own signal handler, and the adapter's
  SIGINT/SIGTERM handlers are registered before the request is read. The
  adapter additionally owns its child group: it sweeps it (SIGKILL) on
  every final, on exit, and when its own stdout closes (host death), its
  3s stop escalation is shorter than the host's default 5s grace, one
  stdin line consumer feeds both the request and the control lines (a
  `cancel` in the same chunk as the request is never discarded), and a
  stop that lands before the spawn suppresses the spawn. The adapter also
  records the child's root pid in `child-pid.txt` under the artifact dir,
  and the host sweeps that recorded pid after every path - the backstop
  that covers protocol-violation kills, which SIGKILL the adapter before
  any of its own cleanup can run.
- The checked-in schemas were corrected to match the implemented contract
  (request description: stdin stays open for `cancel` control lines;
  hello usage-semantics object scoped to `t: hello`; final usage carries
  the normalized totals shape with an optional `estimated` flag;
  `profile.sampling` accepts null), and both test suites validate their
  own requests and event streams against the schemas with ajv so
  schema/validator drift fails CI.
- The conformance suite includes the fake adapters, framing and
  grandchild-kill tests; no live Cline run has been executed.

## Outcome and boundaries

Repoise launches the user's real agent; that agent owns its model requests,
reasoning, tools, approvals, compaction and editing. An OpenAI-compatible model
endpoint is not an agent harness. Providing endpoint/model/key environment
variables alone cannot reproduce a consumer workflow.

Ship a reusable external adapter interface and one Cline implementation under
`packages/agent-adapter-cline/`. Consumers may supply another executable in any
language. Model profiles are configuration separate from adapter identity: using
two models in Cline must not require two adapters. No benchmark implementation
should be copied into every consumer repository.

## Invocation contract

Select an adapter through explicit config/CLI or a proposed
`REPOISE_BENCH_RUNNER` executable-path override. These names are design proposals,
not installed environment variables. Use an executable plus argument array, never
a shell command template. Keep prompts, paths and settings in a versioned JSON
stdin request. Reserve stdout for bounded NDJSON protocol events and stderr for
diagnostics. Support Windows executables/shims through deliberate cross-platform
launching rules; quoting paths with spaces must not invoke a shell accidentally.

First handshake advertises adapter/harness versions and capabilities: fresh
sessions, isolated state, MCP configuration, read-only restrictions, tool telemetry,
usage semantics, model selection and cancellation. Unsupported required
capabilities fail preflight; they are never silently approximated.

| Request field group | Purpose                                                                                               |
| ------------------- | ----------------------------------------------------------------------------------------------------- |
| Identity            | Protocol version, experiment/trial/task IDs, repetition                                               |
| Workspace           | Explicit resolved root, snapshot/commit, allowed write area, scope                                    |
| Task                | Prompt or coordinator-supplied prompt file, task version/hash; no grading answer                      |
| Condition           | Approved tool availability, MCP server argv/config, retrieval mode and guidance overlay               |
| Agent/model profile | Harness settings, provider references, model/revision, sampling/context/reasoning/compaction settings |
| Limits              | Wall time, calls/iterations, available token/cost budgets, output/event bounds                        |
| Outputs             | Trial artifact directory and coordinator-owned event/result destinations                              |

Use environment variables for opt-in credential references and process
configuration. Do not dump inherited environment, raw keys or auth-bearing URLs
into the reproducibility manifest. Record redacted effective settings and hashes
of non-secret inputs. Configuration lookup alone must never execute repository
content; running an explicitly selected agent/adapter is the execution action.

## Results, usage and observability

Events identify model-request start/end when available, tool start/end, usage,
artifact references, warnings and final status. Final status distinguishes task
completion, agent error, timeout, cancellation, budget stop and adapter/protocol
error. Independent evaluation owns correctness; process exit zero or an agent's
claim of success is not proof.

Preserve raw harness/provider usage alongside normalized fields and their source.
Declare whether input totals include cache reads, whether usage events are deltas
or cumulative, and whether subagent/compaction requests are included. Do not sum
incremental events and final totals twice. Missing output/reasoning/cache/cost
measurements remain null with a reason, never zero. Estimates are tagged and are
not interchangeable with provider-reported token counts.

The coordinator measures external monotonic elapsed time. Harness-reported agent
duration is a separate value; process startup, queue time and shutdown may differ.
Tool transcripts must expose enough data to verify actual Repoise mode, lexical
degradation, fallback calls and errors. If the harness cannot expose this,
declare the limit and reject conditions requiring verification or use an explicit
instrumentation wrapper. Do not invent observations from a final answer.

## First adapter: Cline

Start with the installed headless CLI where its behavior/telemetry suffices; use
the full SDK harness only if necessary and document the resulting differences.
Do not build a minimal replacement loop and call it Cline. Current official docs
describe CLI NDJSON events, final usage/duration, isolated config/data directories,
working-directory selection, and SDK session usage methods. Verify exact flags,
event schemas and effective behavior against the pinned installed release.

Preserve the normal system prompt/tools, provider settings and relevant project
rules. Create a fresh conversation and isolated agent state for every trial;
fresh task history does not imply a cold provider prompt cache. Configure Repoise
MCP per trial without rewriting global user settings. Record all effective
guidance, skills, plugins and auto-loaded context. Baseline removes only
Repoise-specific integration; other project rules stay consistent.

Match existing user workflow where practical, then document any differences
between CLI, VS Code extension and the user's Kanban fork. Sharing a model or
some core code does not establish equivalent prompts, tools, defaults or context.
Results must name the surface/harness revision that was actually executed.

Handle approvals through explicit benchmark policy and disposable workspaces,
not an unattended interactive prompt. Distinguish an approval block from model
failure. Keep optional teams/subagents consistent across conditions and include
their usage; disable them for initial controlled trials if reliable accounting
is unavailable. No automatic upgrades, account sign-ins or model/server changes.

## Conformance, validation and distribution

Provide synthetic fake-adapter fixtures before live inference: successful run,
bad version, missing telemetry, cumulative/delta usage, malformed/truncated JSON,
oversized events, stalled process, failed child, cancellation and leaked child
process prevention. Validate explicit workspace binding and isolated MCP config.
Use a stub harness for adapter tests; live local inference is a manual pilot.

Declare tested adapter/harness/OS versions and required optional installation.
Never install an agent or fetch executable code merely because Repoise starts.
Pin dependencies and document licenses. A packaged optional adapter must not
become a mandatory dependency of native lexical operation or the thin npm
launcher. Unsupported harnesses can still participate through the documented
protocol; they do not need changes in Repoise core.

## Open decisions and acceptance

Resolve exact Cline version, telemetry sufficiency, CLI versus SDK choice,
provider-setting inheritance, credential reference syntax, terminal status/exit
mapping, cancellation grace period and maximum event sizes. Define model-profile
precedence and immutable effective config before supporting multi-model runs.

Acceptance: a fake and a Cline adapter use the same protocol; repeated trials do
not share conversation state; measurements are attributed without double counts;
unsupported capabilities fail honestly; cancellation cleans only owned processes;
all retrieval-mode checks can be observed. No live benchmark result is claimed
from adapter conformance tests.

References checked October 19, 2026:
[Cline CLI reference](https://docs.cline.bot/cli/cli-reference),
[Cline full harness](https://docs.cline.bot/sdk/clinecore).
