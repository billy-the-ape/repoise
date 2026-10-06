//! Operator status view for the declared scope (master plan section 8).
//!
//! `repoise status` reports the resolved scope and snapshot, the published
//! index (generation, coverage, fingerprints), freshness against the live
//! snapshot, cache location/size and any recorded build error. It re-runs
//! the deterministic inventory (metadata only, no chunking) to judge
//! freshness honestly.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Result;
use crate::adapter::{SnapshotMode, SourceAdapter};
use crate::cache::CachePaths;
use crate::config::EffectiveConfig;
use crate::discovery;
use crate::error::Error;
use crate::store::{self, Store};

/// Config section of the status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusConfig {
    /// Path of the config file that contributed values, if any.
    pub file: Option<PathBuf>,
    /// Validation problems (empty when the config is sound).
    pub problems: Vec<String>,
}

/// Scope section of the status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusScope {
    /// Adapter kind name.
    pub adapter: String,
    /// Canonical root.
    pub root: PathBuf,
    /// Declared snapshot mode.
    pub mode: String,
    /// Stable repository scope id.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Sanitized remote identity, if any.
    pub remote_identity: Option<String>,
}

/// Snapshot section of the status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusSnapshot {
    /// Opaque snapshot id for the live snapshot.
    pub snapshot_id: String,
    /// Adapter-qualified opaque revision.
    pub revision: Option<String>,
    /// Branch/workspace hint.
    pub branch: Option<String>,
    /// Mode name.
    pub mode: String,
    /// Content manifest hash of the live snapshot.
    pub manifest_hash: String,
    /// Mutable overlay digest (working-tree/plain-directory modes).
    pub dirty_digest: Option<String>,
    /// Dirty (uncommitted) file count, git working-tree mode only.
    pub dirty_count: usize,
}

/// Index section of the status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusIndex {
    /// Current generation id.
    pub generation_id: i64,
    /// Index schema version.
    pub schema_version: i64,
    /// File record count.
    pub files: i64,
    /// Chunk record count.
    pub chunks: i64,
    /// Build time in milliseconds.
    pub built_at_ms: i64,
    /// Content manifest hash at build time.
    pub manifest_hash: String,
    /// Effective config fingerprint at build time.
    pub config_fingerprint: String,
    /// Parser fingerprint at build time.
    pub parser_fingerprint: String,
}

/// Freshness section of the status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusFreshness {
    /// `fresh` | `stale` | `unknown` | `error`.
    pub status: String,
    /// Why the index is not fresh.
    pub reasons: Vec<String>,
    /// Live snapshot id.
    pub snapshot_id: String,
    /// Live branch/workspace hint.
    pub branch: Option<String>,
    /// Dirty file count (git working-tree mode).
    pub dirty_count: usize,
}

/// Cache section of the status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusCache {
    /// Resolved cache root.
    pub root: PathBuf,
    /// Database path for the scope.
    pub db_path: PathBuf,
    /// Database file size in bytes.
    pub db_bytes: u64,
    /// WAL sidecar size in bytes (0 when absent).
    pub wal_bytes: u64,
}

/// Full operator status view.
#[derive(Clone, Debug, Serialize)]
pub struct StatusView {
    /// Engine version.
    pub version: String,
    /// Config resolution.
    pub config: StatusConfig,
    /// Resolved scope.
    pub scope: StatusScope,
    /// Live snapshot.
    pub snapshot: StatusSnapshot,
    /// Published index, if any.
    pub index: Option<StatusIndex>,
    /// Freshness judgment.
    pub freshness: StatusFreshness,
    /// Cache location and sizes.
    pub cache: StatusCache,
    /// Last recorded build error, if any.
    pub last_error: Option<String>,
}
/// Builds the full status view for one declared scope.
pub fn status(
    adapter: &dyn SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
    config_file: Option<&Path>,
    store: &Store,
    cache: &CachePaths,
) -> Result<StatusView> {
    let root = adapter.canonical_root()?;
    let remote_identity = adapter.remote_identity()?;
    let record =
        crate::provenance::RepositoryRecord::new(adapter.kind(), &root, remote_identity.clone());
    let worktree = crate::cache::worktree_id(&record.repo_id, &root, mode);
    let mode_name = format!("{mode:?}");

    // Live snapshot metadata (inventory without chunking).
    let inventory = discovery::inventory(adapter, mode, eff)?;
    let dirty_count =
        if adapter.kind() == crate::adapter::SourceKind::Git && mode == SnapshotMode::WorkingTree {
            crate::adapter::git::GitAdapter::new(&root)
                .map_err(Error::Adapter)?
                .dirty_count()
                .map_err(Error::Adapter)?
        } else {
            0
        };
    let snapshot = StatusSnapshot {
        snapshot_id: inventory.snapshot.snapshot_id.clone(),
        revision: inventory
            .snapshot
            .revision_id
            .as_ref()
            .map(|r| r.as_str().to_string()),
        branch: inventory.snapshot.branch.clone(),
        mode: mode_name.clone(),
        manifest_hash: inventory.manifest.manifest_hash.clone(),
        dirty_digest: inventory.snapshot.dirty_overlay_digest.clone(),
        dirty_count,
    };

    let conn = store.open()?;
    let current = store::current_generation(&conn, &record.repo_id, &worktree)?;
    let schema_version = store::meta_value(&conn, "schema_version")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let index = current.as_ref().map(|meta| StatusIndex {
        generation_id: meta.generation_id,
        schema_version,
        files: meta.files,
        chunks: meta.chunks,
        built_at_ms: meta.built_at_ms,
        manifest_hash: meta.manifest_hash.clone(),
        config_fingerprint: meta.config_fingerprint.clone(),
        parser_fingerprint: meta.parser_fingerprint.clone(),
    });
    let last_error = store::last_error(&conn)?;

    let (freshness_status, reasons) = match &current {
        None => (
            "unknown".into(),
            vec!["no published index for this scope".to_string()],
        ),
        Some(meta) => {
            let mut reasons = Vec::new();
            if meta.manifest_hash != inventory.manifest.manifest_hash {
                reasons.push(
                    "inventory manifest differs from the live snapshot (files changed)".into(),
                );
            }
            if meta.config_fingerprint != eff.fingerprint() {
                reasons.push("effective config changed since the last build".into());
            }
            if mode == SnapshotMode::WorkingTree && dirty_count > 0 {
                reasons.push(format!(
                    "{dirty_count} uncommitted change(s) in the working tree"
                ));
            }
            if reasons.is_empty() {
                ("fresh".into(), reasons)
            } else {
                ("stale".into(), reasons)
            }
        }
    };

    let db_path = cache.db_path(&record.repo_id, &worktree);
    let db_bytes = fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let wal_bytes = fs::metadata(db_path.with_extension("sqlite-wal"))
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(StatusView {
        version: env!("CARGO_PKG_VERSION").to_string(),
        config: StatusConfig {
            file: config_file.map(PathBuf::from),
            problems: eff.validate(),
        },
        scope: StatusScope {
            adapter: format!("{:?}", adapter.kind()),
            root: root.clone(),
            mode: mode_name,
            repo_id: record.repo_id.clone(),
            worktree_id: worktree.clone(),
            remote_identity,
        },
        snapshot,
        index,
        freshness: StatusFreshness {
            status: freshness_status,
            reasons,
            snapshot_id: inventory.snapshot.snapshot_id,
            branch: inventory.snapshot.branch,
            dirty_count,
        },
        cache: StatusCache {
            root: cache.root.clone(),
            db_path: db_path.clone(),
            db_bytes,
            wal_bytes,
        },
        last_error,
    })
}
