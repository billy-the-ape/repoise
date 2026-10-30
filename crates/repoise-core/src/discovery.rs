//! Deterministic discovery/inventory over adapter snapshots.
//!
//! Inventory is policy-aware and bounded before content I/O: the adapter's
//! metadata-only intake collects native ignore files and candidate path/size
//! pairs, the explainable policy decides each candidate, and only accepted,
//! size-eligible files are read (once, bounded) and hashed. Skip diagnostics
//! carry relative path and reason only, never content.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::adapter::{AdapterError, SnapshotMode, SourceAdapter, SourceEntry};
use crate::classify::{self, RoleRule};
use crate::config::EffectiveConfig;
use crate::error::Error;
use crate::hash;
use crate::ignore::{Decision, Policy};
use crate::progress::{IndexPhase, IndexProgressReporter};
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
    // The adapter's intake walk collects native ignore files without
    // reading or hashing corpus content.
    let revision = adapter.resolve(None, mode)?;
    Ok(adapter.intake(&revision, mode)?.native)
}

/// Builds the deterministic inventory for one adapter snapshot.
/// `progress` reports per-candidate discovery progress when provided.
pub fn inventory(
    adapter: &dyn SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
    progress: Option<&dyn IndexProgressReporter>,
) -> Result<Inventory, Error> {
    let revision = adapter.resolve(None, mode)?;
    // Policy-aware intake: native ignore files plus candidate metadata, with
    // no corpus content read or hashed. Excluded or oversized files below
    // never incur any content I/O.
    let intake = adapter.intake(&revision, mode)?;
    // Honor the config flag exactly as `native_ignore_files` does for
    // explain/doctor: with inheritance disabled the collected native ignore
    // files contribute no policy rules (they are still collected, since they
    // are explicitly small policy reads).
    let native: &[(PathBuf, PathBuf, Vec<String>)] = if eff.inherit_git_ignores {
        &intake.native
    } else {
        &[]
    };
    let policy = Policy::build(eff, native)?;
    let role_rules: Vec<RoleRule> = eff
        .document_roles
        .iter()
        .map(|(rule, _)| rule.clone())
        .collect();

    // The candidate list is the reliable denominator for discovery work.
    let discovery_total = intake.candidates.len() as u64;
    if let Some(reporter) = progress {
        reporter.phase_started(IndexPhase::Discovery, Some(discovery_total));
    }
    let mut entries: Vec<SourceEntry> = Vec::new();
    let mut files: Vec<FileRecord> = Vec::new();
    let mut skips: Vec<SkipDiagnostic> = Vec::new();
    for (discovered, candidate) in intake.candidates.iter().enumerate() {
        // The loop position is the number of units completed before this one,
        // honoring the reporter's "units complete" contract (the final line is
        // rendered by phase_finished after the last candidate is processed).
        if let Some(reporter) = progress {
            reporter.progress(IndexPhase::Discovery, discovered as u64, discovery_total);
        }
        let decision = policy.decide(&candidate.path);
        if !decision.included {
            // Excluded by policy: never read, never hashed.
            continue;
        }
        if candidate.size > eff.max_file_bytes {
            skips.push(SkipDiagnostic {
                path: candidate.path.clone(),
                reason: SkipReason::FileTooLarge,
            });
            continue;
        }
        // One bounded content read per accepted candidate: the size limit is
        // applied against the observed bytes, so a file that grew after the
        // metadata check is bounded, not fully read.
        let bytes = match adapter.read_bounded(&revision, mode, &candidate.path, eff.max_file_bytes)
        {
            Ok(bytes) => bytes,
            Err(err) => {
                // A file that vanished between the metadata check and the read
                // is tolerated; any other failure on an eligible file is real.
                let vanished = match &err {
                    AdapterError::MissingFile(_) => true,
                    AdapterError::Io(io_err) => io_err.kind() == std::io::ErrorKind::NotFound,
                    _ => false,
                };
                if !vanished {
                    return Err(err.into());
                }
                skips.push(SkipDiagnostic {
                    path: candidate.path.clone(),
                    reason: SkipReason::Unreadable,
                });
                continue;
            }
        };
        let observed = bytes.len() as u64;
        if observed > eff.max_file_bytes {
            // The file grew past the bound after the metadata check; the
            // bounded read stopped at max+1 bytes.
            skips.push(SkipDiagnostic {
                path: candidate.path.clone(),
                reason: SkipReason::FileTooLarge,
            });
            continue;
        }
        // A working tree can change while scanning; with a single content
        // read the observable signal is a size drift from the metadata.
        if observed != candidate.size {
            skips.push(SkipDiagnostic {
                path: candidate.path.clone(),
                reason: SkipReason::ChangedDuringScan,
            });
            continue;
        }
        // The accepted bytes are hashed once and reused for the file record
        // and the snapshot manifest.
        let content_hash = hash::sha256_hex(&bytes);
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                skips.push(SkipDiagnostic {
                    path: candidate.path.clone(),
                    reason: SkipReason::BinaryEncoding,
                });
                continue;
            }
        };
        if crate::ignore::scan_secret_content(&text).is_some() {
            skips.push(SkipDiagnostic {
                path: candidate.path.clone(),
                reason: SkipReason::SecretContent,
            });
            continue;
        }
        let (role, lifecycle, source) = classify::classify(&candidate.path, &role_rules, &text);
        files.push(FileRecord {
            path: candidate.path.clone(),
            content_hash: content_hash.clone(),
            role,
            lifecycle,
            classification_source: source,
            language: classify::detect_language(&candidate.path),
            parser_version: PARSER_VERSION_TEXT.to_string(),
        });
        entries.push(SourceEntry {
            path: candidate.path.clone(),
            content_hash,
            size: observed,
        });
    }

    if let Some(reporter) = progress {
        // Every enumerated entry is processed by here; report completion so
        // the phase's final line reflects it.
        reporter.progress(IndexPhase::Discovery, discovery_total, discovery_total);
        reporter.phase_finished(IndexPhase::Discovery);
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
