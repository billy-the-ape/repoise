//! Cache purging for the relocatable cache (master plan section 11).
//!
//! `repoise purge` removes generated cache data only — never source files.
//! Scope ids are validated before any path is touched, and the removal unit
//! is the whole scope directory (database plus WAL/SHM sidecars).
//!
//! Cross-worktree shared stores (see [`crate::shared`]): a worktree-scoped
//! purge removes the scope directory and the CLI additionally removes that
//! scope's member registration, leaving sibling worktrees' shared artifacts
//! and vectors intact. `--all` removes everything under the selected cache
//! root — including `families/` when an explicit cache root is in use. With
//! the default checkout-local cache root, the family store lives under the
//! repository's Git common directory (outside the cache root) and is not
//! removed by `--all`, so sibling worktrees keep their shared data.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Result;
use crate::cache::CachePaths;
use crate::error::Error;

/// One purge request.
#[derive(Clone, Debug, Default)]
pub struct PurgeRequest {
    /// Remove the entire cache root (all scopes).
    pub all: bool,
    /// Scope to purge (ignored when `all` is set).
    pub repo_id: Option<String>,
    pub worktree_id: Option<String>,
}

/// A scope whose ownership lock is currently held by a live writer.
///
/// An active `index`/`watch`/`purge` writer must not lose its scope mid-run
/// (partial generations, lost lock state); the operator stops the writer
/// first and retries. The list is non-empty only for scopes that are owned
/// at the moment of the check.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedScope {
    /// Scope directory path.
    pub path: PathBuf,
    /// Descriptive owner marker, when one is present.
    pub owner: Option<crate::ownership::OwnerReport>,
}

/// A purge that was refused because a live writer owns at least one scope.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedScopePurgeReport {
    /// The owned scopes that were skipped.
    pub owned: Vec<OwnedScope>,
}

/// Report of what was removed.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeReport {
    /// Paths removed.
    pub removed: Vec<PathBuf>,
    /// Present when the purge was refused because live writer(s) own
    /// scope(s): nothing was removed.
    pub skipped_owned: Option<OwnedScopePurgeReport>,
}

/// Validates a scope id: `[a-z0-9][a-z0-9-]*` (ids are hash-derived).
fn validate_id(value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(Error::Policy(format!("invalid scope id: {value}")))
    }
}

/// Purges the whole cache or one scope directory.
///
/// One writer per scope: before removing a scope directory, the purge takes
/// that scope's ownership lock, so a live `index`/`watch` writer's scope can
/// never be removed from under it, and removal cannot be bypassed by
/// unlinking/recreating the lock file (the lock is held on the open file, not
/// by path name).
///
/// A scoped purge whose scope is owned fails with [`Error::Owned`] before
/// anything is removed. `--all` removes every scope it can lock; owned
/// scopes are skipped and reported in `skipped_owned`. The scope check runs
/// twice (before removal and immediately before the final root sweep), so a
/// writer that starts during the purge is still locked and skipped; the only
/// residual race is a writer starting in the instant between the final check
/// and the root sweep, which is inherent to lock-free directory sweeps
/// (scopes must not block each other, so there is no root-level writer lock).
pub fn purge(cache: &CachePaths, request: &PurgeRequest) -> Result<PurgeReport> {
    if request.all {
        return purge_all(cache);
    }
    let Some(repo_id) = &request.repo_id else {
        return Err(Error::Config("purge requires --all or a scope".into()));
    };
    let Some(worktree_id) = &request.worktree_id else {
        return Err(Error::Config("purge requires a worktree scope id".into()));
    };
    validate_id(repo_id)?;
    validate_id(worktree_id)?;
    let dir = cache.worktree_dir(repo_id, worktree_id);
    if !dir.exists() {
        return Err(Error::IndexState(format!(
            "scope not found in cache: {dir:?}"
        )));
    }
    // Hold the scope's ownership lock while it is removed; a live writer
    // holds the same lock and is reported (never interrupted).
    let _lock = crate::ownership::ScopeLock::acquire(&dir, "purge")?;
    fs::remove_dir_all(&dir)?;
    let removed = vec![dir];
    // Trim empty parents (keep the layout).
    let _ = fs::remove_dir(cache.repo_dir(repo_id).join("worktrees"));
    Ok(PurgeReport {
        removed,
        skipped_owned: None,
    })
}

/// Walks `repos_root/*/worktrees/*` and locks each scope directory before
/// touching it: free scopes are removed, owned scopes are recorded (never
/// removed). Used for the main pass and again immediately before the final
/// root sweep, so a writer that starts during the purge cannot lose its
/// scope to the sweep.
fn purge_scope_dirs(
    repos_root: &Path,
    removed: &mut Vec<PathBuf>,
    owned: &mut Vec<OwnedScope>,
) -> Result<()> {
    for entry in fs::read_dir(repos_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(repo_id) = name.to_str() else {
            continue;
        };
        if !validate_id(repo_id).is_ok() || !entry.path().is_dir() {
            continue;
        }
        let worktrees_root = entry.path().join("worktrees");
        if worktrees_root.is_dir() {
            for scope_entry in fs::read_dir(&worktrees_root)? {
                let scope_entry = scope_entry?;
                let scope_name = scope_entry.file_name();
                let Some(worktree_id) = scope_name.to_str() else {
                    continue;
                };
                if !validate_id(worktree_id).is_ok() || !scope_entry.path().is_dir() {
                    continue;
                }
                let scope_dir = scope_entry.path();
                match crate::ownership::ScopeLock::acquire(&scope_dir, "purge") {
                    // Locked: remove the whole scope directory (the lock
                    // file goes with it; the guard's drop is a no-op).
                    Ok(_lock) => {
                        fs::remove_dir_all(&scope_dir)?;
                        removed.push(scope_dir);
                    }
                    // A live writer owns this scope: skip it and report it
                    // (once — the pre-sweep re-check meets it again).
                    Err(crate::ownership::OwnershipError::Busy { metadata }) => {
                        if !owned.iter().any(|skip| skip.path == scope_dir) {
                            owned.push(OwnedScope {
                                path: scope_dir,
                                owner: metadata,
                            });
                        }
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
        // Trim the now-possibly-empty repository scope directories.
        let _ = fs::remove_dir(&worktrees_root);
        let _ = fs::remove_dir(entry.path());
    }
    Ok(())
}

/// Removes every scope the purge can lock, skipping owned scopes and
/// reporting them.
fn purge_all(cache: &CachePaths) -> Result<PurgeReport> {
    let repos_root = cache.root.join("repos");
    let mut removed = Vec::new();
    let mut owned = Vec::new();
    if repos_root.is_dir() {
        purge_scope_dirs(&repos_root, &mut removed, &mut owned)?;
        // Pre-sweep re-check: a writer that started during the purge may have
        // created a scope directory after the main pass; it is locked (and
        // therefore skipped) before its roots are swept.
        purge_scope_dirs(&repos_root, &mut removed, &mut owned)?;
        // Leftover repository-scoped data (embedding caches, host caches) and
        // the other cache roots. When owned scopes remain, their repository
        // directories are still in use and the shared stores may still be
        // referenced by the live writers' in-flight builds — leave them and
        // let the operator retry after the writers exit.
        if owned.is_empty() {
            for dir in [
                repos_root.clone(),
                cache.root.join("embedding-cache"),
                cache.root.join("families"),
            ] {
                if dir.exists() {
                    fs::remove_dir_all(&dir)?;
                    removed.push(dir);
                }
            }
        }
    } else {
        for dir in [
            cache.root.join("embedding-cache"),
            cache.root.join("families"),
        ] {
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
                removed.push(dir);
            }
        }
    }
    Ok(PurgeReport {
        removed,
        skipped_owned: if owned.is_empty() {
            None
        } else {
            Some(OwnedScopePurgeReport { owned })
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;
    use std::fs::File;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};
    use std::thread;

    /// The scope pass locks before touching anything: an owned scope directory
    /// is recorded and left intact, a free one is removed, and an owned scope
    /// seen in the main pass and the pre-sweep re-check is reported once.
    #[test]
    fn scope_pass_locks_before_removal_and_dedupes_rechecks() {
        let root =
            std::env::temp_dir().join(format!("repoise-purge-pass-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("repos/aaa/worktrees/bbb")).unwrap();
        fs::create_dir_all(root.join("repos/aaa/worktrees/ccc")).unwrap();
        let repos_root = root.join("repos");
        let owned_scope = repos_root.join("aaa/worktrees/bbb");
        let lock_path = owned_scope.join("scope.lock");

        // A concurrent process holds the owned scope's lock (barrier-synchronized).
        let ready = Arc::new(Barrier::new(2));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let hold = Arc::clone(&ready);
        let held_path = lock_path.clone();
        let holder = thread::spawn(move || {
            let file = File::create(&held_path).unwrap();
            file.try_lock_exclusive().unwrap();
            hold.wait();
            let _ = release_rx.recv();
        });
        ready.wait();

        let mut removed = Vec::new();
        let mut owned = Vec::new();
        purge_scope_dirs(&repos_root, &mut removed, &mut owned).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].ends_with("ccc"));
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].path, owned_scope);
        assert!(owned_scope.is_dir(), "an owned scope is never removed");

        // The pre-sweep re-check meets the same owned scope again: it is
        // reported once and still untouched.
        purge_scope_dirs(&repos_root, &mut removed, &mut owned).unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(removed.len(), 1);
        assert!(owned_scope.is_dir());

        release_tx.send(()).unwrap();
        holder.join().unwrap();
        // After the writer exits, the same scope is free and removable.
        let mut removed_after = Vec::new();
        let mut owned_after = Vec::new();
        purge_scope_dirs(&repos_root, &mut removed_after, &mut owned_after).unwrap();
        assert!(owned_after.is_empty());
        assert_eq!(removed_after.len(), 1);
        assert!(!owned_scope.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
