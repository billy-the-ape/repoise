//! Cache purging for the relocatable cache (master plan section 11).
//!
//! `repoise purge` removes generated cache data only — never source files.
//! Scope ids are validated before any path is touched, and the removal unit
//! is the whole scope directory (database plus WAL/SHM sidecars).

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

/// Report of what was removed.
#[derive(Clone, Debug, Serialize)]
pub struct PurgeReport {
    /// Paths removed.
    pub removed: Vec<PathBuf>,
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
pub fn purge(cache: &CachePaths, request: &PurgeRequest) -> Result<PurgeReport> {
    let mut removed = Vec::new();
    if request.all {
        for dir in [cache.root.join("repos"), cache.root.join("embedding-cache")] {
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
                removed.push(dir);
            }
        }
        return Ok(PurgeReport { removed });
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
    fs::remove_dir_all(&dir)?;
    removed.push(dir);
    // Trim empty parents (keep the layout).
    let _ = fs::remove_dir(cache.repo_dir(repo_id).join("worktrees"));
    Ok(PurgeReport { removed })
}
