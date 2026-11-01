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
- `features` — always-present capabilities (`event-stream`, `mcp-stdio`,
  `exclusive-scope-lock`) plus enabled build features
  (`remote-embedding`, `github-enrichment`);
- `commands` — every command with its machine output modes
  (`json`, `events`, or `mcp-stdio`);
- `benchmark` — `supported: true`; the `bench` coordinator (AP-04) with its
  command, docs path and exit-code note, including the offline `bench report`
  rebuild (AP-05; see [bench](bench.md));
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
  Watch live-config issues additionally carry `kind` (`"invalid"`: the
  resolved configuration failed validation, so the watch publishes nothing
  until it is corrected; `"restart-required"`: a process-bound setting —
  `cacheDir`, `sharedCache` or `history` — changed and cannot move in
  process; the watch also publishes nothing in this state until the change
  is reverted or the process is restarted). The `message` is sanitized: it names the structurally broken
  file when identifiable and reports only the validation problem count —
  user-typed configuration values and credentials are never included (run
  `repoise doctor` for details). Repeated
  identical issues are throttled; the last published generation stays served
  through both states.
- `heartbeat` — liveness only, no progress claim. Default every 15 seconds;
  override with `REPOISE_EVENTS_HEARTBEAT_MS` (floored at 10 ms). A healthy
  process waiting on an embedding provider keeps heartbeating; a dead
  process does not.
- `completed` / `cancelled` / `failed` — the single **terminal** event.
  `completed` payloads carry `scope` and the full `result` (index outcome or
  the watch's final state); `failed` payloads carry `scope`, a stable
  `code` (`operational`), a context `message`, `retryable` (null when
  unknown) and, for writer-refusal failures, a stable machine `reason`
  (`scope-owned`: another writer holds the scope lock, retry once it exits;
  `lock-unavailable`: the lock file itself could not be used). Stack traces
  and secret configuration values are never part of the protocol.

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
| 3    | index missing or stale (`status`, `search`, `check --fresh`), or `bench` preflight/suite failure |
| 4    | coverage not met (`check`), or `bench` interruption (SIGINT/SIGTERM) |

The table is advertised by `capabilities --json` and covered by tests;
automation should not parse human text for status.

Exit code 4 reflects scope-aware embedding coverage: the served generation
is judged against its own persisted embedding scope (docs, or docs + code),
so lexical-only chunks outside the scope (e.g. config) never make coverage
unmet, and a stored profile over an empty eligible corpus is reported as a
deterministic unmet coverage (nothing pending), not as a failure. See the
[storage guide](storage.md#vector-coverage-semantics) for the full state
set (`disabled`, `profile-unavailable`, `empty`, `partial`, `complete`).

## Signals and cancellation

- `watch` installs a `ctrlc` handler (with the `termination` feature on
  Unix). `SIGINT`, `SIGTERM` (and `SIGHUP` on Unix; Windows `Ctrl+C`) set a
  cancel flag; the loop stops at its next step boundary, preserves the last
  complete published generation, emits a `cancelled` terminal event
  (`reason: "signal"`) and exits `0`. The scope lock is released by the OS
  when the process exits (the descriptive marker is removed on the clean
  exit). No in-flight provider call is claimed to be interrupted;
  cancellation takes effect at the next check point.
- `index` has no graceful signal handler. A signal kills the process
  abruptly (no terminal event → incomplete output); the transactional
  publication guarantees the last complete generation survives.
- `bench` handles SIGINT/SIGTERM per trial: the in-flight trial receives the
  cooperative `cancel` line (the bridge and the adapter's recorded process
  tree are force-killed after the grace period), remaining trials are
  recorded `not_started`, the report is still published and the exit code is
  4 (see [bench](bench.md#trial-lifecycle)).
- Windows process termination (`TerminateProcess`) is abrupt for both
  operations, the same way as any OS kill.

## Writer ownership

Exactly one writer per mutable index scope. `index`, `watch` and the opt-in
MCP refresh acquire an authoritative OS-backed lock on
`<cache>/repos/<repoId>/worktrees/<worktreeId>/scope.lock` (flock(2) on Unix,
`LockFileEx` on Windows) before any provider work or publication. The lock is
atomic across processes and is released by the OS when the owning process
dies (clean exit, crash or kill), so ownership never depends on PID
freshness or unsafe stale-file deletion. A second writer for the same scope
receives a stable, actionable busy result — exit code 1 for one-shot
operations, and a `failed` terminal event with the machine `reason`
`scope-owned` (or `lock-unavailable` when the lock file itself is unusable)
plus `retryable: true` in the `--events` stream — and never starts provider
work, publishes, or overwrites the live owner's metadata. A long-lived
`watch` can call indexing without deadlocking on its own lease: the lease
lives in the CLI/MCP/purge entry points, not in the shared index service.

The `owner.json` marker remains, but only as **descriptive heartbeat
metadata** for the process holding the lock: `pid`, `operation`, start time
and last refresh (refreshed at most every 5 seconds, published atomically via
sibling temp file + rename). A PID is descriptive only — never proof of
liveness (PIDs are reused) and never grounds for killing or adopting a
process.

- `status` reports the authoritative writer state in a `writer` field
  (`lockHeld`, `state`: `owned` | `busy` | `stale-metadata` | `free`) and
  still exposes the descriptive `owner` marker (`pid`, `operation`,
  `startedAtMs`, `refreshedAtMs`, `state` `active`/`stale` with the 10-minute
  window).
- `purge` holds each scope's lock before removing it, so an active writer's
  scope can never be removed from under it, and removal cannot be bypassed
  by unlinking/recreating the lock path. A scoped purge of an owned scope
  fails before removing anything; `--all` skips owned scopes and reports
  them in `skippedOwned`.
- Read-only operations (`status`, `search`, `read`, `related`, `check`) keep
  working while a writer owns the scope.
- Manual recovery: nothing is needed after a crashed writer — the OS
  releases the lock. A leftover `owner.json` is reported as
  `stale-metadata` and is replaced on the next successful acquisition.
- Filesystem assumptions: local filesystems (or single-client shares)
  where OS lock semantics hold. `flock` is advisory between cooperating
  Repoise processes; multi-client network filesystems (e.g. NFS) are not
  supported for shared scope locking.

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
  `watch_events_reports_config_issue_and_recovers`,
  `status_reports_the_active_watch_owner_and_clears_it_after_exit`).
- Existing `--json` CLI and MCP behavior is unchanged and still covered by
  the existing suites.
