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
use std::path::PathBuf;

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
/// scopes are skipped and reported in `skipped_owned`.
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

/// Removes every scope the purge can lock, skipping owned scopes and
/// reporting them.
fn purge_all(cache: &CachePaths) -> Result<PurgeReport> {
    let repos_root = cache.root.join("repos");
    let mut removed = Vec::new();
    let mut owned = Vec::new();
    if repos_root.is_dir() {
        for entry in fs::read_dir(&repos_root)? {
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
                        // A live writer owns this scope: skip it and report it.
                        Err(crate::ownership::OwnershipError::Busy { metadata }) => {
                            owned.push(OwnedScope {
                                path: scope_dir,
                                owner: metadata,
                            });
                        }
                        Err(err) => return Err(err.into()),
                    }
                }
            }
            // Trim the now-possibly-empty repository scope directories.
            let _ = fs::remove_dir(&worktrees_root);
            let _ = fs::remove_dir(entry.path());
        }
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
