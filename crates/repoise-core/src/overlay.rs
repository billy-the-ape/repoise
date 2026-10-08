//! Non-destructive overlay lifecycle (card K6): `uninstall` and `update`.
//!
//! The overlay manifest (`repoise.overlay.json`) records the exact managed
//! bytes (file content or block text) at install time. Both operations use
//! that pinned baseline so they only ever touch bytes the overlay itself
//! owns, never owner-modified content:
//!
//! - `uninstall` removes whole managed files and managed marker blocks
//!   (deleting a file only when the block was its entire content); any
//!   modified managed byte is reported as a conflict and left untouched.
//!   The manifest is removed last, only when every entry was removable.
//! - `update` performs a 3-way merge per managed entry (installed baseline,
//!   current file, fresh template render). Only the clean side is advanced;
//!   divergent edits are conflicts. Re-running a versioned tool never
//!   silently rewrites owner content.
//!
//! Both are dry-run capable and never prompt.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::OVERLAY_FILENAME;
use crate::config::Preset;
use crate::error::Error;
use crate::init::{
    OverlayFileEntry, OverlayFileKind, OverlayManifest, OverlayRole, agents_snippet_block,
    managed_block, render_config,
};

/// One conflict found during overlay lifecycle work.
#[derive(Clone, Debug, Serialize)]
pub struct OverlayConflict {
    /// Relative path of the managed entry.
    pub path: String,
    /// Stable conflict reason.
    pub reason: String,
}

/// Outcome of `repoise overlay uninstall`.
#[derive(Clone, Debug, Serialize)]
pub struct UninstallOutcome {
    /// Managed files removed (or that would be removed in a dry run).
    pub removed_files: Vec<String>,
    /// Managed blocks removed (or that would be removed in a dry run).
    pub removed_blocks: Vec<String>,
    /// Entries init refused to touch (owner-modified).
    pub conflicts: Vec<OverlayConflict>,
    /// The overlay manifest itself.
    pub manifest_removed: bool,
    /// True when nothing was written.
    pub dry_run: bool,
}

/// Outcome of `repoise overlay update`.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateOutcome {
    /// Entries advanced to the fresh template.
    pub updated: Vec<String>,
    /// Entries already at the fresh template.
    pub unchanged: Vec<String>,
    /// Entries the update refused to touch (divergent owner edits).
    pub conflicts: Vec<OverlayConflict>,
    /// The overlay manifest itself.
    pub manifest_updated: bool,
    /// True when nothing was written.
    pub dry_run: bool,
}

/// Loads and validates the overlay manifest at `root`.
pub fn load_manifest(root: &Path) -> Result<OverlayManifest, Error> {
    let raw = fs::read_to_string(root.join(OVERLAY_FILENAME)).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::Init(format!(
                "no overlay manifest at {} (was `repoise init` run?)",
                OVERLAY_FILENAME
            ))
        } else {
            Error::Io(err)
        }
    })?;
    serde_json::from_str(&raw)
        .map_err(|err| Error::Init(format!("unparsable overlay manifest: {err}")))
}

/// Finds the managed block region in a file: (start, end) byte offsets.
fn managed_block_region(file: &str) -> Option<(usize, usize)> {
    let start = file.find("<!-- repoise:managed begin")?;
    let end_marker = "<!-- repoise:managed end -->";
    let end = file[start..].find(end_marker)? + start + end_marker.len();
    Some((start, end))
}

/// The exact bytes the overlay expects for one entry now (fresh template).
fn fresh_content(entry: &OverlayFileEntry, manifest: &OverlayManifest) -> Option<String> {
    let preset_name = manifest.preset.as_deref().unwrap_or(Preset::DEFAULT.name());
    match entry.role {
        Some(OverlayRole::Config) => {
            let preset = Preset::parse(preset_name)?;
            let opts = crate::init::InitOptions {
                preset,
                dry_run: true,
                yes: true,
                provider: manifest.provider.clone(),
                adopt_managed_block: None,
                agents_snippet: false,
            };
            // Config rendering must be deterministic; a hybrid preset without
            // a recorded provider cannot be re-rendered safely.
            render_config(&opts).ok()
        }
        Some(OverlayRole::ManagedBlock) => Some(managed_block()),
        Some(OverlayRole::AgentsSnippet) => Some(agents_snippet_block()),
        None => None,
    }
}
/// Removes the overlay (dry-run capable). Only bytes the overlay installed
/// are removed; anything else is a conflict. The manifest is removed last.
pub fn uninstall(root: &Path, dry_run: bool) -> Result<UninstallOutcome, Error> {
    let manifest = load_manifest(root)?;
    let mut outcome = UninstallOutcome {
        removed_files: Vec::new(),
        removed_blocks: Vec::new(),
        conflicts: Vec::new(),
        manifest_removed: false,
        dry_run,
    };

    for entry in &manifest.files {
        let target = root.join(&entry.path);
        match fs::read_to_string(&target) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Already gone; nothing to remove, not a conflict.
                continue;
            }
            Err(err) => return Err(Error::Io(err)),
            Ok(current) => match entry.kind {
                OverlayFileKind::File => {
                    let unchanged = entry.installed.as_deref() == Some(current.as_str());
                    if !unchanged {
                        outcome.conflicts.push(OverlayConflict {
                            path: entry.path.clone(),
                            reason: "modified since init; refusing to remove owner content"
                                .to_string(),
                        });
                        continue;
                    }
                    if !dry_run {
                        fs::remove_file(&target)?;
                    }
                    outcome.removed_files.push(entry.path.clone());
                }
                OverlayFileKind::Block => {
                    let Some((start, end)) = managed_block_region(&current) else {
                        outcome.conflicts.push(OverlayConflict {
                            path: entry.path.clone(),
                            reason: "managed block not found; refusing to guess".to_string(),
                        });
                        continue;
                    };
                    let block = &current[start..end];
                    let unchanged = entry.installed.as_deref() == Some(block);
                    if !unchanged {
                        outcome.conflicts.push(OverlayConflict {
                            path: entry.path.clone(),
                            reason: "managed block modified since init".to_string(),
                        });
                        continue;
                    }
                    if !dry_run {
                        let trimmed = format!("{}{}", &current[..start], &current[end..]);
                        if trimmed.trim().is_empty() {
                            fs::remove_file(&target)?;
                        } else {
                            fs::write(&target, trimmed)?;
                        }
                    }
                    outcome.removed_blocks.push(entry.path.clone());
                }
            },
        }
    }

    let all_removable = outcome.conflicts.is_empty();
    if all_removable && !dry_run {
        fs::remove_file(root.join(OVERLAY_FILENAME))?;
    }
    outcome.manifest_removed = all_removable && !dry_run;
    Ok(outcome)
}

/// Updates the overlay to the current template versions (dry-run capable),
/// using a 3-way merge against the installed baseline.
pub fn update(root: &Path, dry_run: bool) -> Result<UpdateOutcome, Error> {
    let manifest = load_manifest(root)?;
    let mut outcome = UpdateOutcome {
        updated: Vec::new(),
        unchanged: Vec::new(),
        conflicts: Vec::new(),
        manifest_updated: false,
        dry_run,
    };

    for entry in &manifest.files {
        let Some(fresh) = fresh_content(entry, &manifest) else {
            outcome.conflicts.push(OverlayConflict {
                path: entry.path.clone(),
                reason: "pre-v2 manifest entry without install baseline; \
                        re-run `repoise init`"
                    .to_string(),
            });
            continue;
        };
        let target = root.join(&entry.path);
        let Ok(current) = fs::read_to_string(&target) else {
            outcome.conflicts.push(OverlayConflict {
                path: entry.path.clone(),
                reason: "managed file missing; re-run `repoise init`".to_string(),
            });
            continue;
        };
        match entry.kind {
            OverlayFileKind::File => {
                let base = entry.installed.as_deref().unwrap_or("");
                if current == fresh {
                    outcome.unchanged.push(entry.path.clone());
                } else if current == base {
                    if !dry_run {
                        fs::write(&target, &fresh)?;
                    }
                    outcome.updated.push(entry.path.clone());
                } else {
                    outcome.conflicts.push(OverlayConflict {
                        path: entry.path.clone(),
                        reason: "modified since init; refusing to touch owner content".to_string(),
                    });
                }
            }
            OverlayFileKind::Block => {
                let Some((start, end)) = managed_block_region(&current) else {
                    outcome.conflicts.push(OverlayConflict {
                        path: entry.path.clone(),
                        reason: "managed block not found; re-run `repoise init`".to_string(),
                    });
                    continue;
                };
                let block = &current[start..end];
                let base = entry.installed.as_deref().unwrap_or("");
                if block == fresh {
                    outcome.unchanged.push(entry.path.clone());
                } else if block == base {
                    if !dry_run {
                        let next = format!("{}{}{}", &current[..start], fresh, &current[end..]);
                        fs::write(&target, next)?;
                    }
                    outcome.updated.push(entry.path.clone());
                } else {
                    outcome.conflicts.push(OverlayConflict {
                        path: entry.path.clone(),
                        reason: "managed block modified since init".to_string(),
                    });
                }
            }
        }
    }

    // Rewrite the manifest with the advanced entries' new baseline only when
    // nothing conflicted and at least one entry advanced.
    if !outcome.updated.is_empty() && outcome.conflicts.is_empty() && !dry_run {
        let mut new_manifest = manifest.clone();
        for entry in &mut new_manifest.files {
            if outcome.updated.iter().any(|path| path == &entry.path)
                && let Some(fresh) = fresh_content(entry, &manifest)
            {
                entry.installed = Some(fresh);
            }
        }
        fs::write(
            root.join(OVERLAY_FILENAME),
            serde_json::to_string_pretty(&new_manifest)? + "\n",
        )?;
        outcome.manifest_updated = true;
    }
    Ok(outcome)
}
