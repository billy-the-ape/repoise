//! Idempotent, dry-run capable, preset-driven `init` with a non-destructive
//! overlay. Init never replaces existing project files; it only creates its
//! own config, records an overlay manifest, and (when explicitly opted in)
//! appends a bounded managed marker block preserving surrounding bytes.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{EmbeddingConfig, Preset, SCHEMA_VERSION};
use crate::error::Error;
use crate::{CONFIG_FILENAME, OVERLAY_FILENAME, TEMPLATE_VERSION};

/// Options for `repoise init` (always non-interactive).
#[derive(Clone, Debug)]
pub struct InitOptions {
    /// Corpus preset to configure.
    pub preset: Preset,
    /// When true, callers must not apply the plan (dry run).
    pub dry_run: bool,
    /// Explicit confirmation; the CLI never prompts, this only documents intent.
    pub yes: bool,
    /// Provider for the hybrid preset (required for hybrid).
    pub provider: Option<String>,
    /// Opt-in target file for the managed marker block.
    pub adopt_managed_block: Option<PathBuf>,
}

/// What init will do to one managed file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    /// File will be created.
    Create,
    /// File already has exactly the managed content.
    Unchanged,
    /// File exists with different content; init will not touch it.
    Conflict,
}

/// One managed file in the init plan.
#[derive(Clone, Debug)]
pub struct PlannedFile {
    /// Relative path from the root.
    pub relative: PathBuf,
    /// Planned action.
    pub action: FileAction,
    /// Content init would write (also current content when unchanged).
    pub content: String,
}

/// Manifest of files managed by init; pins tool and template versions
/// separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayManifest {
    /// Repoise tool version that wrote the overlay.
    pub tool_version: String,
    /// Template version of the managed content.
    pub template_version: String,
    /// Managed files, relative paths.
    pub files: Vec<String>,
}

/// The full init plan (read-only until applied).
#[derive(Clone, Debug)]
pub struct InitPlan {
    /// Canonical root.
    pub root: PathBuf,
    /// Managed files with planned actions.
    pub files: Vec<PlannedFile>,
    /// Overlay manifest to write.
    pub manifest: OverlayManifest,
}

/// Result of applying a plan.
#[derive(Clone, Debug, Default)]
pub struct InitOutcome {
    /// Files created.
    pub created: Vec<PathBuf>,
    /// Files already managed and unchanged.
    pub unchanged: Vec<PathBuf>,
    /// Files init refused to touch (reported, not modified).
    pub conflicts: Vec<PathBuf>,
}

/// Text of the bounded managed marker block (template version pinned).
pub fn managed_block() -> String {
    format!(
        "<!-- repoise:managed begin template=\"{template}\" -->\n\
         Repoise is configured for this repository; run `repoise doctor` to inspect effective settings.\n\
         <!-- repoise:managed end -->",
        template = TEMPLATE_VERSION
    )
}

/// Renders the `repoise.config.json` content for the options (deterministic).
pub fn render_config(opts: &InitOptions) -> Result<String, Error> {
    if opts.preset.requires_provider() && opts.provider.as_deref().is_none_or(str::is_empty) {
        return Err(Error::Init(
            "hybrid preset requires an explicit --provider value".into(),
        ));
    }
    let mut obj = serde_json::Map::new();
    obj.insert("schemaVersion".into(), serde_json::json!(SCHEMA_VERSION));
    obj.insert("preset".into(), serde_json::json!(opts.preset.name()));
    obj.insert("inheritGitIgnores".into(), serde_json::json!(true));
    if let Some(provider) = opts.provider.as_deref().filter(|p| !p.is_empty()) {
        let embedding: EmbeddingConfig = EmbeddingConfig {
            provider: Some(provider.to_string()),
            endpoint: None,
            model: None,
            dimensions: None,
            api_key_env: None,
            batch_size: None,
            timeout_ms: None,
            max_retries: None,
            max_requests_per_build: None,
            max_input_chars_per_build: None,
        };
        obj.insert("embedding".into(), serde_json::to_value(&embedding)?);
    }
    let pretty = serde_json::to_string_pretty(&serde_json::Value::Object(obj))?;
    Ok(format!("{pretty}\n"))
}
/// Builds the init plan without touching the filesystem (dry-run safe).
pub fn plan(root: &Path, opts: &InitOptions) -> Result<InitPlan, Error> {
    let root = root.canonicalize()?;
    let _ = opts.yes;
    let config_content = render_config(opts)?;

    let mut files: Vec<PlannedFile> = Vec::new();

    // 1. Committed config: never replaced; identical content is idempotent.
    let config_path = root.join(CONFIG_FILENAME);
    let config_action = match fs::read_to_string(&config_path) {
        Ok(existing) if existing == config_content => FileAction::Unchanged,
        Ok(_) => FileAction::Conflict,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => FileAction::Create,
        Err(err) => return Err(Error::Io(err)),
    };
    files.push(PlannedFile {
        relative: PathBuf::from(CONFIG_FILENAME),
        action: config_action,
        content: config_content.clone(),
    });

    // 2. Opt-in managed marker block in an owner-selected file.
    if let Some(target) = &opts.adopt_managed_block {
        if !target.is_relative() {
            return Err(Error::Init(format!(
                "--adopt-managed-block must be a relative path: {}",
                target.display()
            )));
        }
        let block = managed_block();
        let target_path = root.join(target);
        match fs::read_to_string(&target_path) {
            Ok(existing) => {
                let start = existing.find("<!-- repoise:managed begin");
                let end_marker = "<!-- repoise:managed end -->";
                if let (Some(start), Some(end_offset)) = (
                    start,
                    start.and_then(|start| {
                        existing[start..]
                            .find(end_marker)
                            .map(|offset| start + offset + end_marker.len())
                    }),
                ) {
                    let current = &existing[start..end_offset];
                    let action = if current == block {
                        FileAction::Unchanged
                    } else {
                        FileAction::Conflict
                    };
                    files.push(PlannedFile {
                        relative: target.clone(),
                        action,
                        content: existing,
                    });
                } else if start.is_some() {
                    files.push(PlannedFile {
                        relative: target.clone(),
                        action: FileAction::Conflict,
                        content: existing,
                    });
                } else {
                    // Append, preserving surrounding bytes exactly.
                    let base = if existing.ends_with('\n') {
                        existing
                    } else {
                        format!("{existing}\n")
                    };
                    files.push(PlannedFile {
                        relative: target.clone(),
                        action: FileAction::Create,
                        content: format!("{base}{block}\n"),
                    });
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                files.push(PlannedFile {
                    relative: target.clone(),
                    action: FileAction::Create,
                    content: format!("{block}\n"),
                });
            }
            Err(err) => return Err(Error::Io(err)),
        }
    }
    // 3. Overlay manifest (managed by init; pins tool + template versions).
    let mut manifest_files: Vec<String> = vec![CONFIG_FILENAME.to_string()];
    if let Some(target) = &opts.adopt_managed_block {
        manifest_files.push(target.to_string_lossy().into_owned());
    }
    manifest_files.sort();
    manifest_files.dedup();
    let manifest = OverlayManifest {
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        template_version: TEMPLATE_VERSION.to_string(),
        files: manifest_files,
    };
    let manifest_path = root.join(OVERLAY_FILENAME);
    let manifest_action = match fs::read_to_string(&manifest_path) {
        Ok(existing) => {
            let parsed: OverlayManifest = serde_json::from_str(&existing)
                .map_err(|err| Error::Init(format!("unparsable overlay manifest: {err}")))?;
            if parsed == manifest {
                FileAction::Unchanged
            } else {
                FileAction::Conflict
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => FileAction::Create,
        Err(err) => return Err(Error::Io(err)),
    };
    files.push(PlannedFile {
        relative: PathBuf::from(OVERLAY_FILENAME),
        action: manifest_action,
        content: serde_json::to_string_pretty(&manifest)? + "\n",
    });

    Ok(InitPlan {
        root,
        files,
        manifest,
    })
}

/// Applies a plan; conflicts are reported, never overwritten.
pub fn apply(plan: &InitPlan) -> Result<InitOutcome, Error> {
    let mut outcome = InitOutcome::default();
    for file in &plan.files {
        match file.action {
            FileAction::Create => {
                fs::write(plan.root.join(&file.relative), &file.content)?;
                outcome.created.push(file.relative.clone());
            }
            FileAction::Unchanged => {
                outcome.unchanged.push(file.relative.clone());
            }
            FileAction::Conflict => {
                outcome.conflicts.push(file.relative.clone());
            }
        }
    }
    Ok(outcome)
}
