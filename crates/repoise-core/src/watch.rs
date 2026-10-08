//! Incremental index watcher (card K6, master plan section 12).
//!
//! The watcher observes the declared scope by polling the deterministic
//! inventory (metadata only between scans) and runs the same serialized,
//! incremental index operation used by `repoise index` — no separate build
//! path. Changes are debounced (default 200 ms) and coalesced into one
//! publication per quiescent window; a branch/workspace switch forces a full
//! rescan and reconciliation. The pending change set is bounded; beyond the
//! bound the watcher degrades to a full reparse instead of queuing. The
//! generated cache directory is excluded from the corpus by default, so the
//! watcher's own output never feeds back into the index.
//!
//! Watch publication is offline (no embedding provider call, no host
//! enrichment); remote work remains an explicit operator action.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::Result;
use crate::adapter::{SnapshotMode, SourceAdapter};
use crate::cache::CachePaths;
use crate::config::EffectiveConfig;
use crate::discovery;
use crate::indexing::{self, IndexRequest};
use crate::store::{self, Store};

/// Default poll interval between scans.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);
/// Default debounce before a detected change is published.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(200);
/// Default bound on the pending changed-path set before degrading to full.
pub const DEFAULT_MAX_PENDING_PATHS: usize = 4096;

/// Watcher tuning.
#[derive(Clone, Copy, Debug)]
pub struct WatchOptions {
    /// Poll interval between scans.
    pub interval: Duration,
    /// Debounce before a detected change is published.
    pub debounce: Duration,
    /// Bound on the pending changed-path set (beyond it: full reparse).
    pub max_pending_paths: usize,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            debounce: DEFAULT_DEBOUNCE,
            max_pending_paths: DEFAULT_MAX_PENDING_PATHS,
        }
    }
}

/// Outcome of one watcher step that published a new generation.
#[derive(Clone, Debug, Serialize)]
pub struct WatchPublication {
    /// Published generation id.
    pub generation_id: i64,
    /// Changed paths that triggered the publication (empty when a full
    /// reparse was run).
    pub changed_paths: Vec<String>,
    /// Files reparsed by the publication.
    pub files_reparsed: usize,
    /// Whether a full reparse (not incremental) was run.
    pub force_full: bool,
    /// Branch/workspace switch detected since the previous scan.
    pub branch_switch: bool,
    /// Publication duration in milliseconds.
    pub duration_ms: u64,
}

/// One step of the watch loop.
/// Outcome of one watcher step.
#[derive(Debug)]
pub enum WatchStep {
    /// No changes since the last scan.
    Idle,
    /// A new generation was published.
    Published(WatchPublication),
    /// A change was detected but the debounce window was cancelled first.
    Cancelled,
}

/// The incremental watcher for one declared scope.
pub struct Watcher {
    adapter: Box<dyn SourceAdapter>,
    mode: SnapshotMode,
    eff: EffectiveConfig,
    store: Store,
    cache: CachePaths,
    options: WatchOptions,
    repo_id: String,
    worktree_id: String,
    last_branch: Option<String>,
    published_manifest: Option<String>,
    published_fingerprint: Option<String>,
}

impl Watcher {
    /// Creates a watcher for one declared scope.
    pub fn new(
        adapter: Box<dyn SourceAdapter>,
        mode: SnapshotMode,
        eff: EffectiveConfig,
        store: Store,
        cache: CachePaths,
        options: WatchOptions,
    ) -> Result<Self> {
        let root = adapter.canonical_root()?;
        let record = crate::provenance::RepositoryRecord::new(
            adapter.kind(),
            &root,
            adapter.remote_identity()?,
        );
        let worktree_id = crate::cache::worktree_id(&record.repo_id, &root, mode);
        let conn = store.open()?;
        let published = store::current_generation(&conn, &record.repo_id, &worktree_id)?;
        Ok(Self {
            adapter,
            mode,
            eff,
            store,
            cache,
            options,
            repo_id: record.repo_id,
            worktree_id,
            last_branch: None,
            published_manifest: published.as_ref().map(|m| m.manifest_hash.clone()),
            published_fingerprint: published.as_ref().map(|m| m.config_fingerprint.clone()),
        })
    }
    /// Runs one scan-and-reconcile iteration, publishing at most one new
    /// generation. Serializes with itself through the caller's loop; the
    /// `cancel` flag is checked throughout the debounce and honored before
    /// any publication starts.
    pub fn step(&mut self, cancel: &AtomicBool) -> Result<WatchStep> {
        let started = Instant::now();
        let inventory = discovery::inventory(self.adapter.as_ref(), self.mode, &self.eff)?;
        let live_branch = inventory.snapshot.branch.clone();
        let branch_switch = self.branch_changed(&live_branch);

        if !self.needs_publish(&inventory) {
            self.last_branch = live_branch;
            return Ok(WatchStep::Idle);
        }

        // Debounce: give in-flight writes time to settle, checking cancel.
        let deadline = started + self.options.debounce;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.last_branch = live_branch;
                return Ok(WatchStep::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(20)));
        }
        if cancel.load(Ordering::SeqCst) {
            self.last_branch = live_branch;
            return Ok(WatchStep::Cancelled);
        }

        // Coalesce: rescan after the debounce and reconcile against the
        // freshest state, not the pre-debounce scan.
        let inventory = discovery::inventory(self.adapter.as_ref(), self.mode, &self.eff)?;
        let live_branch = inventory.snapshot.branch.clone();
        if !self.needs_publish(&inventory) {
            self.last_branch = live_branch;
            return Ok(WatchStep::Idle);
        }
        let branch_switch_now = branch_switch || self.branch_changed(&live_branch);

        // Bounded pending set: paths changed versus the published generation.
        let changed_paths = self.changed_paths(&inventory)?;
        let force_full = branch_switch_now || changed_paths.len() > self.options.max_pending_paths;

        let built = Instant::now();
        let request = IndexRequest {
            affected_paths: Some(changed_paths.iter().map(PathBuf::from).collect()),
            force_full,
        };
        let outcome = indexing::index(
            self.adapter.as_ref(),
            self.mode,
            &self.eff,
            &self.store,
            &self.cache,
            &request,
            None,
            None,
        )?;
        self.last_branch = live_branch;
        self.published_manifest = Some(outcome.manifest_hash.clone());
        self.published_fingerprint = Some(self.eff.fingerprint());
        Ok(WatchStep::Published(WatchPublication {
            generation_id: outcome.generation_id,
            changed_paths: if force_full {
                Vec::new()
            } else {
                changed_paths
            },
            files_reparsed: outcome.files_reparsed,
            force_full,
            branch_switch: branch_switch_now,
            duration_ms: built.elapsed().as_millis() as u64,
        }))
    }

    /// Whether the live snapshot differs from the published generation.
    fn needs_publish(&self, inventory: &crate::discovery::Inventory) -> bool {
        match (
            self.published_manifest.as_ref(),
            self.published_fingerprint.as_ref(),
        ) {
            (Some(manifest), Some(fp)) => {
                manifest != &inventory.manifest.manifest_hash || fp != &self.eff.fingerprint()
            }
            _ => true,
        }
    }

    /// True when the live branch differs from the previously observed one.
    fn branch_changed(&self, live_branch: &Option<String>) -> bool {
        match (self.last_branch.as_ref(), live_branch.as_ref()) {
            (Some(prev), Some(live)) => prev != live,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// Paths added, removed or changed versus the published generation.
    fn changed_paths(&self, inventory: &crate::discovery::Inventory) -> Result<Vec<String>> {
        let conn = self.store.open()?;
        let prev = store::load_generation(&conn, &self.repo_id, &self.worktree_id)?;
        let prev_set: std::collections::HashMap<&std::path::Path, &str> = prev
            .as_ref()
            .map(|prev| {
                prev.files
                    .iter()
                    .map(|file| (std::path::Path::new(&file.path), file.content_hash.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        let live_set: std::collections::HashMap<&std::path::Path, &str> = inventory
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.content_hash.as_str()))
            .collect();
        let mut changed: Vec<String> = Vec::new();
        for file in &inventory.files {
            let path: &std::path::Path = file.path.as_path();
            match prev_set.get(path) {
                Some(prev_hash) if *prev_hash == file.content_hash => {}
                _ => changed.push(file.path.to_string_lossy().into_owned()),
            }
        }
        for path in prev_set.keys() {
            if !live_set.contains_key(path) {
                changed.push(path.to_string_lossy().into_owned());
            }
        }
        changed.sort();
        changed.dedup();
        Ok(changed)
    }
}
