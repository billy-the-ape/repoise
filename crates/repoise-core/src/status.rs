//! Operator status view for the declared scope (master plan section 8).
//!
//! `repoise status` reports the resolved scope and snapshot, the published
//! index (generation, coverage, fingerprints), freshness against the live
//! snapshot, cache location/size and any recorded build error. It re-runs
//! the deterministic inventory (metadata only, no chunking) to judge
//! freshness honestly.
//!
//! Freshness means the index represents the selected filesystem snapshot:
//! it is judged from the declared scope/snapshot (content manifest hash)
//! and the effective configuration fingerprint, with parser compatibility
//! enforced at publication by `indexing` (a changed parser fingerprint
//! forces a full reparse). Git working-tree dirtiness is a separate
//! concern: `snapshot.dirty_count` and `freshness.dirty_count` report it,
//! but uncommitted changes never make the index stale by themselves. An
//! index may legitimately be fresh over a dirty tree (its saved contents
//! are indexed); only a change to the saved snapshot, the effective
//! configuration, or the scope/mode makes it stale. Unsaved editor buffers
//! remain outside the filesystem snapshot either way.

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
    /// Informational: it never affects the freshness judgment.
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
    /// Vector record count in the current generation (0 when lexical-only).
    pub vectors: i64,
    /// Full embedding profile fingerprint when vectors are present.
    pub vector_profile: Option<String>,
    /// Stored embedding scope of the generation (`docs` or `all`); vectors
    /// and pending work are always judged against this persisted scope, not
    /// today's configuration.
    pub embedding_scope: String,
    /// Chunks eligible under the stored embedding scope (docs, or docs +
    /// code when the scope is `all`).
    pub eligible_chunks: i64,
    /// Lexical chunks the stored embedding scope never covers (e.g. config
    /// corpus); these are never embedded and never pending.
    pub ineligible_chunks: i64,
    /// Eligible chunks without a vector for the stored profile (0 when the
    /// generation is lexical-only or the eligible corpus is empty).
    pub vectors_pending: i64,
    /// Scope-aware coverage state: `disabled` (no embedding profile stored
    /// and none configured today), `profile-unavailable` (embeddings are
    /// configured today but the stored generation has no profile),
    /// `empty` (profile stored, eligible corpus empty), `partial` (eligible
    /// chunks await vectors) or `complete` (every eligible chunk covered).
    pub embedding_coverage: String,
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
    /// Dirty file count (git working-tree mode). Informational only:
    /// uncommitted changes never affect the freshness judgment.
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

/// Writer-ownership section of the status view.
///
/// `state` is derived from the authoritative scope lock (non-mutating
/// probe) plus the descriptive owner marker: `owned` (live writer with
/// current metadata), `busy` (lock held; marker missing or stale),
/// `stale-metadata` (lock free; marker is descriptive residue), `free`
/// (lock free, no marker). See [`crate::ownership::writer_state`].
#[derive(Clone, Debug, Serialize)]
pub struct WriterStatus {
    /// Whether the scope's OS lock is currently held.
    pub lock_held: bool,
    /// Machine state: `owned` | `busy` | `stale-metadata` | `free`.
    pub state: String,
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
    /// History lane state for the published generation, if any (enabled,
    /// item count, head revision, gaps, enrichment outcome).
    pub history_lane: Option<crate::history::HistoryLaneStatus>,
    /// Freshness judgment.
    pub freshness: StatusFreshness,
    /// Cache location and sizes.
    pub cache: StatusCache,
    /// Advisory writer-ownership marker for this scope, when one is present
    /// (the process that holds — or last held — the scope's ownership lock).
    pub owner: Option<crate::ownership::OwnerReport>,
    /// Authoritative writer state for this scope (lock probe + marker).
    pub writer: WriterStatus,
    /// Linked-worktree sharing state (shared store, opt-out, fallback).
    pub sharing: crate::shared::SharingInfo,
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
    let inventory = discovery::inventory(adapter, mode, eff, None)?;
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

    let sharing = crate::shared::describe(&crate::shared::resolve_sharing(adapter, eff, cache)?);
    let conn = store.open()?;
    let current = store::current_generation(&conn, &record.repo_id, &worktree)?;
    let schema_version = store::meta_value(&conn, "schema_version")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let index = match current.as_ref() {
        Some(meta) => Some({
            // Scope-aware coverage from the generation's persisted
            // scope/profile and the actual chunk corpora (never today's
            // config, which only distinguishes disabled from
            // profile-unavailable).
            let coverage = store::generation_vector_coverage(&conn, meta)?;
            let embedding_coverage = match coverage.level {
                store::CoverageLevel::NoProfile => {
                    if eff.preset == crate::config::Preset::Hybrid || eff.embedding.is_some() {
                        "profile-unavailable".to_string()
                    } else {
                        "disabled".to_string()
                    }
                }
                other => format!("{other:?}").to_lowercase(),
            };
            StatusIndex {
                generation_id: meta.generation_id,
                schema_version,
                files: meta.files,
                chunks: meta.chunks,
                built_at_ms: meta.built_at_ms,
                manifest_hash: meta.manifest_hash.clone(),
                config_fingerprint: meta.config_fingerprint.clone(),
                parser_fingerprint: meta.parser_fingerprint.clone(),
                vectors: coverage.vectors,
                vector_profile: meta.vector_profile.clone(),
                embedding_scope: meta.embedding_scope.as_str().to_string(),
                eligible_chunks: coverage.eligible_chunks,
                ineligible_chunks: coverage.ineligible_chunks,
                vectors_pending: coverage.pending,
                embedding_coverage,
            }
        }),
        None => None,
    };
    let last_error = store::last_error(&conn)?;
    // History lane state for the published generation (card K5): agents see
    // gaps and enrichment stops without running a history search.
    let history_lane = current
        .as_ref()
        .map(|meta| crate::history::lane_status(&conn, meta))
        .transpose()?;

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
            // Git working-tree dirtiness is reported (snapshot.dirty_count and
            // freshness.dirty_count) but never judged: the index is fresh when
            // it represents the saved snapshot, whether or not Git is dirty.
            // Saved uncommitted content can be indexed and fresh; only a
            // snapshot, configuration or scope mismatch makes it stale.
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
    // Writer state for this scope: the authoritative OS-lock probe
    // (non-mutating — creates nothing) plus the descriptive owner marker.
    let scope_dir = db_path.parent();
    let lock_held = scope_dir.is_some_and(crate::ownership::lock_held);
    let owner = scope_dir.and_then(crate::ownership::inspect);
    let writer = WriterStatus {
        lock_held,
        state: crate::ownership::writer_state(owner.as_ref(), lock_held).to_string(),
    };

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
        history_lane,
        freshness: StatusFreshness {
            status: freshness_status,
            reasons,
            snapshot_id: inventory.snapshot.snapshot_id,
            branch: inventory.snapshot.branch,
            dirty_count,
        },
        sharing,
        cache: StatusCache {
            root: cache.root.clone(),
            db_path: db_path.clone(),
            db_bytes,
            wal_bytes,
        },
        owner,
        writer,
        last_error,
    })
}
