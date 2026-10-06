//! Git source adapter.
//!
//! Git is invoked with argument arrays (never shell strings) and NUL-delimited
//! output. Committed mode reads blobs from one resolved commit, so hashing
//! never touches mutable files. Detached HEAD is an explicit, supported state.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::adapter::{
    AdapterError, Capabilities, Capability, Revision, RevisionId, SnapshotMode, SourceAdapter,
    SourceEntry, SourceKind,
    filesystem::{read_within_root, scan_directory},
    normalize_relative,
};
use crate::hash;
use crate::provenance::sanitize_remote_identity;

/// Adapter over a Git repository.
#[derive(Clone, Debug)]
pub struct GitAdapter {
    root: PathBuf,
    canonical_root: PathBuf,
}

impl GitAdapter {
    /// Opens a Git adapter rooted at `root`; fails if it is not a Git repository.
    pub fn new(root: &Path) -> Result<Self, AdapterError> {
        let canonical_root = root.canonicalize().map_err(AdapterError::Io)?;
        let git_marker = canonical_root.join(".git");
        if !git_marker.exists() {
            return Err(AdapterError::NotARepository(root.to_path_buf()));
        }
        Ok(Self {
            root: root.to_path_buf(),
            canonical_root,
        })
    }

    /// Runs `git <args>` in the repository, returning stdout on success.
    pub fn run_git(&self, args: &[&str]) -> Result<String, AdapterError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .map_err(AdapterError::Io)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AdapterError::Other(format!(
                "git {args:?} failed: {stderr}"
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Resolves `git:<object id>` style revisions to a commit id.
    pub fn resolve_commit(&self, revision_id: &RevisionId) -> Result<String, AdapterError> {
        let Some(object) = revision_id.as_str().strip_prefix("git:") else {
            return Err(AdapterError::MissingRevision(revision_id.clone()));
        };
        match self.run_git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{object}^{{commit}}"),
        ]) {
            Ok(commit) if !commit.trim().is_empty() => Ok(commit.trim().to_string()),
            _ => Err(AdapterError::MissingRevision(revision_id.clone())),
        }
    }

    /// Exact bytes of the file at `commit:path`, or `MissingFile`.
    fn blob_at(&self, commit: &str, relative: &Path) -> Result<Vec<u8>, AdapterError> {
        let relative = normalize_relative(relative)?;
        let spec = super::to_posix(&relative);
        let blob = match self.run_git(&["rev-parse", "--verify", &format!("{commit}:{spec}")]) {
            Ok(value) => value,
            Err(_) => return Err(AdapterError::MissingFile(relative)),
        };
        let blob = blob.trim().to_string();
        self.run_git(&["cat-file", "blob", &blob])
            .map(|text| text.into_bytes())
    }

    /// Lists `(path, blob sha)` for every blob in the tree (NUL-safe).
    fn tree_blobs(&self, commit: &str) -> Result<Vec<(String, String)>, AdapterError> {
        let output = self.run_git(&["ls-tree", "-r", "-z", commit])?;
        let mut blobs = Vec::new();
        for entry in output.split('\0') {
            if entry.is_empty() {
                continue;
            }
            let Some((metadata, path)) = entry.split_once('\t') else {
                continue;
            };
            // metadata: "<mode> SP <type> SP <sha>"
            let mut parts = metadata.split(' ');
            let _mode = parts.next();
            let object_type = parts.next();
            let sha = parts.next();
            if object_type == Some("blob")
                && let Some(sha) = sha
            {
                blobs.push((path.to_string(), sha.to_string()));
            }
        }
        Ok(blobs)
    }

    /// Branch of the current HEAD; `None` when detached (explicit, not an error).
    pub fn current_branch(&self) -> Option<String> {
        self.run_git(&["symbolic-ref", "--short", "HEAD"])
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    /// Nearest name-rev of a commit; `None` when it cannot be named.
    fn branch_for_commit(&self, commit: &str) -> Option<String> {
        self.run_git(&["name-rev", "--name-only", commit])
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
}

impl SourceAdapter for GitAdapter {
    fn kind(&self) -> SourceKind {
        SourceKind::Git
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
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
        if mode == SnapshotMode::PlainDirectory {
            return Err(AdapterError::UnsupportedOperation {
                operation: "plain directory snapshot",
                capability: Capability::AtomicSnapshots,
            });
        }
        let (commit, branch) = match requested {
            Some(revision_id) => {
                let commit = self.resolve_commit(revision_id)?;
                let branch = self.branch_for_commit(&commit);
                (commit, branch)
            }
            None => {
                let head = self
                    .run_git(&["rev-parse", "HEAD"])
                    .map_err(|_| AdapterError::NoRevision("repository has no HEAD".into()))?
                    .trim()
                    .to_string();
                (head, self.current_branch())
            }
        };
        Ok(Revision {
            id: RevisionId::new(format!("git:{commit}")),
            branch,
        })
    }
    fn enumerate(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        match mode {
            SnapshotMode::Committed => {
                let commit = self.resolve_commit(&revision.id)?;
                let mut entries = Vec::new();
                for (path, blob_sha) in self.tree_blobs(&commit)? {
                    let bytes = self.run_git(&["cat-file", "blob", &blob_sha])?.into_bytes();
                    entries.push(SourceEntry {
                        path: PathBuf::from(path),
                        content_hash: hash::sha256_hex(&bytes),
                        size: bytes.len() as u64,
                    });
                }
                entries.sort_by(|a, b| a.path.cmp(&b.path));
                Ok(entries)
            }
            SnapshotMode::WorkingTree => scan_directory(&self.canonical_root, &self.canonical_root),
            SnapshotMode::PlainDirectory => Err(AdapterError::UnsupportedOperation {
                operation: "plain directory snapshot",
                capability: Capability::AtomicSnapshots,
            }),
        }
    }

    fn read(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        match mode {
            SnapshotMode::Committed => {
                let commit = self.resolve_commit(&revision.id)?;
                self.blob_at(&commit, relative)
            }
            SnapshotMode::WorkingTree => read_within_root(&self.canonical_root, relative),
            SnapshotMode::PlainDirectory => Err(AdapterError::UnsupportedOperation {
                operation: "plain directory snapshot",
                capability: Capability::AtomicSnapshots,
            }),
        }
    }

    fn remote_identity(&self) -> Result<Option<String>, AdapterError> {
        let Ok(url) = self.run_git(&["remote", "get-url", "origin"]) else {
            return Ok(None);
        };
        Ok(sanitize_remote_identity(&url))
    }
}

/// Lines of `.git/info/exclude` when present (working-tree local policy).
pub fn git_exclude_lines(root: &Path) -> Vec<String> {
    match fs::read_to_string(root.join(".git").join("info").join("exclude")) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}
