//! Deterministic discovery/inventory over adapter snapshots.
//!
//! Inventory applies the explainable policy to raw adapter entries, then
//! bounds intake (size limit, UTF-8 validation, content secret scan) and
//! builds a deterministic snapshot manifest. Skip diagnostics carry relative
//! path and reason only, never content.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapter::{
    AdapterError, SnapshotMode, SourceAdapter, SourceEntry, SourceKind, filesystem::scan_directory,
    git::git_exclude_lines,
};
use crate::classify::{self, RoleRule};
use crate::config::EffectiveConfig;
use crate::error::Error;
use crate::hash;
use crate::ignore::{Decision, Policy};
use crate::provenance::{
    FileRecord, PARSER_VERSION_TEXT, RepositoryRecord, SnapshotManifest, SnapshotRecord,
};

/// One intake skip: relative path plus reason (never content).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipDiagnostic {
    /// Relative path of the skipped file.
    pub path: PathBuf,
    /// Why the file was skipped.
    pub reason: SkipReason,
}

/// Reasons a readable candidate file can still be skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Not valid UTF-8 (binary or unsupported encoding).
    BinaryEncoding,
    /// Larger than the configured max file size.
    FileTooLarge,
    /// Content matched a secret shape.
    SecretContent,
    /// Content changed between enumeration and read.
    ChangedDuringScan,
    /// The file could not be read.
    Unreadable,
}

/// Full deterministic inventory result.
#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    /// Repository-level provenance.
    pub repository: RepositoryRecord,
    /// Snapshot-level provenance.
    pub snapshot: SnapshotRecord,
    /// Deterministic manifest of included entries.
    pub manifest: SnapshotManifest,
    /// File records, sorted by relative path.
    pub files: Vec<FileRecord>,
    /// Skips with reasons (path + reason only).
    pub skips: Vec<SkipDiagnostic>,
}

/// Collects adapter-owned native ignore files as (base dir, label, lines),
/// ordered by directory depth.
pub fn native_ignore_files(
    adapter: &dyn SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
) -> Result<Vec<(PathBuf, PathBuf, Vec<String>)>, Error> {
    if !eff.inherit_git_ignores {
        return Ok(Vec::new());
    }
    let mut files: Vec<(PathBuf, PathBuf, Vec<String>)> = Vec::new();
    match adapter.kind() {
        SourceKind::Git => match mode {
            SnapshotMode::Committed => {
                // Evaluate .gitignore files from the selected commit.
                let revision = adapter.resolve(None, mode)?;
                let raw = adapter.enumerate(&revision, mode)?;
                for entry in raw {
                    if entry
                        .path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy() == ".gitignore")
                    {
                        let bytes = adapter.read(&revision, mode, &entry.path)?;
                        let lines = String::from_utf8_lossy(&bytes)
                            .lines()
                            .map(str::to_string)
                            .collect();
                        let base = entry
                            .path
                            .parent()
                            .unwrap_or_else(|| Path::new(""))
                            .to_path_buf();
                        files.push((base, entry.path, lines));
                    }
                }
            }
            SnapshotMode::WorkingTree => {
                let root = adapter.canonical_root()?;
                for (rel, lines) in gitignore_files_on_disk(&root)? {
                    let base = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
                    files.push((base, rel, lines));
                }
                let exclude_lines = git_exclude_lines(&root);
                if !exclude_lines.is_empty() {
                    // Local excludes are root-relative; they differ from the
                    // committed policy and are recorded in the fingerprint.
                    files.push((
                        PathBuf::new(),
                        PathBuf::from(".git/info/exclude"),
                        exclude_lines,
                    ));
                }
            }
            SnapshotMode::PlainDirectory => {}
        },
        SourceKind::Filesystem => {
            let root = adapter.canonical_root()?;
            for (rel, lines) in gitignore_files_on_disk(&root)? {
                let base = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
                files.push((base, rel, lines));
            }
        }
        SourceKind::Fake => {}
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

/// Finds `.gitignore` files on disk under `root`, relative to it.
fn gitignore_files_on_disk(root: &Path) -> Result<Vec<(PathBuf, Vec<String>)>, Error> {
    let entries = scan_directory(root, root)?;
    let mut out = Vec::new();
    for entry in entries {
        if entry
            .path
            .file_name()
            .is_some_and(|name| name.to_string_lossy() == ".gitignore")
        {
            let bytes = fs::read(root.join(&entry.path))?;
            let lines = String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_string)
                .collect();
            out.push((entry.path, lines));
        }
    }
    Ok(out)
}

/// Builds the deterministic inventory for one adapter snapshot.
pub fn inventory(
    adapter: &dyn SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
) -> Result<Inventory, Error> {
    let revision = adapter.resolve(None, mode)?;
    let raw = adapter.enumerate(&revision, mode)?;
    let native = native_ignore_files(adapter, mode, eff)?;
    let policy = Policy::build(eff, &native)?;
    let role_rules: Vec<RoleRule> = eff
        .document_roles
        .iter()
        .map(|(rule, _)| rule.clone())
        .collect();

    let mut entries: Vec<SourceEntry> = Vec::new();
    let mut files: Vec<FileRecord> = Vec::new();
    let mut skips: Vec<SkipDiagnostic> = Vec::new();

    for entry in raw {
        let decision = policy.decide(&entry.path);
        if !decision.included {
            continue;
        }
        if entry.size > eff.max_file_bytes {
            skips.push(SkipDiagnostic {
                path: entry.path.clone(),
                reason: SkipReason::FileTooLarge,
            });
            continue;
        }
        let bytes = match adapter.read(&revision, mode, &entry.path) {
            Ok(bytes) => bytes,
            Err(err) => {
                skips.push(SkipDiagnostic {
                    path: entry.path.clone(),
                    reason: match err {
                        AdapterError::Io(_) => SkipReason::Unreadable,
                        _ => SkipReason::Unreadable,
                    },
                });
                continue;
            }
        };
        // A working tree can change while scanning; report it honestly.
        if hash::sha256_hex(&bytes) != entry.content_hash {
            skips.push(SkipDiagnostic {
                path: entry.path.clone(),
                reason: SkipReason::ChangedDuringScan,
            });
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                skips.push(SkipDiagnostic {
                    path: entry.path.clone(),
                    reason: SkipReason::BinaryEncoding,
                });
                continue;
            }
        };
        if crate::ignore::scan_secret_content(&text).is_some() {
            skips.push(SkipDiagnostic {
                path: entry.path.clone(),
                reason: SkipReason::SecretContent,
            });
            continue;
        }
        let (role, lifecycle, source) = classify::classify(&entry.path, &role_rules, &text);
        files.push(FileRecord {
            path: entry.path.clone(),
            content_hash: entry.content_hash.clone(),
            role,
            lifecycle,
            classification_source: source,
            language: classify::detect_language(&entry.path),
            parser_version: PARSER_VERSION_TEXT.to_string(),
        });
        entries.push(entry);
    }

    let manifest = SnapshotManifest::build(entries);
    let dirty = match mode {
        SnapshotMode::Committed => None,
        SnapshotMode::WorkingTree | SnapshotMode::PlainDirectory => {
            Some(manifest.manifest_hash.clone())
        }
    };
    let canonical_root = adapter.canonical_root()?;
    let repository =
        RepositoryRecord::new(adapter.kind(), &canonical_root, adapter.remote_identity()?);
    let snapshot = SnapshotRecord::new(
        adapter.kind(),
        Some(revision.id.clone()),
        &manifest.manifest_hash,
        mode,
        revision.branch.clone(),
        dirty,
    );
    Ok(Inventory {
        repository,
        snapshot,
        manifest,
        files,
        skips,
    })
}

/// Explains the effective policy for one relative path (exists or not).
pub fn explain_path(
    adapter: &dyn SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
    relative: &Path,
) -> Result<Decision, Error> {
    let native = native_ignore_files(adapter, mode, eff)?;
    let policy = Policy::build(eff, &native)?;
    Ok(policy.decide(relative))
}
