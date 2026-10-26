# AP-02: CLI automation and lifecycle contracts

Status: implemented (2026-10-26); see the [implementation record](#implementation-record-2026-10-26)
at the end of this document for the shipped design, validation and deferred items.
Depends on [AP-01](01-workspace-and-ci.md).
Consumers: agent adapters, benchmark coordinator and VS Code extension.

## Outcome

Expose the same operations to terminal users, unattended programs and editor UI.
Keep shared indexing/search services in Rust. The extension must not scrape
terminal spinners or implement a second indexer/watch loop. Preserve existing
JSON response shapes and scope/hash validation; introduce a separate explicit
streaming mode rather than changing existing `--json` output silently.

Source entry points: `crates/repoise-cli/src/main.rs`, `output.rs`, `mcp.rs`;
`crates/repoise-core/src/indexing.rs`, `watch.rs`, `status.rs`, `check.rs`;
`schemas/`, `docs/mcp.md`, `docs/agents.md`, `docs/development.md`.
Coordinate existing indexing-progress work and PR #24 before touching output.

## Proposed public surface

Exact flag names are settled in this PR before implementation; examples here are
intent, not valid installed commands. Provide a version/capabilities handshake,
an explicit NDJSON progress mode for supported operations, and machine-readable
final outcomes. Capabilities identify CLI/protocol versions, enabled build
features and available commands. Editor/adapter clients fail with an actionable
compatibility error when a required capability is missing.

Each event envelope includes schema version, operation/run ID, sequence, UTC
timestamp, event type and typed payload. Use monotonic time for elapsed duration.
Define start, phase/progress, warning, heartbeat, completion, cancellation and
error events. Exactly one terminal event is expected on normal controlled exit;
an abrupt process death must be recognizable as incomplete output. Bound event
size/frequency and parsing buffers; preserve backpressure and avoid per-token
event amplification at the CLI boundary.

Machine stdout contains protocol records only; diagnostics go to stderr. Human
terminal output may render progress from the same events. Structured failures
carry stable codes, context and retryability when known; stack traces and secret
configuration values do not become the protocol. Existing MCP stdout remains
JSON-RPC only: never inject progress lines into that stream. Add MCP progress
only through supported protocol mechanisms and negotiated capabilities.

## Index progress and ETA

Report phases such as inventory, hashing/chunking, lexical publication and
embedding work with their own units and denominators. Distinguish cached,
embedded, failed and pending inputs. Include provider request/elapsed counters
where measured. A healthy process waiting on an embedding provider should be
visibly distinguishable from a process that exited or stopped emitting events.

Percentage requires a known denominator; inventory may initially be indeterminate.
ETA is optional, phase-scoped and explicitly estimated. Suppress it until there
are enough observations; account for cache hits, retries, batch size and provider
delays. Do not present 100% vectors when only lexical indexing completed, or claim
an overall percentage based on uncalibrated phase weights. Fresh lexical state
and incomplete embedding coverage can coexist and must be reported separately.

## Lifecycle and operation ownership

- Commands target an explicit root/worktree/snapshot. Include resolved scope in
  start/final records; the caller can reject a mismatch before relying on output.
- Watch supervises saved filesystem changes, not editor buffers. Report effective
  configuration changes, branch switches, publication and coverage truthfully.
- Cancelling index/watch preserves the last complete usable generation. Follow
  actual embedding cancellation semantics; do not claim interruption of an
  in-flight provider call if the transport only stops after it returns.
- SIGINT/SIGTERM and Windows process termination need documented behavior. Use
  bounded graceful shutdown then escalation for owned subprocesses; clean up
  process trees without killing a user-managed watcher or model server.
- Multiple clients must not blindly create competing writers. Define how callers
  detect an existing owner/lock, inspect health and handle stale ownership. Scope
  any lock by actual worktree/index identity, not merely repository remote URL.

The ownership mechanism is an implementation decision: inspect existing SQLite
and index publication concurrency before adding process locks or a daemon.
No always-on daemon is required by this plan. Read-only MCP clients may coexist
with a watch writer under the existing storage contract. An editor reconnect
must not infer ownership merely because a PID was reused.

## Benchmark facade and runtime boundary

The intended user-facing command is `repoise bench`. It must work outside VS Code.
AP-04 decides whether the coordinator is native or an optional packaged Node
companion. A companion cannot be an implicit download or a new requirement for
native indexing/search. If needed, document explicit installation, executable
resolution, compatible versions and a missing-companion error. Never embed Cline
or implement model requests/tool loops in the Rust search core.

Capabilities must make optional benchmark support discoverable. Its final report
and streaming events should use the same envelope conventions. Preserve the
distinction between command failure, agent failure, evaluator failure and a
completed benchmark containing unsuccessful trials. Settle precise exit codes
with AP-04; do not reuse the existing stale-index code ambiguously.

## Acceptance and tests

Provide checked-in schemas and deterministic fake-operation fixtures. Verify
schema compatibility, one terminal outcome, sequence ordering, stdout purity,
stderr diagnostics, slow reader behavior, provider waits, partial vectors,
cancelled publications, malformed client input and abrupt subprocess death.
Retain existing CLI/MCP equivalence and unchanged JSON fixtures. Test explicit
worktree selection and Windows as well as POSIX cancellation. Use a fake provider
and controlled delays; no production model/server settings are required.

Update current-state CLI/MCP/development guides only for implemented contracts.
Publish a compatibility policy for additive events, unknown fields/events,
breaking versions and supported producer/consumer ranges. Clients should ignore
documented optional event types but reject incompatible required fields/versions.

## Open decisions

Choose streaming flag/capability command names, exact schemas and CLI error-code
policy; how existing progress work maps to the contract; watchdog thresholds
versus legitimate long provider requests; ownership/lock diagnostics; and any
minimum Windows host requirements. These must be resolved before UI/adapters
depend on the interface. Adding a daemon or network control API is out of scope.

## Implementation record (2026-10-26)

Implemented as one reviewable PR against `main`
([#37](https://github.com/billy-the-ape/repoise/pull/37)). Current-state guide:
[docs/automation.md](../../../docs/automation.md).

Settled decisions:

- Streaming mode: explicit `--events` flag on `index` and `watch` (the machine
  modes advertised by `capabilities`); existing `--json` output is untouched.
  Stdout carries NDJSON protocol records only; diagnostics stay on stderr.
- Handshake: new `repoise capabilities` command (human summary or `--json`
  document, schema version 1) advertising version, event-stream protocol and
  schema, build features, the command/machine-mode matrix, benchmark
  availability (`supported: false`; `bench` prints a stable pointer error,
  exit 1), the stable exit-code table and the additive compatibility policy.
- Event stream v1 (`schemas/event-stream.v1.schema.json`): envelope with
  `schemaVersion`, `runId`, zero-based strictly increasing `sequence`, UTC
  ISO-8601 `timestamp`, `type` and typed `payload`. Events: `started`
  (resolved scope), `progress`, `warning`, `heartbeat` (15 s default,
  `REPOISE_EVENTS_HEARTBEAT_MS` override floored at 10 ms), and exactly one
  terminal of `completed`/`cancelled`/`failed` on controlled exit. Missing
  terminal event = incomplete output (abrupt death). In-phase progress is
  throttled to at least 100 ms apart; phase start, completion and the final
  update always emit. ETAs stay phase-scoped and suppressed until reliable.
- Progress mapping: the existing human `ProgressReporter` and the new event
  reporter both implement the same `IndexProgressReporter` service interface;
  the human reporter is unchanged. Watch publications emit one `progress`
  event per generation, now including the new `configChange` field
  (effective-configuration-driven publication; false on the first build).
- Signals: `watch` uses `ctrlc` with the `termination` feature (workspace
  dependency now enables it). `SIGINT`/`SIGTERM`/`SIGHUP` (Unix) and
  `Ctrl+C` (Windows) cancel at the next step boundary, preserve the last
  complete generation, emit `cancelled` (`reason: "signal"`) and exit 0.
  `index` has no graceful handler (abrupt death leaves the last complete
  generation via transactional publication); both are documented.
- Ownership: advisory marker `owner.json` in the worktree scope directory
  (`repoise-core::ownership`), scoped by repo/worktree index identity, not a
  remote URL. Writers acquire (creating the scope directory if needed),
  refresh at most every 5 s and release only their exact record. Fresh
  foreign markers (under 10 min) warn on stderr and proceed; stale markers
  (10 min) are silently superseded. `status` reports the marker read-only
  (`owner` field with `active`/`stale` state); `purge` removes it with the
  scope. PID is never liveness proof; refresh freshness is the signal. No
  lock, no daemon, no blocking.
- MCP: unchanged — JSON-RPC only on stdout, no progress injection.

Validation: unit tests for the envelope contract (fields, sequences, single
terminal, heartbeat, throttled progress) and the ownership lifecycle
(acquire/inspect/release, stale detection, foreign-marker preservation);
integration tests for the capabilities handshake, `index --events` NDJSON
purity and terminal outcome, `watch --events` SIGINT cancellation, and the
owner marker appearing in `status` while watch runs and clearing after a
clean SIGTERM exit. Existing `--json`/MCP suites pass unchanged.

Deferred / not shipped with this PR:

- `repoise bench` and its exit-code partitioning (AP-04); capabilities only
  advertises the planned command.
- MCP progress through protocol mechanisms (negotiated capabilities).
- Windows live signal/cancellation verification (the Unix path is tested;
  Windows behavior follows `ctrlc` and is documented, not measured).
- Editor/adapter conformance against the contract (AP-03/AP-07).
- `index --events` graceful signal handling (abrupt-death semantics are
  documented instead).
