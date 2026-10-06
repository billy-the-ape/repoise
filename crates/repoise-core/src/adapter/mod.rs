//! Neutral source-adapter and snapshot contracts.
//!
//! Adapters separate source-control systems (Git first) from hosts (GitHub,
//! GitLab, ...) and from plain directories. They enumerate and read exact
//! source at an opaque, adapter-qualified revision and report capabilities
//! explicitly; unsupported operations return
//! [`AdapterError::UnsupportedOperation`] instead of failing ambiguously.

pub mod fake;
pub mod filesystem;
pub mod git;

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Kind of source an adapter reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// A plain directory with no source-control system.
    Filesystem,
    /// A Git repository; other VCS adapters are intentionally not implied.
    Git,
    /// Synthetic in-memory adapter used to prove the neutral contracts.
    Fake,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SourceKind::Filesystem => "filesystem",
            SourceKind::Git => "git",
            SourceKind::Fake => "fake",
        })
    }
}

/// Optional operations an adapter may or may not provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Snapshots are atomic: a revision never mixes states.
    AtomicSnapshots,
    /// Named branches exist and can be resolved.
    Branches,
    /// Diffs between revisions are available.
    Diffs,
    /// Revision history is available.
    History,
    /// Reads at a revision are immutable even while the working state changes.
    ImmutableReads,
}

/// Explicit capability set reported by an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities(u8);

impl Capabilities {
    /// No capabilities.
    pub const NONE: Self = Self(0);
    /// All capabilities.
    pub const ALL: Self = Self(0b11111);

    /// Builds a capability set from an iterator of capabilities.
    pub fn of(iter: impl Iterator<Item = Capability>) -> Self {
        let mut flags = 0u8;
        for capability in iter {
            flags |= flag_for(capability);
        }
        Self(flags)
    }

    /// Whether the adapter provides `capability`.
    pub fn has(self, capability: Capability) -> bool {
        self.0 & flag_for(capability) != 0
    }

    /// Lists the provided capabilities in a stable order.
    pub fn list(self) -> Vec<Capability> {
        [
            (Capability::AtomicSnapshots, 0b00001),
            (Capability::Branches, 0b00010),
            (Capability::Diffs, 0b00100),
            (Capability::History, 0b01000),
            (Capability::ImmutableReads, 0b10000),
        ]
        .into_iter()
        .filter(|(_, flag)| self.0 & flag != 0)
        .map(|(capability, _)| capability)
        .collect()
    }
}

fn flag_for(capability: Capability) -> u8 {
    match capability {
        Capability::AtomicSnapshots => 0b00001,
        Capability::Branches => 0b00010,
        Capability::Diffs => 0b00100,
        Capability::History => 0b01000,
        Capability::ImmutableReads => 0b10000,
    }
}

/// Opaque, adapter-qualified revision identifier.
///
/// Adapters choose their own revision format (for example `git:<object id>`);
/// consumers must never assume a fixed length or hash format.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RevisionId(String);

impl RevisionId {
    /// Wraps an opaque revision string; the adapter owns its meaning.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the raw revision string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A resolved revision plus optional branch hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    /// Opaque adapter-qualified revision id.
    pub id: RevisionId,
    /// Branch name when the adapter has one; `None` for detached states.
    pub branch: Option<String>,
}

/// How a snapshot is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotMode {
    /// Mutable working state (local agent default).
    WorkingTree,
    /// One resolved immutable revision (CI default).
    Committed,
    /// Plain directory state; only meaningful for filesystem-style adapters.
    PlainDirectory,
}

/// One exact source file in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    /// Relative path from the adapter root, normalized (no `..`, no leading `/`).
    pub path: PathBuf,
    /// Lowercase SHA-256 hex digest of the exact content.
    pub content_hash: String,
    /// Content size in bytes.
    pub size: u64,
}

/// Explicit adapter failures.
#[derive(Debug)]
pub enum AdapterError {
    /// Filesystem or subprocess I/O failure.
    Io(std::io::Error),
    /// The root is not a repository of this adapter kind.
    NotARepository(PathBuf),
    /// The requested revision does not exist or is not adapter-qualified.
    MissingRevision(RevisionId),
    /// No revision can be resolved from the current state.
    NoRevision(String),
    /// The requested file is not part of the snapshot.
    MissingFile(PathBuf),
    /// The path escapes the adapter root.
    Containment(PathBuf),
    /// The requested operation requires a capability the adapter does not have.
    UnsupportedOperation {
        /// Human name of the requested operation.
        operation: &'static str,
        /// Capability that is missing.
        capability: Capability,
    },
    /// Any other adapter-specific failure.
    Other(String),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterError::Io(err) => write!(f, "I/O error: {err}"),
            AdapterError::NotARepository(path) => {
                write!(
                    f,
                    "not a source repository of this kind: {}",
                    path.display()
                )
            }
            AdapterError::MissingRevision(id) => write!(f, "unknown revision: {id}"),
            AdapterError::NoRevision(detail) => write!(f, "cannot resolve a revision: {detail}"),
            AdapterError::MissingFile(path) => {
                write!(f, "file not in snapshot: {}", path.display())
            }
            AdapterError::Containment(path) => {
                write!(f, "path escapes the source root: {}", path.display())
            }
            AdapterError::UnsupportedOperation {
                operation,
                capability,
            } => {
                write!(
                    f,
                    "unsupported operation {operation}: capability {capability:?} not available"
                )
            }
            AdapterError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AdapterError {}

impl From<std::io::Error> for AdapterError {
    fn from(err: std::io::Error) -> Self {
        AdapterError::Io(err)
    }
}

/// Checks a capability and returns an explicit unsupported status.
pub fn require_capability(
    capabilities: Capabilities,
    capability: Capability,
    operation: &'static str,
) -> Result<(), AdapterError> {
    if capabilities.has(capability) {
        Ok(())
    } else {
        Err(AdapterError::UnsupportedOperation {
            operation,
            capability,
        })
    }
}

/// Normalizes a relative path, rejecting anything that escapes the root.
pub fn normalize_relative(path: &Path) -> Result<PathBuf, AdapterError> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AdapterError::Containment(path.to_path_buf()));
            }
        }
    }
    Ok(out)
}

/// Neutral source adapter: enumerate and read exact source at a revision.
pub trait SourceAdapter {
    /// The kind of source this adapter reads.
    fn kind(&self) -> SourceKind;

    /// The explicit capability set.
    fn capabilities(&self) -> Capabilities;

    /// The adapter root as given by the caller.
    fn root(&self) -> &Path;

    /// The canonical root used for containment checks.
    fn canonical_root(&self) -> Result<PathBuf, AdapterError>;

    /// Explicit status when `capability` is missing for an operation.
    fn require(&self, capability: Capability, operation: &'static str) -> Result<(), AdapterError> {
        require_capability(self.capabilities(), capability, operation)
    }

    /// Resolves a revision; `requested` may be `None` for the default state.
    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        mode: SnapshotMode,
    ) -> Result<Revision, AdapterError>;

    /// Enumerates all raw source files (policy filtering happens in discovery),
    /// sorted by relative path.
    fn enumerate(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError>;

    /// Reads the exact content of one relative path within the snapshot.
    fn read(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError>;

    /// Sanitized optional remote identity (host plus path, no credentials).
    fn remote_identity(&self) -> Result<Option<String>, AdapterError> {
        Ok(None)
    }
}
