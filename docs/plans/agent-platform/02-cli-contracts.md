# AP-02: CLI automation and lifecycle contracts

Status: proposed. Depends on [AP-01](01-workspace-and-ci.md).
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
