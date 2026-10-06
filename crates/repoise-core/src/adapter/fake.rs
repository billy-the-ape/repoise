//! Synthetic non-Git revision adapter for contract fixtures.
//!
//! This adapter holds in-memory trees keyed by opaque revision ids. It proves
//! the neutral snapshot/revision contracts without promising production
//! support for additional VCS systems.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::adapter::{
    AdapterError, Capabilities, Capability, Revision, RevisionId, SnapshotMode, SourceAdapter,
    SourceEntry, SourceKind, normalize_relative,
};
use crate::hash;

/// In-memory adapter with opaque, self-owned revision ids (for example
/// `fake:v2`). Content is immutable once inserted.
#[derive(Clone, Debug)]
pub struct FakeRevisionAdapter {
    /// Pseudo-root label; not a real directory.
    label: PathBuf,
    /// Revision id string -> sorted path -> content bytes.
    revisions: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

impl FakeRevisionAdapter {
    /// Builds a fake adapter; `revisions` maps id string to path -> bytes.
    pub fn new(
        label: impl Into<PathBuf>,
        revisions: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    ) -> Self {
        Self {
            label: label.into(),
            revisions,
        }
    }

    /// Returns the ids of all revisions.
    pub fn revision_ids(&self) -> Vec<String> {
        self.revisions.keys().cloned().collect()
    }
}

impl SourceAdapter for FakeRevisionAdapter {
    fn kind(&self) -> SourceKind {
        SourceKind::Fake
    }

    fn capabilities(&self) -> Capabilities {
        // In-memory trees are atomic and immutable; there is no history or
        // diff machinery, so those requests must fail explicitly.
        Capabilities::of([Capability::AtomicSnapshots, Capability::ImmutableReads].into_iter())
    }

    fn root(&self) -> &Path {
        &self.label
    }

    fn canonical_root(&self) -> Result<PathBuf, AdapterError> {
        Ok(self.label.clone())
    }

    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        _mode: SnapshotMode,
    ) -> Result<Revision, AdapterError> {
        let id = match requested {
            Some(requested) => requested.clone(),
            None => match self.revision_ids().as_slice() {
                [only] => RevisionId::new(only),
                _ => {
                    return Err(AdapterError::NoRevision(
                        "no requested revision and no unique default".into(),
                    ));
                }
            },
        };
        if !self.revisions.contains_key(id.as_str()) {
            return Err(AdapterError::MissingRevision(id));
        }
        Ok(Revision { id, branch: None })
    }

    fn enumerate(
        &self,
        revision: &Revision,
        _mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        let tree = self
            .revisions
            .get(revision.id.as_str())
            .ok_or_else(|| AdapterError::MissingRevision(revision.id.clone()))?;
        Ok(tree
            .iter()
            .map(|(path, bytes)| SourceEntry {
                path: PathBuf::from(path),
                content_hash: hash::sha256_hex(bytes),
                size: bytes.len() as u64,
            })
            .collect())
    }

    fn read(
        &self,
        revision: &Revision,
        _mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        let relative = normalize_relative(relative)?;
        let key = super::to_posix(&relative);
        let tree = self
            .revisions
            .get(revision.id.as_str())
            .ok_or_else(|| AdapterError::MissingRevision(revision.id.clone()))?;
        tree.get(&key)
            .cloned()
            .ok_or(AdapterError::MissingFile(relative))
    }
}
