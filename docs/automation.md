# CLI automation contracts

Status: implemented (AP-02, 2026-10-26). This is the current-state guide for
driving the CLI from unattended programs and editor UIs: the capabilities
handshake, the NDJSON event stream, stable exit codes, signal behavior and
writer-ownership detection. It does not change the existing `--json` output
shapes; `--events` is a separate explicit streaming mode.

Source entry points: `crates/repoise-cli/src/capabilities.rs`,
`crates/repoise-cli/src/events.rs`, `crates/repoise-cli/src/main.rs`,
`crates/repoise-core/src/ownership.rs`, `schemas/capabilities.v1.schema.json`,
`schemas/event-stream.v1.schema.json`.

## Capabilities handshake

```sh
repoise capabilities            # compact human summary
repoise capabilities --json     # the authoritative machine document
```

The JSON document (`schemaVersion: 1`) carries:

- `name`, `version` — the product and the exact CLI version;
- `eventStream` — the event-stream protocol version, its checked-in schema
  path, the operations that stream (`index`, `watch`) and the effective
  heartbeat interval in milliseconds;
- `features` — always-present capabilities (`event-stream`, `mcp-stdio`)
  plus enabled build features (`remote-embedding`, `github-enrichment`);
- `commands` — every command with its machine output modes
  (`json`, `events`, or `mcp-stdio`);
- `benchmark` — `supported: false` in this build; the planned `bench`
  coordinator (AP-04) is discoverable but not shipped;
- `exitCodes` — the stable exit-code table (below);
- `compatibility` — the compatibility policy (below).

The document is defined by `schemas/capabilities.v1.schema.json`.

## Event stream (`--events`)

`repoise index --events [ROOT]` and `repoise watch --events [ROOT]` put an
NDJSON event stream on **stdout** — protocol records only. All diagnostics
stay on **stderr**. Each line is one JSON object (envelope):

| Field         | Meaning                                                            |
| ------------- | ------------------------------------------------------------------ |
| `schemaVersion` | Protocol version (`1`); consumers reject versions they do not support |
| `runId`       | One id per run (`<operation>-<pid>-<start nanos hex>`)             |
| `sequence`    | Zero-based, strictly increasing within the run                    |
| `timestamp`   | UTC ISO-8601 with milliseconds (`...Z`)                           |
| `type`        | Event type (below); terminal types are exactly one per run        |
| `payload`     | Typed payload for the event                                       |

Event types:

- `started` — first event; payload carries `operation`, `pid` and the
  resolved `scope` (`root`, `repoId`, `worktreeId`, `snapshotMode`). Use it
  to reject a scope mismatch before relying on the stream.
- `progress` — phase-scoped work updates. Index payloads carry `phase`
  (`discovery`, `reconcile`, `embedding`, `history`, `publication`),
  `unit`, `done`, `total` (unknown until inventory finishes), `elapsedMs`
  and an optional `etaSeconds`. Watch payloads carry one record per
  publication: `generationId`, `changedPaths`, `filesReparsed`,
  `forceFull`, `branchSwitch`, `configChange`, `vectorsTotal`, `embedding`
  stats and `durationMs`.
- `warning` — non-fatal degradation or step failure (for example embedding
  configured but unavailable; a failed watch step). The run continues.
- `heartbeat` — liveness only, no progress claim. Default every 15 seconds;
  override with `REPOISE_EVENTS_HEARTBEAT_MS` (floored at 10 ms). A healthy
  process waiting on an embedding provider keeps heartbeating; a dead
  process does not.
- `completed` / `cancelled` / `failed` — the single **terminal** event.
  `completed` payloads carry `scope` and the full `result` (index outcome or
  the watch's final state); `failed` payloads carry `scope`, a stable
  `code` (`operational`), a context `message` and `retryable` (null when
  unknown). Stack traces and secret configuration values are never part of
  the protocol.

Exactly one terminal event is expected on controlled exit. A stream without
a terminal event is **incomplete output** (abrupt process death, OS kill);
consumers must treat it as such. Progress amplification is bounded:
in-phase progress events are throttled to at least 100 ms apart, while
phase starts, phase completions and the final update of a phase always
emit. ETAs are optional, phase-scoped and suppressed until observations are
reliable; they are never an overall percentage. Lexical coverage and
embedding coverage are reported separately (`total: null` means indeterminate).

## Exit codes

| Code | Meaning                                                            |
| ---- | ------------------------------------------------------------------ |
| 0    | ok (success, clean cancellation)                                  |
| 1    | operational failure (index, watch step, enrichment, I/O, …)      |
| 2    | usage error (unknown flags/arguments)                            |
| 3    | index missing or stale (`status`, `search`, `check --fresh`)     |
| 4    | coverage not met (`check`)                                        |

The table is advertised by `capabilities --json` and covered by tests;
automation should not parse human text for status.

## Signals and cancellation

- `watch` installs a `ctrlc` handler (with the `termination` feature on
  Unix). `SIGINT`, `SIGTERM` (and `SIGHUP` on Unix; Windows `Ctrl+C`) set a
  cancel flag; the loop stops at its next step boundary, preserves the last
  complete published generation, emits a `cancelled` terminal event
  (`reason: "signal"`) and exits `0`. The owner marker is released on the
  clean exit. No in-flight provider call is claimed to be interrupted;
  cancellation takes effect at the next check point.
- `index` has no graceful signal handler. A signal kills the process
  abruptly (no terminal event → incomplete output); the transactional
  publication guarantees the last complete generation survives.
- Windows process termination (`TerminateProcess`) is abrupt for both
  operations, the same way as any OS kill.

## Writer ownership

Publications are transactional (one SQLite immediate transaction per
generation), so a foreign writer can never corrupt a published generation.
The ownership marker exists for **detection**, not exclusion:

- Writers (`index`, `watch`) acquire an advisory marker
  `<cache>/repos/<repoId>/worktrees/<worktreeId>/owner.json` — scoped by the
  actual repository/worktree index scope, not by a remote URL. It records
  `pid`, `operation`, start time and the last refresh; writers refresh at
  most every 5 seconds.
- Acquisition never blocks or fails the operation. A recent (under 10
  minutes) marker from another process produces a stderr warning and a
  `warning`-style diagnostic, then the run proceeds; publications remain
  transactional.
- `status` exposes the marker read-only in an `owner` field
  (`pid`, `operation`, `startedAtMs`, `refreshedAtMs`, `state`), where
  `state` is `active` (refreshed within 10 minutes) or `stale`. A PID is
  never treated as proof of liveness (PIDs are reused); refresh freshness is
  the health signal. `purge` removes the marker with the scope directory.

## MCP boundary

The stdio MCP server remains JSON-RPC 2.0 on stdin/stdout; progress lines are
never injected into that stream. Progress for MCP clients will arrive only
through supported MCP protocol mechanisms and negotiated capabilities (see
[docs/mcp.md](mcp.md)).

## Compatibility policy

- Policy: **additive**. New event types and new fields do not bump the
  version; breaking changes bump `schemaVersion` (new major schema file).
- Consumers must ignore unknown event types and unknown fields.
- Consumers must reject an event stream whose `schemaVersion` they do not
  support (`capabilities.eventStream.supportedEventStreamVersions` lists
  what this producer supports).
- The checked-in schemas under `schemas/` are the normative definitions for
  each version.

## Validation

- Unit tests: `crates/repoise-cli/src/events.rs` (envelope fields, sequence
  ordering, single terminal, heartbeat, throttled progress) and
  `crates/repoise-core/src/ownership.rs` (acquire/inspect/release, stale
  detection, foreign-marker preservation).
- Integration tests: `crates/repoise-cli/tests/cli.rs`
  (`capabilities_json_is_a_stable_handshake`,
  `index_events_streams_ndjson_with_one_terminal`,
  `watch_events_cancels_with_a_terminal_event_on_sigint`,
  `status_reports_the_active_watch_owner_and_clears_it_after_exit`).
- Existing `--json` CLI and MCP behavior is unchanged and still covered by
  the existing suites.
