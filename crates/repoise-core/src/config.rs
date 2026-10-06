//! Versioned `repoise.config.json` configuration (master plan section 10).
//!
//! Precedence: CLI flags > local overrides (`repoise.local.json`, untracked)
//! > committed config > presets > defaults. Unknown keys are rejected. Local
//! > overrides may never bypass mandatory containment or secret policy.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::classify::{self, RoleRule};
use crate::error::Error;
use crate::hash;
use crate::{DEFAULT_CACHE_DIR, DEFAULT_MAX_FILE_BYTES};

/// Published configuration schema version accepted by this build.
pub const SCHEMA_VERSION: u32 = 1;

/// Committed (and local) configuration document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Schema version; must equal `SCHEMA_VERSION` when present.
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Preset name: `docs-only`, `docs-code-lexical`, or `hybrid`.
    #[serde(default)]
    pub preset: Option<String>,
    /// Whether native Git ignore files are inherited (default true).
    #[serde(default)]
    pub inherit_git_ignores: Option<bool>,
    /// Extra include patterns (narrow re-includes; never secret bypasses).
    #[serde(default)]
    pub include: Vec<String>,
    /// Extra exclude patterns.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Maximum file bytes accepted into the corpus (default 1 MiB).
    #[serde(default)]
    pub max_file_bytes: Option<u64>,
    /// Configured role mappings (pattern -> role).
    #[serde(default)]
    pub document_roles: Vec<RoleRule>,
    /// Generated-data directory (kept out of the corpus by default).
    #[serde(default)]
    pub cache_dir: Option<String>,
    /// Optional embedding settings; secrets only via environment references.
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,
}

/// Optional embedding provider settings (operator-selected, never mandatory).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EmbeddingConfig {
    /// Provider name chosen by the operator.
    #[serde(default)]
    pub provider: Option<String>,
    /// Endpoint URL reference; must be an `env:` reference when present.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Model identifier.
    #[serde(default)]
    pub model: Option<String>,
}

/// Corpus preset selected by `init` or configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Documentation and configuration only; code excluded.
    DocsOnly,
    /// Docs/config plus code symbol/lexical indexing, fully offline (default).
    DocsCodeLexical,
    /// Docs/code plus embedding search; requires an explicit provider.
    Hybrid,
}

impl Preset {
    /// Default preset (offline docs+code lexical).
    pub const DEFAULT: Preset = Preset::DocsCodeLexical;

    /// Parses a preset name; `None` when unknown.
    pub fn parse(value: &str) -> Option<Preset> {
        match value {
            "docs-only" => Some(Preset::DocsOnly),
            "docs-code-lexical" => Some(Preset::DocsCodeLexical),
            "hybrid" => Some(Preset::Hybrid),
            _ => None,
        }
    }

    /// Canonical preset name.
    pub fn name(self) -> &'static str {
        match self {
            Preset::DocsOnly => "docs-only",
            Preset::DocsCodeLexical => "docs-code-lexical",
            Preset::Hybrid => "hybrid",
        }
    }

    /// Whether this preset requires an explicitly configured provider.
    pub fn requires_provider(self) -> bool {
        self == Preset::Hybrid
    }
}

/// Where an effective value came from (shown by `doctor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Built-in default.
    Default,
    /// Contributed by the selected preset.
    Preset,
    /// Committed `repoise.config.json`.
    Committed,
    /// Untracked local override file.
    Local,
    /// Command-line flag.
    Cli,
}

/// CLI-level overrides (highest precedence).
#[derive(Clone, Debug, Default)]
pub struct CliOverrides {
    /// Preset override.
    pub preset: Option<Preset>,
    /// Extra include patterns.
    pub include: Vec<String>,
    /// Extra exclude patterns.
    pub exclude: Vec<String>,
    /// Max file bytes override.
    pub max_file_bytes: Option<u64>,
}

/// Optional explicit config file locations (CLI overrides).
#[derive(Clone, Debug, Default)]
pub struct ConfigPaths {
    /// Committed config path override; default `repoise.config.json`.
    pub committed: Option<PathBuf>,
    /// Local override path; default `repoise.local.json`.
    pub local: Option<PathBuf>,
}
/// Effective configuration after precedence resolution, with origins.
#[derive(Clone, Debug, Serialize)]
pub struct EffectiveConfig {
    /// Selected preset.
    pub preset: Preset,
    /// Where the preset came from.
    pub preset_origin: Origin,
    /// Whether native Git ignores are inherited.
    pub inherit_git_ignores: bool,
    /// Include patterns with origins.
    pub include: Vec<(String, Origin)>,
    /// Exclude patterns with origins.
    pub exclude: Vec<(String, Origin)>,
    /// Maximum accepted file size in bytes.
    pub max_file_bytes: u64,
    /// Configured role rules with origins.
    pub document_roles: Vec<(RoleRule, Origin)>,
    /// Generated-data directory.
    pub cache_dir: String,
    /// Optional embedding settings.
    pub embedding: Option<EmbeddingConfig>,
}

impl EffectiveConfig {
    /// Loads committed and local config for `root` and resolves precedence.
    pub fn resolve(root: &Path, cli: &CliOverrides) -> Result<EffectiveConfig, Error> {
        Self::resolve_with(root, &ConfigPaths::default(), cli)
    }

    /// Resolves with explicit config file overrides (default names otherwise).
    pub fn resolve_with(
        root: &Path,
        paths: &ConfigPaths,
        cli: &CliOverrides,
    ) -> Result<EffectiveConfig, Error> {
        let committed = paths
            .committed
            .clone()
            .unwrap_or_else(|| root.join(crate::CONFIG_FILENAME));
        let local = paths
            .local
            .clone()
            .unwrap_or_else(|| root.join(crate::LOCAL_CONFIG_FILENAME));
        Self::resolve_files(&committed, &local, cli)
    }

    /// Resolves two explicit config file paths (missing files are fine).
    pub fn resolve_files(
        committed: &Path,
        local: &Path,
        cli: &CliOverrides,
    ) -> Result<EffectiveConfig, Error> {
        let committed = load(committed)?;
        let local = load(local)?;
        Self::resolve_layers(committed.as_ref(), local.as_ref(), cli)
    }

    /// Resolves precedence: defaults < committed < local < CLI.
    pub fn resolve_layers(
        committed: Option<&Config>,
        local: Option<&Config>,
        cli: &CliOverrides,
    ) -> Result<EffectiveConfig, Error> {
        let mut preset = Preset::DEFAULT;
        let mut preset_origin = Origin::Default;
        let mut inherit = true;
        let mut include: Vec<(String, Origin)> = Vec::new();
        let mut exclude: Vec<(String, Origin)> = Vec::new();
        let mut max_file_bytes = DEFAULT_MAX_FILE_BYTES;
        let mut document_roles: Vec<(RoleRule, Origin)> = Vec::new();
        let mut cache_dir = DEFAULT_CACHE_DIR.to_string();
        let mut embedding: Option<EmbeddingConfig> = None;

        for (origin, config) in [(Origin::Committed, committed), (Origin::Local, local)] {
            if let Some(config) = config {
                if let Some(name) = &config.preset {
                    preset = Preset::parse(name)
                        .ok_or_else(|| Error::Config(format!("unknown preset: {name}")))?;
                    preset_origin = origin;
                }
                if let Some(flag) = config.inherit_git_ignores {
                    inherit = flag;
                }
                for pattern in &config.exclude {
                    exclude.push((pattern.clone(), origin));
                }
                for pattern in &config.include {
                    include.push((pattern.clone(), origin));
                }
                if let Some(limit) = config.max_file_bytes {
                    max_file_bytes = limit;
                }
                for rule in &config.document_roles {
                    document_roles.push((rule.clone(), origin));
                }
                if let Some(dir) = &config.cache_dir {
                    cache_dir = dir.clone();
                }
                if let Some(settings) = &config.embedding {
                    embedding = Some(settings.clone());
                }
            }
        }

        if let Some(preset_cli) = cli.preset {
            preset = preset_cli;
            preset_origin = Origin::Cli;
        }
        for pattern in &cli.include {
            include.push((pattern.clone(), Origin::Cli));
        }
        for pattern in &cli.exclude {
            exclude.push((pattern.clone(), Origin::Cli));
        }
        if let Some(limit) = cli.max_file_bytes {
            max_file_bytes = limit;
        }

        // The docs-only preset keeps the corpus docs/config-only.
        if preset == Preset::DocsOnly {
            for pattern in classify::code_extension_globs() {
                exclude.push((pattern, Origin::Preset));
            }
        }

        Ok(EffectiveConfig {
            preset,
            preset_origin,
            inherit_git_ignores: inherit,
            include,
            exclude,
            max_file_bytes,
            document_roles,
            cache_dir,
            embedding,
        })
    }
}
impl EffectiveConfig {
    /// Validation diagnostics; empty means the config is usable.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.max_file_bytes == 0 {
            problems.push("maxFileBytes must be greater than 0".into());
        }
        for (pattern, _) in self.include.iter().chain(self.exclude.iter()) {
            if crate::ignore::parse_pattern(pattern, Path::new("")).is_err() {
                problems.push(format!("invalid pattern: {pattern}"));
            }
        }
        if let Some(settings) = &self.embedding
            && let Some(endpoint) = &settings.endpoint
            && !endpoint.starts_with("env:")
        {
            problems
                .push("embedding.endpoint must be an environment reference like env:VAR".into());
        }
        problems
    }

    /// Stable fingerprint of the effective policy; config/ignore changes
    /// invalidate stored inventories.
    pub fn fingerprint(&self) -> String {
        let mut key = String::new();
        key.push_str(self.preset.name());
        key.push('\n');
        key.push_str(&self.inherit_git_ignores.to_string());
        key.push('\n');
        for (pattern, origin) in &self.include {
            key.push_str(&format!("include|{}|{origin:?}\n", pattern));
        }
        for (pattern, origin) in &self.exclude {
            key.push_str(&format!("exclude|{}|{origin:?}\n", pattern));
        }
        for (rule, origin) in &self.document_roles {
            key.push_str(&format!(
                "role|{}|{}|{origin:?}\n",
                rule.pattern,
                rule.role.name()
            ));
        }
        key.push_str(&self.max_file_bytes.to_string());
        key.push('\n');
        key.push_str(&self.cache_dir);
        hash::sha256_hex(key)
    }
}

/// Loads one config file; `Ok(None)` when absent.
pub fn load(path: &Path) -> Result<Option<Config>, Error> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::Io(err)),
    };
    let config: Config = serde_json::from_str(&text)
        .map_err(|err| Error::Config(format!("{}: {err}", path.display())))?;
    if let Some(version) = config.schema_version
        && version != SCHEMA_VERSION
    {
        return Err(Error::Config(format!(
            "{}: unsupported schemaVersion {version}; this build requires {SCHEMA_VERSION}",
            path.display()
        )));
    }
    Ok(Some(config))
}
