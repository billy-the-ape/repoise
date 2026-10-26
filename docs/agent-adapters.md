# Agent adapters (AP-03)

Agent adapters are standalone, versioned Node 20+ executables that drive
external agents for controlled trials. The protocol is the contract; adapters
are its implementations; a future runner (AP-04) is the consumer.

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
to its stdin, enforces the hello handshake, budgets and protocol rules, and
always returns exactly one result (`{status, reason, errors, warnings,
unknownEventTypes, events, hello, adapter, harness, usage, elapsedMs,
adapterDurationMs, exitCode, exitSignal}`); a `final` event is present only
when the adapter emitted a valid one (preflight, spawn and missing-final
failures report `finalEvent: null`). It writes the optional
`eventsFile`/`resultFile` under the artifact dir. Host-side enforcement
includes the hello timeout, per-line byte and event-count limits, strict
envelope validation, usage semantics (cumulative max vs delta sum),
wall-clock escalation (SIGINT → SIGKILL), and a best-effort process-group
sweep after every path, so a leaked grandchild can never outlive the trial.
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
reference (2026-10-19); live behavior is not yet measured.

- One trial per invocation; `hello` is emitted before anything else.
- Trial state is isolated under `outputs.artifactDir/cline/`:
  `--config`, `--data-dir`, the per-trial `mcp-settings.json`
  (`CLINE_MCP_SETTINGS_PATH`), `CLINE_LOG_PATH`, and an
  **effective-config artifact** recording every effective guidance, skill,
  MCP server, model, limit and state location with credential values
  redacted (names only).
- `repoise` mode configures the trial's single MCP server
  (`condition.mcp`); `baseline` mode gets an empty server list. The
  isolated `--config` dir guarantees no global servers leak in.
- The prompt travels on stdin; the credential (from
  `provider.credentialRef`, only `env:` references are honored) travels
  through the `-k` flag at spawn time and never through the request, the
  effective-config artifact, or any event. Note the `-k` value is visible in
  the child's argv to other local users on shared hosts; forwarded Cline
  output is additionally scrubbed of the resolved credential value
  (substituted with `[REDACTED]`) at every forwarding site, redacting
  before any truncation so a key straddling a truncation boundary cannot
  leak.
- Cline's `--auto-approve` is always on (trials need unattended operation);
  therefore `read_only_restriction` is declared `false` in the hello — the
  runner must enforce workspace boundaries, and adapters that lack the
  capability fail preflight rather than approximate it.
- Model/provider/reasoning/compaction map to `-m`, `-P`, `--thinking`,
  `--compaction`; `limits.wallClockMs` maps to the CLI `-t` backstop in
  seconds and is also enforced by the host. `maxToolCalls` is declared
  unenforceable (no per-tool telemetry) and produces a warning, never a
  silent approximation.
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
violations, budgetless stalls, failed children, host cancellation,
grandchild-kill verification on hangs, grandchild sweep after a clean
adapter exit, host timeout not replaced by a cooperative cancelled final,
events after the final, spawn failure, blank first lines, a throwing
`onEvent` callback, and schema-minimal requests. The adapter suite drives the
real Cline adapter through the real host with a stub Cline CLI, asserting
the exact argv/env the harness observed, per-trial MCP settings,
effective-config redaction, status mapping for cancellation and failure,
rejection of non-`env:` credential references, credential scrubbing from
forwarded CLI output (including a key straddling the unparsed-line
truncation boundary), and cross-platform launching of explicit Node
script executables.

## Status and boundaries

- Implemented: protocol v1 schemas, host, conformance fixtures, Cline
  adapter with its tests.
- Not implemented: `tool_telemetry` and `read_only_restriction` in the Cline
  CLI (declared false), live Cline verification, the runner (AP-04), and
  suites/metrics (AP-05).
- Private evaluation corpora belong in locally configured directories
  (e.g. `~/repoise-eval/`), never in this repository; the repo holds
  synthetic fixtures only.
