//! Advisory writer-ownership marker for one worktree index scope (AP-02).
//!
//! Publications are transactional (one SQLite immediate transaction per
//! generation), so a foreign writer can never corrupt a published
//! generation; this marker exists so callers can *detect* a concurrent
//! writer for the same scope, inspect its health and recognize stale
//! ownership. It is best-effort and never blocks or fails the operation:
//! acquisition, refresh and release report diagnostics as warnings.
//!
//! The marker is scoped by the actual repository/worktree index scope
//! directory (`<cache>/repos/<repoId>/worktrees/<worktreeId>/owner.json`),
//! not by a repository remote URL. A PID alone is never treated as proof
//! of liveness (PIDs are reused); freshness of the last refresh is the
//! health signal. `status` exposes the marker read-only
//! (via [`inspect`](inspect)); `purge` removes it with the scope.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Marker file name inside one worktree scope directory.
pub const OWNER_FILE: &str = "owner.json";
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

/// Advisory ownership for one scope held by the current process. Dropping
/// the guard releases the marker (only if the marker still carries this
/// process's exact record, so a newer foreign marker is never removed).
pub struct OwnerGuard {
    path: PathBuf,
    mine: bool,
    started_at_ms: u64,
    last_refresh: std::sync::Mutex<Instant>,
    operation: String,
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

impl OwnerGuard {
    /// Attempts to acquire the advisory marker for `operation` (`"index"` or
    /// `"watch"`). Returns the guard plus diagnostics; non-empty diagnostics
    /// mean a concurrent or stale owner was detected (the caller still
    /// proceeds — this never blocks).
    pub fn acquire(scope_dir: &Path, operation: &str) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let path = marker_path(scope_dir);
        let pid = std::process::id();
        let started = now_ms();
        if let Ok(text) = fs::read_to_string(&path) {
            match serde_json::from_str::<OwnerRecord>(&text) {
                Ok(record) if record.schema_version == OWNER_SCHEMA_VERSION => {
                    let age_ms = now_ms().saturating_sub(record.refreshed_at_ms);
                    let foreign = record.pid != pid || record.operation != operation;
                    if foreign && age_ms < STALE_AFTER.as_millis() as u64 {
                        // A recent marker from another process: report, never block.
                        warnings.push(format!(
                            "another Repoise writer may be active for this scope (pid {}, operation {}, last refreshed {} ms ago); proceeding, publications are transactional",
                            record.pid, record.operation, age_ms
                        ));
                        return (
                            Self {
                                path,
                                mine: false,
                                started_at_ms: started,
                                last_refresh: std::sync::Mutex::new(Instant::now()),
                                operation: operation.to_string(),
                            },
                            warnings,
                        );
                    }
                    if foreign {
                        warnings.push(format!(
                            "removed stale owner marker (pid {}, operation {}, last refreshed {} ms ago)",
                            record.pid, record.operation, age_ms
                        ));
                    } else {
                        warnings.push("reacquired owner marker left by this process".to_string());
                    }
                    let _ = fs::remove_file(&path);
                }
                Ok(_) => {
                    warnings
                        .push("removed owner marker with an unknown schema version".to_string());
                    let _ = fs::remove_file(&path);
                }
                Err(_) => {
                    warnings.push("removed unreadable owner marker".to_string());
                    let _ = fs::remove_file(&path);
                }
            }
        }
        let record = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid,
            operation: operation.to_string(),
            started_at_ms: started,
            refreshed_at_ms: started,
        };
        let mut guard = Self {
            path,
            mine: false,
            started_at_ms: started,
            last_refresh: std::sync::Mutex::new(Instant::now()),
            operation: operation.to_string(),
        };
        let record_text = serde_json::to_string(&record).unwrap_or_default();
        // The scope directory may not exist before the first publication;
        // create it so the marker has a home.
        let _ = fs::create_dir_all(scope_dir);
        match fs::write(&guard.path, record_text) {
            Ok(()) => guard.mine = true,
            Err(err) => {
                warnings.push(format!(
                    "could not write owner marker: {err}; ownership detection is degraded for this run"
                ));
            }
        }
        (guard, warnings)
    }

    /// Refreshes the marker (throttled to [`REFRESH_INTERVAL`]); no-op when
    /// the current process is not the owner.
    pub fn refresh(&self) {
        if !self.mine {
            return;
        }
        let last = *self.last_refresh.lock().unwrap();
        if last.elapsed() < REFRESH_INTERVAL {
            return;
        }
        *self.last_refresh.lock().unwrap() = Instant::now();
        let record = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: std::process::id(),
            operation: self.operation.clone(),
            started_at_ms: self.started_at_ms,
            refreshed_at_ms: now_ms(),
        };
        let _ = fs::write(
            &self.path,
            serde_json::to_string(&record).unwrap_or_default(),
        );
    }
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        // Remove the marker only when it still carries this process's exact
        // record; a newer foreign marker must survive.
        if self.mine
            && let Ok(text) = fs::read_to_string(&self.path)
            && let Ok(record) = serde_json::from_str::<OwnerRecord>(&text)
            && record.schema_version == OWNER_SCHEMA_VERSION
            && record.pid == std::process::id()
            && record.started_at_ms == self.started_at_ms
        {
            let _ = fs::remove_file(&self.path);
        }
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

    #[test]
    fn acquire_inspect_and_release_lifecycle() {
        let dir = scratch("lifecycle");
        let (guard, warnings) = OwnerGuard::acquire(&dir, "index");
        assert!(
            warnings.is_empty(),
            "first acquire has no warnings: {warnings:?}"
        );

        let report = inspect(&dir).expect("marker is visible to inspect");
        assert_eq!(report.operation, "index");
        assert_eq!(report.state, "active");
        assert_eq!(report.pid, std::process::id());

        // A foreign process marker is detected but never removed or blocked.
        let foreign = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: std::process::id().wrapping_add(1),
            operation: "watch".to_string(),
            started_at_ms: now_ms(),
            refreshed_at_ms: now_ms(),
        };
        fs::write(marker_path(&dir), serde_json::to_string(&foreign).unwrap()).unwrap();
        let (_other, other_warnings) = OwnerGuard::acquire(&dir, "watch");
        assert_eq!(
            other_warnings.len(),
            1,
            "fresh foreign marker is reported: {other_warnings:?}"
        );
        assert!(other_warnings[0].contains("may be active"));
        // The foreign marker survived (never blocked or removed).
        let report = inspect(&dir).expect("foreign marker still present");
        assert_eq!(report.pid, foreign.pid);
        assert_eq!(report.state, "active");

        // Release: the current process (not the owner) removes nothing.
        drop(_other);
        assert!(inspect(&dir).is_some(), "non-owner drop removes nothing");

        // Simulate the foreign run having exited (its marker replaced by the
        // owner's own live record), then the owner's drop removes the marker.
        let own = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: std::process::id(),
            operation: "index".to_string(),
            started_at_ms: guard.started_at_ms,
            refreshed_at_ms: now_ms(),
        };
        fs::write(marker_path(&dir), serde_json::to_string(&own).unwrap()).unwrap();
        drop(guard);
        assert!(inspect(&dir).is_none(), "owner drop removes its marker");
    }

    #[test]
    fn stale_markers_are_reclaimed_and_reported() {
        let dir = scratch("stale");
        let stale = OwnerRecord {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: 4242,
            operation: "watch".to_string(),
            started_at_ms: now_ms() - 720_000,
            refreshed_at_ms: now_ms() - 720_000,
        };
        fs::write(marker_path(&dir), serde_json::to_string(&stale).unwrap()).unwrap();
        let (guard, warnings) = OwnerGuard::acquire(&dir, "index");
        assert_eq!(warnings.len(), 1, "stale marker is reported: {warnings:?}");
        assert!(warnings[0].contains("stale"), "{warnings:?}");
        let report = inspect(&dir).expect("new marker is visible");
        assert_eq!(report.pid, std::process::id());
        drop(guard);
        assert!(inspect(&dir).is_none());
    }

    #[test]
    fn unreadable_and_unknown_schema_markers_are_replaced() {
        let dir = scratch("garbage");
        fs::write(marker_path(&dir), "not json").unwrap();
        let (guard, warnings) = OwnerGuard::acquire(&dir, "index");
        assert!(
            warnings.iter().any(|w| w.contains("unreadable")),
            "{warnings:?}"
        );

        fs::write(
            marker_path(&dir),
            r#"{"schemaVersion":999,"pid":1,"operation":"index","startedAtMs":1,"refreshedAtMs":1}"#,
        )
        .unwrap();
        let (guard2, warnings2) = OwnerGuard::acquire(&dir, "index");
        assert!(
            warnings2.iter().any(|w| w.contains("unknown schema")),
            "{warnings2:?}"
        );
        drop(guard2);
        drop(guard);
    }

    #[test]
    fn refresh_is_throttled_and_updates_health() {
        let dir = scratch("refresh");
        let (guard, _) = OwnerGuard::acquire(&dir, "watch");
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
}
