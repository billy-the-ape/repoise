//! NDJSON event-stream protocol for automation (AP-02).
//!
//! In `--events` mode the machine stdout carries exactly one JSON event
//! envelope per line (newline-delimited JSON); all diagnostics go to stderr.
//! Every envelope carries the schema version, the run id, a monotonically
//! increasing sequence number, a UTC RFC 3339 timestamp, the event type and
//! a typed payload.
//!
//! Run lifecycle: the stream opens with a `started` event carrying the
//! resolved scope and ends with exactly one terminal event (`completed` for
//! a finished `index`, `cancelled` for a signal-stopped `watch`, or
//! `failed` for a run that errored). After a terminal event the stream
//! refuses further events, so an abrupt process death is recognizable as a
//! stream that ends without its terminal event.
//!
//! Heartbeats make a healthy process that is waiting on a slow provider
//! distinguishable from a stopped one; the interval defaults to 15 s and can
//! be tuned with [`HEARTBEAT_ENV_VAR`] for watchdogs. Event frequency is
//! bounded (per-phase progress is throttled), and writes to a slow reader
//! apply ordinary pipe backpressure.
//!
//! The checked-in contract is [`EVENT_STREAM_SCHEMA_FILE`]; see
//! `docs/automation.md` for the compatibility policy.
//!
//! Compatibility policy: additive. Consumers must ignore unknown event
//! types and unknown fields, and must reject a stream whose
//! `schemaVersion` they do not support; breaking changes bump the version.

use std::io::{Stdout, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use repoise_core::progress::{IndexPhase, IndexProgressReporter};
use serde::Serialize;
use serde_json::{Value, json};

use crate::progress::eta_seconds_from;

/// Version of the event-stream protocol (bumped on breaking changes).
pub const EVENT_SCHEMA_VERSION: u32 = 1;
/// Checked-in schema file for the event-stream protocol.
pub const EVENT_STREAM_SCHEMA_FILE: &str = "schemas/event-stream.v1.schema.json";
/// Envelope fields every event must carry (checked against the schema).
pub const ENVELOPE_REQUIRED_FIELDS: &[&str] = &[
    "schemaVersion",
    "runId",
    "sequence",
    "timestamp",
    "type",
    "payload",
];
/// All event types this producer emits.
pub const EVENT_TYPES: &[&str] = &[
    "started",
    "progress",
    "warning",
    "heartbeat",
    "completed",
    "cancelled",
    "failed",
];
/// Terminal event types (exactly one per run).
pub const TERMINAL_EVENT_TYPES: &[&str] = &["completed", "cancelled", "failed"];
/// Default heartbeat interval (milliseconds).
pub const DEFAULT_HEARTBEAT_MS: u64 = 15000;
/// Floor on the heartbeat interval override (milliseconds).
pub const MIN_HEARTBEAT_MS: u64 = 10;
/// Environment override for the heartbeat interval (milliseconds).
pub const HEARTBEAT_ENV_VAR: &str = "REPOISE_EVENTS_HEARTBEAT_MS";
/// Minimum interval between in-phase progress events (amplification bound).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Resolved operation scope, included in start and final records so a
/// caller can reject a mismatch before relying on the output.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    /// Canonical repository root.
    pub root: String,
    /// Stable repository scope id.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Snapshot mode: `working-tree`, `committed` or `plain-directory`.
    pub snapshot_mode: String,
}

/// Stable kebab-case snapshot-mode name for scope records.
pub fn scope_mode_name(mode: repoise_core::adapter::SnapshotMode) -> &'static str {
    match mode {
        repoise_core::adapter::SnapshotMode::WorkingTree => "working-tree",
        repoise_core::adapter::SnapshotMode::Committed => "committed",
        repoise_core::adapter::SnapshotMode::PlainDirectory => "plain-directory",
    }
}

/// Resolves the heartbeat interval: the environment override (floored at
/// [`MIN_HEARTBEAT_MS`]) or the default.
pub fn heartbeat_interval() -> Duration {
    match std::env::var(HEARTBEAT_ENV_VAR) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms.max(MIN_HEARTBEAT_MS)),
            Err(_) => {
                eprintln!(
                    "note: invalid {HEARTBEAT_ENV_VAR}={value:?}; using the default {DEFAULT_HEARTBEAT_MS} ms"
                );
                Duration::from_millis(DEFAULT_HEARTBEAT_MS)
            }
        },
        Err(_) => Duration::from_millis(DEFAULT_HEARTBEAT_MS),
    }
}

/// UTC RFC 3339 timestamp with millisecond precision.
pub fn utc_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let ms = now.subsec_millis();
    let (year, month, day) = civil_from_days(secs / 86_400);
    let day_secs = (secs % 86_400) as u64;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        day_secs / 3600,
        (day_secs / 60) % 60,
        day_secs % 60
    )
}

/// Converts days since the civil epoch to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// One event envelope (the JSON object on each stdout line).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    schema_version: u32,
    run_id: String,
    sequence: u64,
    timestamp: String,
    #[serde(rename = "type")]
    ty: String,
    payload: Value,
}

struct Inner<W: Write> {
    writer: Mutex<W>,
    sequence: AtomicU64,
    terminal: AtomicBool,
    start: Instant,
    run_id: String,
    heartbeat_stop: Mutex<Option<Arc<AtomicBool>>>,
}

/// The NDJSON event stream for one run. Clones share the same underlying
/// stream; `Drop` stops the heartbeat thread.
pub struct EventStream<W: Write + Send + 'static> {
    inner: Arc<Inner<W>>,
    operation: &'static str,
}

impl<W: Write + Send + 'static> Clone for EventStream<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            operation: self.operation,
        }
    }
}

impl<W: Write + Send + 'static> Drop for EventStream<W> {
    fn drop(&mut self) {
        if let Some(stop) = self.inner.heartbeat_stop.lock().unwrap().take() {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

impl<W: Write + Send + 'static> EventStream<W> {
    /// Creates a stream over `writer` for `operation` (e.g. `"index"`).
    pub fn new(writer: W, operation: &'static str) -> Self {
        let run_id = format!(
            "{operation}-{}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(writer),
                sequence: AtomicU64::new(0),
                terminal: AtomicBool::new(false),
                start: Instant::now(),
                run_id,
                heartbeat_stop: Mutex::new(None),
            }),
            operation,
        }
    }

    /// The run id for this stream (`<operation>-<pid>-<start nanos hex>`).
    pub fn run_id(&self) -> &str {
        &self.inner.run_id
    }

    /// Milliseconds elapsed since the stream was created.
    pub fn elapsed_ms(&self) -> u64 {
        self.inner.start.elapsed().as_millis() as u64
    }

    /// Emits the run's opening `started` event with the resolved scope.
    pub fn start(&self, scope: &Scope) {
        self.emit(
            "started",
            json!({
                "operation": self.operation,
                "scope": scope,
                "pid": std::process::id(),
            }),
        );
    }

    /// Emits a non-terminal event; returns `false` once the run terminated
    /// (the stream then refuses further events).
    pub fn emit(&self, ty: &str, payload: Value) -> bool {
        debug_assert!(EVENT_TYPES.contains(&ty));
        debug_assert!(!TERMINAL_EVENT_TYPES.contains(&ty));
        self.write_event(ty, payload, false)
    }

    /// Emits the run's single terminal event (`completed`, `cancelled` or
    /// `failed`). Subsequent emits (including a second terminal) are refused.
    pub fn terminal(&self, ty: &str, payload: Value) {
        debug_assert!(TERMINAL_EVENT_TYPES.contains(&ty));
        self.write_event(ty, payload, true);
    }

    /// Starts the heartbeat thread at `interval`; dropping the stream stops
    /// it. Heartbeats are `heartbeat` events (liveness, no progress claim).
    pub fn start_heartbeat(&self, interval: Duration) {
        let stop = Arc::new(AtomicBool::new(false));
        *self.inner.heartbeat_stop.lock().unwrap() = Some(stop.clone());
        let stream = self.clone();
        std::thread::spawn(move || {
            let tick = Duration::from_millis(25);
            let mut next = Instant::now() + interval;
            while !stop.load(Ordering::SeqCst) {
                let now = Instant::now();
                if now >= next {
                    stream.emit(
                        "heartbeat",
                        json!({
                            "operation": stream.operation,
                            "elapsedMs": stream.elapsed_ms(),
                        }),
                    );
                    next = Instant::now() + interval;
                }
                std::thread::sleep(tick);
            }
        });
    }

    fn write_event(&self, ty: &str, payload: Value, is_terminal: bool) -> bool {
        let mut writer = self.inner.writer.lock().unwrap();
        if self.inner.terminal.load(Ordering::SeqCst) {
            return false;
        }
        let claimed = if is_terminal {
            self.inner
                .terminal
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        } else {
            true
        };
        if !claimed {
            return false;
        }
        let sequence = self.inner.sequence.fetch_add(1, Ordering::SeqCst);
        let envelope = Envelope {
            schema_version: EVENT_SCHEMA_VERSION,
            run_id: self.inner.run_id.clone(),
            sequence,
            timestamp: utc_now(),
            ty: ty.to_string(),
            payload,
        };
        let mut line = match serde_json::to_string(&envelope) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("repoise events: cannot serialize event: {err}");
                return false;
            }
        };
        line.push('\n');
        // Producer-side invariant: the line carries every envelope field.
        debug_assert!(
            ENVELOPE_REQUIRED_FIELDS
                .iter()
                .all(|field| line.contains(&format!("\"{field}\":"))),
            "envelope is missing required fields: {line}"
        );
        if let Err(err) = writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush())
        {
            eprintln!("repoise events: stdout write failed: {err}; the stream is incomplete");
            // A broken stdout cannot deliver the terminal event; mark the run
            // terminated so no further (failing) writes are attempted.
            self.inner.terminal.store(true, Ordering::SeqCst);
        }
        true
    }
}

impl EventStream<Stdout> {
    /// Creates the stream for the process stdout.
    pub fn stdout(operation: &'static str) -> Self {
        Self::new(std::io::stdout(), operation)
    }
}

/// Unit name for one phase's progress counts (mirrors the human reporter).
fn unit_for(phase: IndexPhase) -> &'static str {
    match phase {
        IndexPhase::Discovery => "candidates",
        IndexPhase::Reconcile => "files",
        IndexPhase::Embedding => "chunks",
        IndexPhase::History => "items",
        IndexPhase::Publication => "steps",
    }
}

struct PhaseState {
    done: u64,
    total: Option<u64>,
    last_emit: Instant,
    eta_base: Option<(u64, Instant)>,
    completion_emitted: bool,
}

/// Index progress reporter that emits `progress` events to the stream.
/// In-phase updates are throttled to [`PROGRESS_INTERVAL`]; phase start,
/// phase completion and the final update of a phase always emit, so a run
/// never looks stuck.
pub struct EventProgressReporter<W: Write + Send + 'static> {
    stream: EventStream<W>,
    state: Mutex<PhaseState>,
}

impl<W: Write + Send + 'static> EventProgressReporter<W> {
    /// Creates the reporter for the stream's operation.
    pub fn new(stream: EventStream<W>) -> Self {
        Self {
            stream,
            state: Mutex::new(PhaseState {
                done: 0,
                total: None,
                last_emit: Instant::now(),
                eta_base: None,
                completion_emitted: false,
            }),
        }
    }

    fn progress_payload(
        &self,
        phase: IndexPhase,
        state: &PhaseState,
        done: u64,
        total: Option<u64>,
    ) -> Value {
        let eta = state.eta_base.and_then(|(base_done, base_time)| {
            total.and_then(|total| {
                eta_seconds_from(done, total, Some(base_done), base_time.elapsed())
            })
        });
        json!({
            "operation": self.stream.operation,
            "phase": phase.name(),
            "unit": unit_for(phase),
            "done": done,
            "total": total,
            "elapsedMs": self.stream.elapsed_ms(),
            "etaSeconds": eta,
        })
    }
}

impl<W: Write + Send + 'static> IndexProgressReporter for EventProgressReporter<W> {
    fn phase_started(&self, phase: IndexPhase, total: Option<u64>) {
        let mut state = self.state.lock().unwrap();
        state.done = 0;
        state.total = total;
        state.eta_base = None;
        state.completion_emitted = false;
        let payload = self.progress_payload(phase, &state, 0, total);
        state.last_emit = Instant::now();
        drop(state);
        self.stream.emit("progress", payload);
    }

    fn progress(&self, phase: IndexPhase, done: u64, total: u64) {
        let payload = {
            let mut state = self.state.lock().unwrap();
            state.done = done;
            state.total = Some(total);
            if done == total && state.completion_emitted {
                return;
            }
            if state.eta_base.is_none() {
                state.eta_base = Some((done, Instant::now()));
            }
            let due = done == total
                || Instant::now().duration_since(state.last_emit) >= PROGRESS_INTERVAL;
            if !due {
                return;
            }
            state.last_emit = Instant::now();
            state.completion_emitted = done == total;
            self.progress_payload(phase, &state, done, Some(total))
        };
        self.stream.emit("progress", payload);
    }

    fn phase_finished(&self, phase: IndexPhase) {
        let payload = {
            let mut state = self.state.lock().unwrap();
            if state.completion_emitted {
                return;
            }
            let done = state.done;
            let total = state.total;
            state.completion_emitted = total.map(|total| done == total).unwrap_or(true);
            self.progress_payload(phase, &state, done, total)
        };
        self.stream.emit("progress", payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared in-memory sink so tests can inspect the NDJSON bytes.
    #[derive(Clone)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sink_stream() -> (EventStream<SharedSink>, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let stream = EventStream::new(SharedSink(buffer.clone()), "test");
        (stream, buffer)
    }

    fn lines(buffer: &Mutex<Vec<u8>>) -> Vec<Value> {
        let bytes = buffer.lock().unwrap().clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn envelope_carries_required_fields() {
        let (stream, buffer) = sink_stream();
        stream.start(&Scope {
            root: "/tmp/repo".into(),
            repo_id: "repo".into(),
            worktree_id: "wt".into(),
            snapshot_mode: "working-tree".into(),
        });
        let events = lines(&buffer);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        for field in ENVELOPE_REQUIRED_FIELDS {
            assert!(event.get(*field).is_some(), "missing {field}: {event}");
        }
        assert_eq!(event["schemaVersion"], EVENT_SCHEMA_VERSION);
        assert_eq!(event["type"], "started");
        assert_eq!(event["sequence"], 0);
        assert_eq!(event["runId"].as_str().unwrap(), stream.run_id());
        assert!(event["runId"].as_str().unwrap().starts_with("test-"));
        assert!(event["timestamp"].as_str().unwrap().ends_with('Z'));
        assert_eq!(event["payload"]["scope"]["repoId"], "repo");
        assert_eq!(event["payload"]["scope"]["worktreeId"], "wt");
        assert_eq!(event["payload"]["scope"]["snapshotMode"], "working-tree");
        assert_eq!(event["payload"]["pid"], std::process::id() as u64);
    }

    #[test]
    fn sequences_increase_monotonically() {
        let (stream, buffer) = sink_stream();
        stream.start(&Scope {
            root: "/tmp/repo".into(),
            repo_id: "repo".into(),
            worktree_id: "wt".into(),
            snapshot_mode: "plain-directory".into(),
        });
        for _ in 0..5 {
            stream.emit("progress", json!({ "phase": "reconcile" }));
        }
        let events = lines(&buffer);
        let sequences: Vec<u64> = events
            .iter()
            .map(|event| event["sequence"].as_u64().unwrap())
            .collect();
        assert_eq!(sequences, (0..6).collect::<Vec<u64>>());
    }

    #[test]
    fn exactly_one_terminal_event() {
        let (stream, buffer) = sink_stream();
        stream.start(&Scope {
            root: "/tmp/repo".into(),
            repo_id: "repo".into(),
            worktree_id: "wt".into(),
            snapshot_mode: "committed".into(),
        });
        stream.terminal("completed", json!({ "operation": "test" }));
        // A non-terminal event after the terminal one is refused.
        assert!(!stream.emit("progress", json!({})));
        // A second terminal event is refused too.
        stream.terminal("failed", json!({ "code": "operational" }));
        let events = lines(&buffer);
        assert_eq!(
            events.len(),
            2,
            "no extra events after the terminal one: {events:?}"
        );
        assert_eq!(events[1]["type"], "completed");
    }

    #[test]
    fn heartbeat_emits_between_activity() {
        let (stream, buffer) = sink_stream();
        stream.start(&Scope {
            root: "/tmp/repo".into(),
            repo_id: "repo".into(),
            worktree_id: "wt".into(),
            snapshot_mode: "plain-directory".into(),
        });
        stream.start_heartbeat(Duration::from_millis(10));
        // Let several heartbeats land.
        std::thread::sleep(Duration::from_millis(80));
        drop(stream); // stops the heartbeat thread
        let events = lines(&buffer);
        let heartbeats: Vec<&Value> = events.iter().filter(|e| e["type"] == "heartbeat").collect();
        assert!(!heartbeats.is_empty(), "expected heartbeats: {events:?}");
        for heartbeat in heartbeats {
            assert_eq!(heartbeat["payload"]["operation"], "test");
            assert!(heartbeat["payload"]["elapsedMs"].is_number());
        }
    }

    #[test]
    fn timestamp_is_iso8601() {
        let ts = utc_now();
        // Shape: YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24, "{ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], ".");
        assert_eq!(&ts[23..24], "Z");
    }

    #[test]
    fn progress_reporter_emits_throttled_and_complete_events() {
        let (stream, buffer) = sink_stream();
        stream.start(&Scope {
            root: "/tmp/repo".into(),
            repo_id: "repo".into(),
            worktree_id: "wt".into(),
            snapshot_mode: "plain-directory".into(),
        });
        let reporter = EventProgressReporter::new(stream);
        reporter.phase_started(IndexPhase::Reconcile, Some(4));
        // Fast updates below the throttle interval are coalesced; the final
        // update always emits.
        for done in 1..=4 {
            reporter.progress(IndexPhase::Reconcile, done, 4);
        }
        reporter.phase_finished(IndexPhase::Reconcile);
        let events = lines(&buffer);
        let progress: Vec<&Value> = events.iter().filter(|e| e["type"] == "progress").collect();
        // At least the start and the completion.
        assert!(
            progress.len() >= 2,
            "expected progress events: {progress:?}"
        );
        let first = &progress.first().unwrap()["payload"];
        let last = &progress.last().unwrap()["payload"];
        assert_eq!(first["phase"], "reconcile");
        assert_eq!(first["done"], 0);
        assert_eq!(first["total"], 4);
        assert_eq!(last["phase"], "reconcile");
        assert_eq!(last["done"], 4);
        assert_eq!(last["total"], 4);
        assert_eq!(last["unit"], "files");
    }
}
