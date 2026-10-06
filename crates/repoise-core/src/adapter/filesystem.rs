//! Filesystem source adapter for plain directories (and Git working trees).

use std::fs;
use std::path::{Path, PathBuf};

use crate::adapter::{
    AdapterError, Capabilities, Revision, RevisionId, SnapshotMode, SourceAdapter, SourceEntry,
    SourceKind, normalize_relative,
};
use crate::hash;

/// Adapter over a plain directory. Symlinks are never followed.
#[derive(Clone, Debug)]
pub struct FilesystemAdapter {
    root: PathBuf,
    canonical_root: PathBuf,
}

impl FilesystemAdapter {
    /// Opens a filesystem adapter rooted at `root`.
    pub fn new(root: &Path) -> Result<Self, AdapterError> {
        let canonical_root = root.canonicalize().map_err(AdapterError::Io)?;
        if !canonical_root.is_dir() {
            return Err(AdapterError::NotARepository(root.to_path_buf()));
        }
        Ok(Self {
            root: root.to_path_buf(),
            canonical_root,
        })
    }
}

impl SourceAdapter for FilesystemAdapter {
    fn kind(&self) -> SourceKind {
        SourceKind::Filesystem
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::NONE
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn canonical_root(&self) -> Result<PathBuf, AdapterError> {
        Ok(self.canonical_root.clone())
    }

    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        mode: SnapshotMode,
    ) -> Result<Revision, AdapterError> {
        match mode {
            SnapshotMode::PlainDirectory => {
                if requested.is_some() {
                    return Err(AdapterError::MissingRevision(RevisionId::new(
                        "filesystem snapshots have no named revisions",
                    )));
                }
                Ok(Revision {
                    id: RevisionId::new("fs:worktree"),
                    branch: None,
                })
            }
            _ => Err(AdapterError::UnsupportedOperation {
                operation: "committed snapshot",
                capability: crate::adapter::Capability::AtomicSnapshots,
            }),
        }
    }

    fn enumerate(
        &self,
        _revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        match mode {
            SnapshotMode::PlainDirectory => {
                scan_directory(&self.canonical_root, &self.canonical_root)
            }
            _ => Err(AdapterError::UnsupportedOperation {
                operation: "committed snapshot",
                capability: crate::adapter::Capability::AtomicSnapshots,
            }),
        }
    }

    fn read(
        &self,
        _revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        if mode != SnapshotMode::PlainDirectory {
            return Err(AdapterError::UnsupportedOperation {
                operation: "committed snapshot",
                capability: crate::adapter::Capability::AtomicSnapshots,
            });
        }
        read_within_root(&self.canonical_root, relative)
    }
}

/// Reads `relative` from `canonical_root` after containment validation.
pub fn read_within_root(canonical_root: &Path, relative: &Path) -> Result<Vec<u8>, AdapterError> {
    let relative = normalize_relative(relative)?;
    if relative.as_os_str().is_empty() {
        return Err(AdapterError::MissingFile(PathBuf::from(".")));
    }
    let target = canonical_root.join(&relative);
    match fs::canonicalize(&target) {
        Ok(resolved) => {
            if !resolved.starts_with(canonical_root) {
                return Err(AdapterError::Containment(relative));
            }
            fs::read(&resolved).map_err(AdapterError::Io)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(AdapterError::MissingFile(relative))
        }
        Err(err) => Err(AdapterError::Io(err)),
    }
}

/// Recursively scans `dir` (never following symlinks) into sorted entries.
pub fn scan_directory(canonical_root: &Path, dir: &Path) -> Result<Vec<SourceEntry>, AdapterError> {
    let mut entries = Vec::new();
    scan(canonical_root, dir, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn scan(
    canonical_root: &Path,
    dir: &Path,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), AdapterError> {
    for result in fs::read_dir(dir)? {
        let entry = result?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            // Never follow symlinks (security and determinism).
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            scan(canonical_root, &path, entries)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&path)?;
            let relative = path
                .strip_prefix(canonical_root)
                .map_err(|_| AdapterError::Containment(path.clone()))?
                .to_path_buf();
            entries.push(SourceEntry {
                path: relative,
                content_hash: hash::sha256_hex(&bytes),
                size: bytes.len() as u64,
            });
        }
    }
    Ok(())
}
