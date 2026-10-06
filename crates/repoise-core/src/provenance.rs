//! Versioned provenance record shapes (master plan section 6).
//!
//! Every result must trace to exact source: repository, snapshot and file
//! records pin source kind, opaque revision, content manifest hash and
//! per-file content hash. Chunk, embedding, generation and history-item
//! shapes arrive with their implementing PRs (K2/K3/K5).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::adapter::{RevisionId, SnapshotMode, SourceEntry, SourceKind};
use crate::classify::{ClassificationSource, Lifecycle, Role};
use crate::hash;

/// Version of the generic text parser recorded on file records.
pub const PARSER_VERSION_TEXT: &str = "text/1";

/// Repository-level provenance record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRecord {
    /// Opaque repo id derived from source kind + canonical root binding.
    pub repo_id: String,
    /// Root binding (canonical absolute path).
    pub root: PathBuf,
    /// Sanitized remote identity (host + path, never credentials).
    pub remote_identity: Option<String>,
    /// Visibility/scope hint; `unknown` unless an adapter proves otherwise.
    pub visibility: String,
}

impl RepositoryRecord {
    /// Builds a record; identity never depends on the remote URL, so two
    /// independent clones of the same remote stay distinct.
    pub fn new(kind: SourceKind, root: &std::path::Path, remote_identity: Option<String>) -> Self {
        let root_key = root.to_string_lossy().into_owned();
        Self {
            repo_id: format!("repo-{}", hash::sha256_hex(format!("{kind}|{root_key}"))),
            root: root.to_path_buf(),
            remote_identity,
            visibility: "unknown".into(),
        }
    }
}

/// Snapshot-level provenance record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    /// Opaque snapshot id derived from kind, revision, manifest and mode.
    pub snapshot_id: String,
    /// Adapter kind that produced the snapshot.
    pub source_kind: SourceKind,
    /// Opaque revision when the adapter has one.
    pub revision_id: Option<RevisionId>,
    /// Content manifest hash over the included entries.
    pub manifest_hash: String,
    /// How the snapshot was taken.
    pub mode: SnapshotMode,
    /// Optional branch/workspace hint.
    pub branch: Option<String>,
    /// Digest of the mutable overlay for working-tree snapshots.
    pub dirty_overlay_digest: Option<String>,
}

impl SnapshotRecord {
    /// Builds a record with a deterministic snapshot id.
    pub fn new(
        kind: SourceKind,
        revision_id: Option<RevisionId>,
        manifest_hash: &str,
        mode: SnapshotMode,
        branch: Option<String>,
        dirty_overlay_digest: Option<String>,
    ) -> Self {
        let revision = revision_id.as_ref().map(RevisionId::as_str).unwrap_or("");
        let key = format!("{kind}|{revision}|{manifest_hash}|{mode:?}");
        Self {
            snapshot_id: format!("snap-{}", hash::sha256_hex(key)),
            source_kind: kind,
            revision_id,
            manifest_hash: manifest_hash.to_string(),
            mode,
            branch,
            dirty_overlay_digest,
        }
    }
}

/// File-level provenance record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Relative path within the source root.
    pub path: PathBuf,
    /// Lowercase SHA-256 hex digest of the exact content.
    pub content_hash: String,
    /// Role classification (e.g. current-doc, code).
    pub role: Role,
    /// Lifecycle (separate from role).
    pub lifecycle: Lifecycle,
    /// Whether metadata was explicit (front matter) or inferred.
    pub classification_source: ClassificationSource,
    /// Detected language key (e.g. `markdown`, `rust`, `text`).
    pub language: String,
    /// Parser version that would process this file.
    pub parser_version: String,
}

/// Deterministic snapshot manifest over included entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Included entries, sorted by relative path.
    pub entries: Vec<SourceEntry>,
    /// Lowercase SHA-256 hex digest of the canonical entry listing.
    pub manifest_hash: String,
}

impl SnapshotManifest {
    /// Sorts `entries` and computes the manifest hash.
    pub fn build(entries: Vec<SourceEntry>) -> Self {
        let mut entries = entries;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            manifest_hash: Self::hash(&entries),
            entries,
        }
    }

    /// Hashes entries in path order: `path NUL hash NUL size NUL`.
    pub fn hash(entries: &[SourceEntry]) -> String {
        let mut key = String::new();
        for entry in entries {
            key.push_str(&entry.path.to_string_lossy());
            key.push('\0');
            key.push_str(&entry.content_hash);
            key.push('\0');
            key.push_str(&entry.size.to_string());
            key.push('\0');
        }
        hash::sha256_hex(key)
    }
}

/// Strips credentials, scheme, ports, fragments and `.git` suffixes from a
/// remote URL, keeping `host/path`. No host-specific fields are assumed.
pub fn sanitize_remote_identity(url: &str) -> Option<String> {
    let mut s = url.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(i) = s.find(['#', '?']) {
        s = &s[..i];
    }
    if !s.contains("://") {
        // scp-like syntax: user@host:path
        if let Some(at) = s.find('@') {
            let rest = &s[at + 1..];
            if let Some((host, path)) = rest.split_once(':') {
                return Some(trim_git_suffix(&format!("{host}/{path}")));
            }
        }
        return Some(trim_git_suffix(s));
    }
    let rest = &s[s.find("://").unwrap() + 3..];
    let authority_path = match rest.rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    let (host, path) = authority_path
        .split_once('/')
        .unwrap_or((authority_path, ""));
    let host = host.rsplit_once(':').map(|(host, _)| host).unwrap_or(host);
    let path = path.trim_matches('/');
    if host.is_empty() {
        return None;
    }
    let combined = format!("{host}/{path}");
    Some(trim_git_suffix(combined.trim_end_matches('/')))
}

fn trim_git_suffix(value: &str) -> String {
    value.trim_end_matches(".git").to_string()
}
