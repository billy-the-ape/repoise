//! Disposable per-trial workspace isolation.
//!
//! Every trial gets a fresh linked Git worktree checked out detached at the
//! frozen snapshot revision (plus the recorded patch, when selected). The
//! user's active checkout is never reset or cleaned: worktrees are added
//! under the experiment output directory and removed after the trial.
//! Materialization policy (recorded in the manifest): tracked content is
//! checked out by Git, symlinks are preserved, submodules are not
//! initialized, and LFS pointers are not fetched.

use std::path::{Path, PathBuf};

use crate::adapter::git::GitAdapter;
use crate::error::Error;

/// One disposable trial workspace.
#[derive(Clone, Debug)]
pub struct Workspace {
    /// The worktree root (the trial's exact searchable root).
    pub path: PathBuf,
    /// The parent repository the worktree was added to.
    pub git_root: PathBuf,
}

impl Workspace {
    /// Adds a detached worktree at `revision` under `path`.
    pub fn create(
        git_root: &Path,
        path: &Path,
        revision: &str,
        patch: Option<&Path>,
    ) -> crate::Result<Self> {
        let adapter = GitAdapter::new(git_root).map_err(Error::Adapter)?;
        let _ = std::fs::create_dir_all(path);
        match adapter.run_git(&[
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            revision,
        ]) {
            Ok(_) => {}
            Err(err) => {
                // Leave no partial directory behind.
                let _ = std::fs::remove_dir_all(path);
                return Err(Error::Adapter(err));
            }
        }
        if let Some(patch) = patch
            && let Err(err) = adapter.run_git_in(
                path,
                &["apply", "--whitespace=nowarn", &patch.to_string_lossy()],
            )
        {
            // The worktree registration already exists; remove it before
            // surfacing the error so a bad patchFile never leaks one.
            let workspace = Self {
                path: path.to_path_buf(),
                git_root: git_root.to_path_buf(),
            };
            let _ = workspace.remove();
            return Err(Error::Adapter(err));
        }
        Ok(Self {
            path: path.to_path_buf(),
            git_root: git_root.to_path_buf(),
        })
    }

    /// Removes the worktree (tolerating a dirty trial workspace) and prunes
    /// the registration; falls back to a plain directory removal.
    pub fn remove(&self) -> crate::Result<()> {
        let adapter = GitAdapter::new(&self.git_root).map_err(Error::Adapter)?;
        let _ = adapter.run_git(&[
            "worktree",
            "remove",
            "--force",
            &self.path.to_string_lossy(),
        ]);
        let _ = adapter.run_git(&["worktree", "prune"]);
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path).map_err(Error::Io)?;
            let _ = adapter.run_git(&["worktree", "prune"]);
        }
        Ok(())
    }
}
