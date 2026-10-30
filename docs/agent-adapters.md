# Agent adapters (AP-03)

Agent adapters are standalone, versioned Node 20+ executables that drive
external agents for controlled trials. The protocol is the contract; adapters
are its implementations; the benchmark runner (`repoise bench`, AP-04, see
[bench](bench.md)) is its consumer.

## Layout

| Component | Path | Role |
| --------- | ---- | ---- |
| Request schema | `schemas/adapter-request.v1.schema.json` | Versioned JSON request handed to an adapter on stdin |
| Events schema | `schemas/adapter-events.v1.schema.json` | NDJSON events an adapter emits on stdout |
| Protocol host | `packages/agent-adapter-protocol/` | Spawns an adapter, enforces the contract, returns one result |
| Conformance suite | `packages/agent-adapter-protocol/test/conformance.test.mjs` | Fake fixtures proving host enforcement |
| Cline adapter | `packages/agent-adapter-cline/` | First real adapter (`bin/agent-adapter-cline.js`) |

The protocol host is the only component that owns the child process. It
spawns the adapter in its own process group, writes exactly one JSON request
as the first stdin line (stdin stays open afterwards), enforces the hello
handshake, budgets and protocol rules, and always returns exactly one result
(`{status, reason, errors, warnings, unknownEventTypes, events, hello,
adapter, harness, usage, elapsedMs, adapterDurationMs, exitCode,
exitSignal}`); a `final` event is present only when the adapter emitted a
valid one (preflight, spawn and missing-final failures report
`finalEvent: null`). It writes the optional `eventsFile`/`resultFile` under
the artifact dir. Host-side enforcement includes the hello timeout,
per-line byte and event-count limits, strict envelope validation, usage
semantics (cumulative max vs delta sum), wall-clock escalation, and a
best-effort process-group sweep after every path, so a leaked grandchild of
the adapter's own process group can never outlive the trial. An adapter
that deliberately moves its children into another process group is
responsible for sweeping them itself; to give the host a backstop, such an
adapter MAY record the child tree's root pid in `child-pid.txt` under
`outputs.artifactDir` (POSIX: the process-group leader, so the host's
group kill reaches the whole tree; Windows: the root pid for the tree
kill), and the host sweeps that recorded pid best-effort after every path
(guarded: only pids above 1 that are neither the host nor its parent are
swept, and the host clears any stale file at run start, so a reused
artifact dir cannot leak a recycled pid) — this is what covers
protocol-violation kills, which SIGKILL the adapter before any of its own
cleanup can run.

**Cancellation is cooperative and cross-platform.** On cancellation (host
request or wall clock) the host first sends a `cancel` control line on the
adapter's stdin — the channel that works on every platform, because on
Windows signals cannot be delivered to console-less Node processes
(`taskkill` without `/F` only posts WM_CLOSE to windows). Adapters SHOULD
honor the line with a cooperative `cancelled` final. On POSIX the owned
process group additionally receives SIGINT, so adapters that never read
stdin keep the documented graceful path. After the grace window the
escalation is a forced kill (`SIGKILL` / `taskkill /F /T`).

After the `final` event the host keeps reading (UTF-8 safe across chunk
boundaries) until the adapter exits or a short grace window escalates, so
trailing events are detected and rejected.

## Contract invariants (enforced, not documented aspirations)

- The first stdout line must be a `hello` with `protocolVersion: 1` and a
  truthful `capabilities` map; missing required capabilities fail the
  preflight with `protocol_error/missing_required_capability` — never
  approximated.
- Exactly one `final` event per run; anything after it is a protocol error.
- Unknown event types are recorded in `unknownEventTypes` and passed through
  to the events file; they never fail the trial.
- Usage fields are normalized to
  `{inputTokens, outputTokens, reasoningTokens, cacheReadTokens,
  cacheWriteTokens, costUsd}`; unavailable fields are `null`, and
  `inputTokens`/`outputTokens` must be present (cost is optional).
- `final.status` is one of `completed`, `cancelled`, `agent_error`,
  `adapter_error`, `budget_stop`, `timeout`, `protocol_error`.

## Cline adapter

`packages/agent-adapter-cline` drives the installed Cline headless CLI
(`cline --json -c <workspace> ...`), verified against the Cline CLI
reference (2026-10-19) and re-verified against the installed CLI 3.0.74
(2026-10-30): that CLI rejects stdin-only invocations ("JSON output mode
requires a prompt argument or piped stdin"), so the prompt travels as the
trailing positional argument.

- One trial per invocation; `hello` is emitted before anything else.
- Trial state is isolated under `outputs.artifactDir/cline/`:
  `--config`, `--data-dir`, the per-trial `mcp-settings.json`
  (`CLINE_MCP_SETTINGS_PATH`), `CLINE_LOG_PATH`, and an
  **effective-config artifact** recording every effective guidance, skill,
  MCP server, model, limit and state location with credential values
  redacted (names only) and the prompt absent from the recorded argv
  (a `<prompt>` placeholder; the prompt is recorded as `promptLength`
  and `promptSha256` under `task`).
- `repoise` mode configures the trial's single MCP server
  (`condition.mcp`); `baseline` mode gets an empty server list. The
  isolated `--config` dir guarantees no global servers leak in.
- The prompt travels as the trailing positional argument (the 3.0.74 CLI
  rejects stdin-only invocations; the child's stdin is closed without
  data); the credential (from
  `provider.credentialRef`, only `env:` references are honored) travels
  through the `-k` flag at spawn time and never through the request, the
  effective-config artifact, or any event. Note the `-k` value is visible in
  the child's argv to other local users on shared hosts; forwarded Cline
  output is additionally scrubbed of the resolved credential value
  (substituted with `[REDACTED]`) at every forwarding site, redacting
  before any truncation so a key straddling a truncation boundary cannot
  leak; the mapped `finishReason` passed into `final.reason` goes through
  the same redaction.
- Cline's `--auto-approve` is always on (trials need unattended operation);
  therefore `read_only_restriction` is declared `false` in the hello — the
  runner must enforce workspace boundaries, and adapters that lack the
  capability fail preflight rather than approximate it.
- Model/provider/reasoning/compaction map to `-m`, `-P`, `--thinking`,
  `--compaction`; `limits.wallClockMs` maps to the CLI `-t` backstop in
  seconds and is also enforced by the host. `maxToolCalls` is declared
  unenforceable (no per-tool telemetry) and produces a warning, never a
  silent approximation.
- The optional `profile.provider.endpoint` (e.g. an OpenAI-compatible base
  URL) is materialized in the Cline per-trial provider settings
  (`data/settings/providers.json` under the isolated state dir) as
  `baseUrl` with a 10-minute per-request timeout, so the CLI does not fall
  back to its built-in default host (`localhost:1234`); the endpoint is
  recorded in the effective-config artifact, and the credential never
  enters that file.
- The agent's final message text (the `done` event's `text`) is recorded
  on the `final` event as `data.answer` (null when no final text was
  produced), so deterministic benchmark grading — suite `answerField`,
  default `answer` — can score the recorded answer without a transcript.
- The Cline `run_result` (with its `finishReason`) is the sole completion
  signal: `completed` → `completed`, aborts during host cancellation →
  `cancelled`, a CLI failure without `run_result` →
  `adapter_error/no_run_result`, other finish reasons → `agent_error`.
- Usage events are declared cumulative; the adapter reports the maximum
  observed value per field. Per-token content events are not amplified into
  protocol events (recorded as a single note at most).
- `CLINE_EXECUTABLE` (absolute path or bare name) overrides resolution;
  a missing explicit path is `adapter_error/cline_not_found`. Cross-platform
  launch: an explicit `.js`/`.mjs`/`.cjs` path is launched through
  `process.execPath` (Windows `CreateProcess` cannot execute shebang
  scripts); a bare `cline` resolves through `PATH` at spawn time — on
  Windows the npm `.cmd` shim cannot be spawned without a shell, so use an
  explicit script path or a native executable there.
- Cancellation is race-free: the Cline child runs in its own process group
  on POSIX (detached spawn), so the host's group-level SIGINT reaches only
  this adapter — the child's `aborted` result can no longer race the
  adapter's own handler and be misreported as `agent_error`. The adapter
  honors both the host's `cancel` control line and SIGINT/SIGTERM (handlers
  registered before the request is read, so a cancel during the stdin read,
  version probe or spawn still produces a final), and signals the child's
  whole tree explicitly (`process.kill(-pid, ...)` on POSIX, reaching any
  MCP servers the CLI spawned).
- Nothing in the child group outlives the trial: the request and every
  control line share one stdin line consumer, so a `cancel` arriving in the
  same chunk as the request is never discarded, and a stop that lands
  before the spawn suppresses the spawn itself (cooperative `cancelled`
  final without starting the CLI). The adapter sweeps the child group
  (SIGKILL, tolerating ESRCH) on every final, on process exit, and when
  its own stdout closes (host death), records the child's root pid in
  `child-pid.txt` under the artifact dir so the host can backstop it, and
  its stop escalation (3 s) is shorter than the host's default grace
  (5 s), so a host forced-kill cannot orphan the group. The sweeps are
  best-effort; a host protocol-violation kill is covered by the host's
  `child-pid.txt` backstop (above).
- `CLONE_*`-prefixed environment variables are the documented
  adapter-specific passthrough namespace to the spawned CLI (used by the
  test stub); the rest of the environment is a minimal allowlist
  (`PATH`, `HOME`, `USERPROFILE`, `SystemRoot`, `TMPDIR`, the Cline
  state variables and the credential variable).

## Testing

```sh
node --test packages/agent-adapter-protocol/test/conformance.test.mjs
node --test packages/agent-adapter-cline/test/adapter.test.mjs
```

Both packages also define a `test` script (`node --test test/*.test.mjs`), so
`pnpm exec turbo run test` — the CI Check-job step — runs the suites
alongside the root test task, and the Prettier globs cover `packages/**`.
The conformance suite runs fake fixture adapters through the real host and
covers the happy path, handshake rejections, usage semantics, framing
violations, budgetless stalls, failed children, host cancellation (including
cooperative cancellation over the stdin `cancel` control line with no signal
delivered), grandchild-kill verification on hangs, grandchild sweep after a
clean adapter exit, host timeout not replaced by a cooperative cancelled
final, events after the final, spawn failure, blank first lines, a throwing
`onEvent` callback, schema-minimal requests, the host's `child-pid.txt`
sweep of a recorded child group after a protocol violation, clearing of a
stale `child-pid.txt` at run start (plus the guard that never sweeps pid 1
or the host's own pids), and acceptance
of the protocol's own happy path (request plus every event) by the
checked-in schemas (validated with ajv). The adapter suite drives the
real Cline adapter through the real host with a stub Cline CLI, asserting
the exact argv/env the harness observed, per-trial MCP settings,
effective-config redaction (credential and prompt), status mapping for
cancellation and failure,
rejection of non-`env:` credential references, credential scrubbing from
forwarded CLI output (including a key straddling the unparsed-line
truncation boundary), a `cancel` control line arriving in the same stdin
chunk as the request, a leaked grandchild inside the Cline child group
being swept with the trial, cross-platform launching of explicit Node
script executables, and acceptance of the adapter's own request and every
emitted event (including the totals-shaped final usage) by the checked-in
schemas (validated with ajv).

## Status and boundaries

- Implemented: protocol v1 schemas, host, conformance fixtures, Cline
  adapter with its tests.
- Not implemented: `tool_telemetry` and `read_only_restriction` in the Cline
  CLI (declared false), and live Cline verification.
- The benchmark runner (AP-04) is implemented and consumes the protocol via
  the checked-in host bridge; independent post-trial grading, paired
  metrics and reports (AP-05) are implemented as well; see
  [bench](bench.md).
- Private evaluation corpora belong in locally configured directories
  (e.g. `~/repoise-eval/`), never in this repository; the repo holds
  synthetic fixtures only.
