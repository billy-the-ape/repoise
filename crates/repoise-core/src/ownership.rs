//! Authoritative writer ownership for one mutable index scope (AP-02,
//! later hardened into an exclusive lock).
//!
//! Exactly one writer per mutable index scope: `index`, `watch` and the
//! opt-in MCP refresh all run through [`ScopeLock`], a cross-platform
//! OS-backed file lock (`flock(2)` on Unix, `LockFileEx` on Windows) on the
//! scope's `scope.lock` file. The lock is atomic across processes and is
//! released by the OS when the owning process dies (clean exit, crash or
//! kill), so it never depends on PID freshness or unsafe stale-file
//! deletion. A rejected writer receives a stable, actionable busy result
//! before any provider work or publication, and must not overwrite the
//! live owner's metadata.
//!
//! `owner.json` remains, but only as **descriptive heartbeat metadata**
//! for the process that holds the lock (pid, operation, start and last
//! refresh): marker publication stays atomic (sibling temp file + rename)
//! so concurrent readers only ever observe a complete record. `status`
//! surfaces the marker read-only via [`inspect`] together with the
//! non-mutating lock probe [`lock_held`], and [`writer_state`] lets
//! automation distinguish `owned`, `busy`, `stale-metadata` and `free`.
//! A PID is descriptive metadata only — it is never treated as proof of
//! liveness, and no process is ever killed or adopted based on it.
//! `purge` removes both files with the scope (it acquires the lock first).
//!
//! Supported filesystem assumptions: a local filesystem (or a
//! single-client share) where the OS lock semantics hold. `flock` is
//! advisory between cooperating Repoise processes; `LockFileEx` is a
//! mandatory byte-range lock on local volumes. Multi-client network
//! filesystems (e.g. NFS) are not supported for shared scope locking.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Marker file name inside one worktree scope directory (descriptive).
pub const OWNER_FILE: &str = "owner.json";
/// Lock file name for the authoritative scope ownership lock.
pub const SCOPE_LOCK_FILE: &str = "scope.lock";
/// A marker not refreshed within this window is reported as stale.
pub const STALE_AFTER: Duration = Duration::from_secs(600);
/// Minimum interval between marker refreshes.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Schema version of the owner marker.
const OWNER_SCHEMA_VERSION: u32 = 1;

/// The marker record.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnerRecord {
    schema_version: u32,
    pid: u32,
    operation: String,
    started_at_ms: u64,
    refreshed_at_ms: u64,
}

/// Read-only view of one scope's owner marker (for `status`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerReport {
    /// PID of the writing process (not proof of liveness).
    pub pid: u32,
    /// Writer operation name (`index` or `watch`).
    pub operation: String,
    /// Unix milliseconds when the writer started.
    pub started_at_ms: u64,
    /// Unix milliseconds of the last marker refresh.
    pub refreshed_at_ms: u64,
    /// Health state: `active` (refreshed within [`STALE_AFTER`]) or `stale`.
    pub state: String,
}

/// Failure to acquire (or use) the authoritative scope lock.
#[derive(Clone, Debug)]
pub enum OwnershipError {
    /// Another process currently holds the scope's OS lock; the live
    /// owner's descriptive metadata is attached when readable.
    Busy {
        /// The live owner's marker, when readable.
        metadata: Option<OwnerReport>,
    },
    /// The lock file could not be opened or locked: the scope cannot be
    /// guaranteed exclusive, so the operation must fail closed.
    Io(String),
}

impl std::fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwnershipError::Busy { metadata } => match metadata {
                Some(meta) => write!(
                    f,
                    "scope is owned by another Repoise writer (operation: {}, pid {}, \
                     last refreshed {} ms ago); this run made no changes; stop the other \
                     writer (or wait for it to finish) and retry, or run `repoise status` \
                     to inspect",
                    meta.operation,
                    meta.pid,
                    now_ms().saturating_sub(meta.refreshed_at_ms)
                ),
                None => write!(
                    f,
                    "scope is owned by another Repoise writer (owner metadata \
                     unavailable); this run made no changes; stop the owning writer and \
                     retry, or run `repoise status` to inspect"
                ),
            },
            OwnershipError::Io(err) => write!(
                f,
                "could not acquire the scope ownership lock: {err}; refusing to write \
                 the index without exclusive ownership"
            ),
        }
    }
}

impl std::error::Error for OwnershipError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OwnershipError::Busy { .. } | OwnershipError::Io(_) => None,
        }
    }
}

/// The authoritative one-writer lease on one mutable index scope.
///
/// The OS lock on [`SCOPE_LOCK_FILE`] is the exclusivity contract: held
/// for the whole operation (a `watch` holds it for the process lifetime),
/// released on drop — or by the OS itself when the process dies. The
/// descriptive `owner.json` heartbeat is refreshed on a schedule so
/// `status` and automation can tell a live owner from stale metadata.
/// The guard is not reentrant: a second acquisition by the same process
/// for the same scope is refused (one writer per scope).
pub struct ScopeLock {
    /// Holds the OS lock; closing the file (drop or process death) releases it.
    file: File,
    /// The descriptive marker path (inside the scope directory).
    path: PathBuf,
    /// Our acquisition time; part of the owner-token check on cleanup.
    started_at_ms: u64,
    last_refresh: Arc<Mutex<Instant>>,
    operation: String,
    /// Diagnostics for the caller (orphaned-marker replacement, degraded
    /// heartbeat writes).
    pub warnings: Vec<String>,
    /// The orphaned marker replaced at acquisition, if any.
    pub replaced_marker: Option<OwnerReport>,
}

impl std::fmt::Debug for ScopeLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeLock")
            .field("path", &self.path)
            .field("operation", &self.operation)
            .field("warnings", &self.warnings)
            .finish()
    }
}

/// Unix milliseconds since the epoch (0 when the clock is before it).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Marker path for one worktree scope directory.
pub fn marker_path(scope_dir: &Path) -> PathBuf {
    scope_dir.join(OWNER_FILE)
}

/// Atomically publishes `text` at `path`: write a sibling temp file
/// (`owner.json.tmp.<pid>`) in the same directory and rename it over the
/// target, so concurrent readers observe either the complete old record or
/// the complete new one — never a truncated or partial file.
fn write_marker_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!("{}.tmp.{}", OWNER_FILE, std::process::id()));
    let result = (|| -> std::io::Result<()> {
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

impl ScopeLock {
    /// Atomically acquires exclusive ownership of the scope.
    ///
    /// On success the lock is held and the descriptive marker (re)written
    /// with this process's record — any marker already present is an
    /// orphan from a writer that died (a live writer would hold the lock).
    /// On [`OwnershipError::Busy`] the caller must stop and surface the
    /// actionable message: no provider work or publication may proceed,
    /// and the live owner's marker is never touched.
    pub fn acquire(scope_dir: &Path, operation: &str) -> Result<Self, OwnershipError> {
        // The scope directory may not exist before the first publication;
        // create it so the lock (and marker) have a home.
        fs::create_dir_all(scope_dir).map_err(|err| OwnershipError::Io(err.to_string()))?;
        let path = scope_dir.join(SCOPE_LOCK_FILE);
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| OwnershipError::Io(err.to_string()))?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(err)
                if err.raw_os_error()
                    == Some(fs2::lock_contended_error().raw_os_error().unwrap_or(-1)) =>
            {
                // The scope lock is held by another live process. Never
                // touch its marker; attach it read-only for the report.
                return Err(OwnershipError::Busy {
                    metadata: inspect(scope_dir),
                });
            }
            Err(err) => return Err(OwnershipError::Io(err.to_string())),
        }

        // We now hold the authoritative lock: any existing marker is an
        // orphan (its holder is dead — otherwise we could not have
        // acquired). Descriptive only; safe to replace.
        let replaced_marker = inspect(scope_dir)
            .filter(|old| old.pid != std::process::id() || old.operation != operation);
        let started = now_ms();
        let record = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: std::process::id(),
            operation: operation.to_string(),
            started_at_ms: started,
            refreshed_at_ms: started,
        };
        let mut guard = Self {
            file,
            path: marker_path(scope_dir),
            started_at_ms: started,
            last_refresh: Arc::new(Mutex::new(Instant::now())),
            operation: operation.to_string(),
            warnings: Vec::new(),
            replaced_marker: replaced_marker.clone(),
        };
        if write_marker_atomic(
            &guard.path,
            &serde_json::to_string(&record).unwrap_or_default(),
        )
        .is_err()
        {
            // The lock stays authoritative; only the descriptive metadata
            // is degraded for observers.
            guard.warnings.push(
                "could not write owner marker: ownership detection is degraded for this run"
                    .to_string(),
            );
        }
        if let Some(old) = &replaced_marker {
            guard.warnings.push(format!(
                "replaced orphaned owner marker (pid {}, operation {}, last refreshed {} ms ago): the previous writer is no longer holding the scope lock",
                old.pid,
                old.operation,
                now_ms().saturating_sub(old.refreshed_at_ms)
            ));
        }
        Ok(guard)
    }

    /// Refreshes the descriptive heartbeat marker (throttled to
    /// [`REFRESH_INTERVAL`]). The OS lock itself needs no refresh; this
    /// only keeps `status` metadata current while the writer is idle.
    pub fn refresh(&self) {
        let mut last = match self.last_refresh.lock() {
            Ok(last) => last,
            Err(poisoned) => poisoned.into_inner(),
        };
        if last.elapsed() < REFRESH_INTERVAL {
            return;
        }
        let record = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: std::process::id(),
            operation: self.operation.clone(),
            started_at_ms: self.started_at_ms,
            refreshed_at_ms: now_ms(),
        };
        if write_marker_atomic(
            &self.path,
            &serde_json::to_string(&record).unwrap_or_default(),
        )
        .is_ok()
        {
            *last = Instant::now();
        }
    }
}

impl Drop for ScopeLock {
    fn drop(&mut self) {
        // Release the OS lock; if the process dies instead, the OS
        // releases it (the lock lives in the kernel, not in a file).
        let _ = self.file.unlock();
        // Owner-token-safe cleanup: remove the descriptive marker only
        // while it still carries this process's exact record. A foreign
        // marker written in the interval (normally impossible — only the
        // lock holder writes it) is never deleted.
        if let Some(record) = inspect(self.path.parent().unwrap_or(Path::new("")))
            && record.pid == std::process::id()
            && record.started_at_ms == self.started_at_ms
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Non-mutating probe: is the scope's OS lock currently held by some
/// process? Creates nothing (an absent lock file means `false`).
///
/// The probe briefly takes and releases the lock to observe it: that is
/// safe because it can only succeed while no writer holds the lease, so
/// the probe never changes the scope's ownership state in any observable
/// way.
pub fn lock_held(scope_dir: &Path) -> bool {
    let path = scope_dir.join(SCOPE_LOCK_FILE);
    let Ok(file) = File::options().read(true).write(true).open(&path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Derives the machine state for `status`/automation from the lock probe
/// and the descriptive marker:
///
/// - `owned`: the lock is held and the marker is active — a live writer
///   with current metadata;
/// - `busy`: the lock is held but the marker is missing or stale — a
///   writer is certainly present (new writers get the busy result) but
///   its health/identity is unknown;
/// - `stale-metadata`: the lock is not held and a marker is present — the
///   scope is free; the marker is descriptive residue (e.g. a crash);
/// - `free`: the lock is not held and no marker is present.
pub fn writer_state(owner: Option<&OwnerReport>, lock_held: bool) -> &'static str {
    if lock_held {
        if owner.is_some_and(|o| o.state == "active") {
            "owned"
        } else {
            "busy"
        }
    } else if owner.is_some() {
        "stale-metadata"
    } else {
        "free"
    }
}

/// Machine-readable failure reason for the `--events` terminal `failed`
/// payload when a writer is refused because another writer owns the
/// scope (retryable once the owner releases).
pub const REASON_SCOPE_OWNERD: &str = "scope-owned";
/// Machine-readable failure reason when the lock file itself is unusable
/// (not retryable by automation).
pub const REASON_LOCK_UNAVAILABLE: &str = "lock-unavailable";

/// Maps an acquisition failure to its machine reason and retryability.
pub fn failure_reason(err: &OwnershipError) -> (String, Option<bool>) {
    match err {
        OwnershipError::Busy { .. } => (REASON_SCOPE_OWNERD.to_string(), Some(true)),
        OwnershipError::Io(_) => (REASON_LOCK_UNAVAILABLE.to_string(), None),
    }
}

impl From<OwnershipError> for Error {
    fn from(err: OwnershipError) -> Self {
        Error::Owned(err.to_string())
    }
}

impl From<OwnershipError> for String {
    fn from(err: OwnershipError) -> Self {
        err.to_string()
    }
}

/// Read-only inspection for `status` (never creates the file or directory).
pub fn inspect(scope_dir: &Path) -> Option<OwnerReport> {
    let text = fs::read_to_string(marker_path(scope_dir)).ok()?;
    let record: OwnerRecord = serde_json::from_str(&text).ok()?;
    if record.schema_version != OWNER_SCHEMA_VERSION {
        return None;
    }
    let age_ms = now_ms().saturating_sub(record.refreshed_at_ms);
    Some(OwnerReport {
        pid: record.pid,
        operation: record.operation,
        started_at_ms: record.started_at_ms,
        refreshed_at_ms: record.refreshed_at_ms,
        state: if age_ms < STALE_AFTER.as_millis() as u64 {
            "active"
        } else {
            "stale"
        }
        .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "repoise-ownership-{}-{}-{name}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn foreign_record(pid: u32, operation: &str, refreshed_age_ms: u64) -> OwnerRecord {
        let now = now_ms();
        OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid,
            operation: operation.to_string(),
            started_at_ms: now.saturating_sub(60_000),
            refreshed_at_ms: now.saturating_sub(refreshed_age_ms),
        }
    }

    #[test]
    fn acquire_writes_marker_and_probe_reports_held() {
        let dir = scratch("lifecycle");
        let guard = ScopeLock::acquire(&dir, "index").expect("acquire");
        assert!(
            guard.warnings.is_empty(),
            "first acquire has no warnings: {:?}",
            guard.warnings
        );
        assert!(lock_held(&dir), "the OS lock is held");

        let report = inspect(&dir).expect("marker is visible to inspect");
        assert_eq!(report.operation, "index");
        assert_eq!(report.state, "active");
        assert_eq!(report.pid, std::process::id());
        assert_eq!(writer_state(Some(&report), true), "owned");

        drop(guard);
        assert!(!lock_held(&dir), "the lock releases on drop");
        assert!(inspect(&dir).is_none(), "owner drop removes its marker");
    }

    #[test]
    fn second_open_for_same_scope_is_denied() {
        // Two independent opens of the same scope (the in-process stand-in
        // for two processes): exactly one acquires.
        let dir = scratch("deny");
        let first = ScopeLock::acquire(&dir, "watch").expect("first acquires");
        match ScopeLock::acquire(&dir, "index") {
            Err(OwnershipError::Busy { metadata }) => {
                assert!(
                    metadata.is_some_and(|meta| meta.pid == std::process::id()),
                    "the live owner's metadata is attached"
                );
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        // The rejected writer must not have touched the owner's marker.
        let report = inspect(&dir).expect("marker untouched");
        assert_eq!(report.operation, "watch");
        drop(first);
        assert!(
            ScopeLock::acquire(&dir, "index").is_ok(),
            "the released scope is acquirable again"
        );
    }

    #[test]
    fn fresh_marker_with_live_pid_never_blocks() {
        // PID reuse / stale-metadata guard: a fresh marker that even names
        // a live pid must not block — only the OS lock decides.
        let dir = scratch("pid-reuse");
        let foreign = foreign_record(std::process::id().wrapping_add(1), "watch", 0);
        fs::write(marker_path(&dir), serde_json::to_string(&foreign).unwrap()).unwrap();
        let guard = ScopeLock::acquire(&dir, "index").expect("a marker alone must never block");
        assert!(
            matches!(
                guard.replaced_marker.as_ref(),
                Some(old) if old.operation == "watch"
            ),
            "the orphan is reported as replaced"
        );
        let report = inspect(&dir).expect("marker rewritten");
        assert_eq!(report.operation, "index");
        drop(guard);
    }

    #[test]
    fn stale_marker_is_replaced_and_reported() {
        let dir = scratch("stale");
        let stale = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: 4242,
            operation: "watch".to_string(),
            started_at_ms: now_ms() - 720_000,
            refreshed_at_ms: now_ms() - 720_000,
        };
        fs::write(marker_path(&dir), serde_json::to_string(&stale).unwrap()).unwrap();
        let guard = ScopeLock::acquire(&dir, "index").expect("stale marker must not block");
        assert!(
            guard
                .warnings
                .iter()
                .any(|w| w.contains("orphaned owner marker")),
            "the orphaned marker is reported: {:?}",
            guard.warnings
        );
        let report = inspect(&dir).expect("new marker is visible");
        assert_eq!(report.pid, std::process::id());
        drop(guard);
        assert!(inspect(&dir).is_none());
    }

    #[test]
    fn corrupt_or_truncated_metadata_never_block() {
        let dir = scratch("garbage");
        fs::write(marker_path(&dir), "not json").unwrap();
        let guard = ScopeLock::acquire(&dir, "index").expect("corrupt marker must not block");
        assert!(
            guard.replaced_marker.is_none(),
            "unreadable metadata is not reported as replaced"
        );
        assert!(inspect(&dir).is_some_and(|o| o.operation == "index"));
        drop(guard);

        // A truncated marker (the failure mode of a non-atomic write).
        let dir = scratch("truncated");
        let full = foreign_record(9999, "watch", 0);
        let full_text = serde_json::to_string(&full).unwrap();
        fs::write(marker_path(&dir), &full_text[..full_text.len() / 2]).unwrap();
        let guard = ScopeLock::acquire(&dir, "index").expect("truncated marker must not block");
        assert!(inspect(&dir).is_some_and(|o| o.operation == "index"));
        drop(guard);
    }

    #[test]
    fn cleanup_is_owner_token_safe() {
        let dir = scratch("token");
        let guard = ScopeLock::acquire(&dir, "index").expect("acquire");
        // A foreign marker written while we hold the lock (normally
        // impossible — only the lock holder writes it) must survive our
        // drop.
        let foreign = foreign_record(999_999, "watch", 0);
        fs::write(marker_path(&dir), serde_json::to_string(&foreign).unwrap()).unwrap();
        drop(guard);
        let remaining = inspect(&dir).expect("foreign marker kept");
        assert_eq!(remaining.pid, 999_999);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_state_matrix() {
        let active = OwnerReport {
            pid: 1,
            operation: "watch".into(),
            started_at_ms: 0,
            refreshed_at_ms: 0,
            state: "active".into(),
        };
        let stale = OwnerReport {
            state: "stale".into(),
            ..active.clone()
        };
        assert_eq!(writer_state(Some(&active), true), "owned");
        assert_eq!(writer_state(None, true), "busy");
        assert_eq!(writer_state(Some(&stale), true), "busy");
        assert_eq!(writer_state(Some(&active), false), "stale-metadata");
        assert_eq!(writer_state(None, false), "free");
    }

    #[test]
    fn probe_never_creates_the_lock_file() {
        let dir = scratch("no-create");
        assert!(!lock_held(&dir));
        assert!(
            !dir.join(SCOPE_LOCK_FILE).exists(),
            "the probe creates nothing"
        );
    }

    #[test]
    fn distinct_scopes_do_not_block_each_other() {
        let a = scratch("scope-a");
        let b = scratch("scope-b");
        let lock_a = ScopeLock::acquire(&a, "watch").expect("scope A acquires");
        let lock_b = ScopeLock::acquire(&b, "watch").expect("scope B acquires concurrently");
        drop(lock_a);
        drop(lock_b);
    }

    #[test]
    fn refresh_is_throttled_and_updates_health() {
        let dir = scratch("refresh");
        let guard = ScopeLock::acquire(&dir, "watch").expect("acquire");
        let first = inspect(&dir).unwrap().refreshed_at_ms;
        guard.refresh(); // throttled: within REFRESH_INTERVAL of acquire
        let second = inspect(&dir).unwrap().refreshed_at_ms;
        assert_eq!(
            first, second,
            "refresh within the throttle window is a no-op"
        );
        *guard.last_refresh.lock().unwrap() =
            Instant::now() - REFRESH_INTERVAL - Duration::from_millis(1);
        guard.refresh();
        let third = inspect(&dir).unwrap().refreshed_at_ms;
        assert!(
            third >= second,
            "refresh after the window updates the marker"
        );
    }

    /// Marker publication is atomic: a reader racing with concurrent rewrites
    /// must only ever observe a complete record (never a truncated one).
    #[test]
    fn marker_writes_are_atomic_under_concurrent_reads() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = scratch("atomic");
        let path = marker_path(&dir);
        // Seed a valid marker so readers never hit a missing file.
        let seed = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: 1,
            operation: "index".to_string(),
            started_at_ms: now_ms(),
            refreshed_at_ms: now_ms(),
        };
        write_marker_atomic(&path, &serde_json::to_string(&seed).unwrap()).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = stop.clone();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let mut i = 0u64;
            while !writer_stop.load(Ordering::Relaxed) {
                let record = OwnerRecord {
                    schema_version: OWNER_SCHEMA_VERSION,
                    pid: 2 + (i % 3) as u32,
                    operation: "watch".to_string(),
                    started_at_ms: now_ms(),
                    refreshed_at_ms: now_ms() + i,
                };
                write_marker_atomic(&writer_path, &serde_json::to_string(&record).unwrap())
                    .unwrap();
                i += 1;
            }
        });
        let mut complete_reads = 0u32;
        while complete_reads < 500 {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    let record: OwnerRecord = serde_json::from_str(&text).unwrap_or_else(|err| {
                        panic!("reader observed a partial marker: {err}; text: {text:?}")
                    });
                    assert_eq!(record.schema_version, OWNER_SCHEMA_VERSION);
                    complete_reads += 1;
                }
                // A transient sharing failure while the marker is being
                // replaced is retryable; any completed read must be a full
                // record.
                Err(_) => std::thread::yield_now(),
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = writer.join();
    }
}
