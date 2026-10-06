//! Persistent cache location resolution (master plan section 6).
//!
//! The cache root is resolved through a tested platform adapter. Resolution
//! order: the `REPOISE_CACHE_DIR` environment override, then the configured
//! `cacheDir` (absolute paths used as-is; relative paths resolve against the
//! repository root as an explicit ignored project-local cache directory for
//! containers/portable workflows). `platform_cache_dir` returns the platform
//! user cache convention (XDG cache on Linux, `Library/Caches` on macOS,
//! `LocalAppData` on Windows) for operators who relocate the default.
//!
//! Layout under the root (generated data, never committed to Git):
//!
//! - `repos/<repoId>/worktrees/<worktreeId>/index.sqlite` — source manifests,
//!   generation metadata, chunks, FTS (and vectors in a later PR)
//! - `repos/<repoId>/worktrees/<worktreeId>/state.json` — versioned integration
//!   manifest and refresh diagnostics; the database remains canonical
//! - `repos/<repoId>/worktrees/<worktreeId>/tmp/` — disposable build staging
//! - `repos/<repoId>/embedding-cache/` — reserved for PR 3
//!
//! SQLite opens in WAL mode, so `-wal`/`-shm` sidecars exist next to the
//! database; tooling that moves or removes a scope must treat the whole
//! directory as the unit.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::hash;

/// Cache root plus the per-scope layout paths derived from it.
#[derive(Clone, Debug)]
pub struct CachePaths {
    /// Resolved cache root.
    pub root: PathBuf,
}

impl CachePaths {
    /// Resolves the cache root: env override > configured `cache_dir`
    /// (absolute, or relative to `root`).
    pub fn resolve(
        root: &Path,
        cache_dir: &str,
        env_override: Option<&str>,
    ) -> Result<Self, Error> {
        let resolved =
            if let Some(override_dir) = env_override.filter(|value| !value.trim().is_empty()) {
                let path = PathBuf::from(override_dir.trim());
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            } else if cache_dir.trim().is_empty() {
                return Err(Error::Config("cacheDir must not be empty".into()));
            } else if Path::new(cache_dir).is_absolute() {
                PathBuf::from(cache_dir)
            } else {
                root.join(cache_dir)
            };
        Ok(Self { root: resolved })
    }

    /// Platform user cache convention for this host (the relocatable default).
    pub fn platform_cache_dir() -> PathBuf {
        #[cfg(not(windows))]
        {
            if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
                && !xdg.is_empty()
            {
                return PathBuf::from(xdg).join("repoise");
            }
            let home = std::env::var_os("HOME").unwrap_or_default();
            let base = if home.is_empty() {
                PathBuf::from("/tmp")
            } else {
                PathBuf::from(home)
            };
            #[cfg(target_os = "macos")]
            {
                return base.join("Library").join("Caches").join("repoise");
            }
            #[cfg(not(target_os = "macos"))]
            {
                base.join(".cache").join("repoise")
            }
        }
        #[cfg(windows)]
        {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("repoise"));
            local.join("repoise")
        }
    }

    /// Repository scope directory: `<root>/repos/<repoId>`.
    pub fn repo_dir(&self, repo_id: &str) -> PathBuf {
        self.root.join("repos").join(repo_id)
    }

    /// Worktree scope directory: `repos/<repoId>/worktrees/<worktreeId>`.
    pub fn worktree_dir(&self, repo_id: &str, worktree_id: &str) -> PathBuf {
        self.repo_dir(repo_id).join("worktrees").join(worktree_id)
    }

    /// Persistent index database path for one scope.
    pub fn db_path(&self, repo_id: &str, worktree_id: &str) -> PathBuf {
        self.worktree_dir(repo_id, worktree_id).join("index.sqlite")
    }

    /// Non-canonical integration manifest path for one scope.
    pub fn state_path(&self, repo_id: &str, worktree_id: &str) -> PathBuf {
        self.worktree_dir(repo_id, worktree_id).join("state.json")
    }

    /// Disposable build staging directory for one scope.
    pub fn tmp_path(&self, repo_id: &str, worktree_id: &str) -> PathBuf {
        self.worktree_dir(repo_id, worktree_id).join("tmp")
    }
}

/// Derives a stable worktree scope id from the canonical root and snapshot
/// mode. Branch names and dirty state are deliberately not part of the id:
/// branches are mutable aliases and dirty state is reported, not a new scope.
pub fn worktree_id(
    repo_id: &str,
    canonical_root: &Path,
    mode: crate::adapter::SnapshotMode,
) -> String {
    let key = format!(
        "{}|{}|{:?}",
        repo_id,
        canonical_root.to_string_lossy(),
        mode
    );
    format!("wt-{}", hash::sha256_hex(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_cache_dir_resolves_against_root() {
        let paths = CachePaths::resolve(Path::new("/root"), ".repoise", None).unwrap();
        assert_eq!(paths.root, PathBuf::from("/root/.repoise"));
    }

    #[test]
    fn absolute_cache_dir_is_used_verbatim() {
        let paths = CachePaths::resolve(Path::new("/root"), "/var/cache/repoise", None).unwrap();
        assert_eq!(paths.root, PathBuf::from("/var/cache/repoise"));
    }

    #[test]
    fn env_override_wins_over_config() {
        let abs =
            CachePaths::resolve(Path::new("/root"), ".repoise", Some("/tmp/env-cache")).unwrap();
        assert_eq!(abs.root, PathBuf::from("/tmp/env-cache"));
        let rel = CachePaths::resolve(Path::new("/root"), ".repoise", Some("local-cache")).unwrap();
        assert_eq!(rel.root, PathBuf::from("/root/local-cache"));
        let empty = CachePaths::resolve(Path::new("/root"), ".repoise", Some("  ")).unwrap();
        assert_eq!(empty.root, PathBuf::from("/root/.repoise"));
    }

    #[test]
    fn layout_paths_follow_the_documented_structure() {
        let paths = CachePaths::resolve(Path::new("/root"), "cache", None).unwrap();
        assert_eq!(
            paths.db_path("repo-a", "wt-b"),
            PathBuf::from("/root/cache/repos/repo-a/worktrees/wt-b/index.sqlite")
        );
        assert_eq!(
            paths.state_path("repo-a", "wt-b"),
            PathBuf::from("/root/cache/repos/repo-a/worktrees/wt-b/state.json")
        );
        assert_eq!(
            paths.tmp_path("repo-a", "wt-b"),
            PathBuf::from("/root/cache/repos/repo-a/worktrees/wt-b/tmp")
        );
    }

    #[test]
    fn worktree_ids_are_stable_and_mode_sensitive() {
        let root = Path::new("/root");
        let a = worktree_id("repo-a", root, crate::adapter::SnapshotMode::WorkingTree);
        let b = worktree_id("repo-a", root, crate::adapter::SnapshotMode::WorkingTree);
        let c = worktree_id("repo-a", root, crate::adapter::SnapshotMode::Committed);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("wt-"));
    }
}
