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
    /// Optional search settings (hybrid fusion parameters).
    #[serde(default)]
    pub search: Option<SearchConfig>,
}

/// Optional embedding provider settings (operator-selected, never mandatory).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EmbeddingConfig {
    /// Provider name chosen by the operator.
    #[serde(default)]
    pub provider: Option<String>,
    /// Which chunks are embedded: `docs` (default) or `docs+code`.
    #[serde(default)]
    pub scope: Option<String>,
    /// Endpoint URL reference; must be an `env:` reference when present.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Model identifier (provider/model revision).
    #[serde(default)]
    pub model: Option<String>,
    /// Vector dimension the model produces.
    #[serde(default)]
    pub dimensions: Option<u32>,
    /// Environment variable name holding the API key (never the secret).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Maximum inputs per provider batch (default 32).
    #[serde(default)]
    pub batch_size: Option<u32>,
    /// Per-batch request timeout in milliseconds (default 30000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Retries after the first attempt for a failed batch (default 2).
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// Maximum provider batches per build (default 500).
    #[serde(default)]
    pub max_requests_per_build: Option<u32>,
    /// Maximum input characters accepted per build (default 1000000).
    #[serde(default)]
    pub max_input_chars_per_build: Option<u64>,
}

/// Optional search settings for hybrid retrieval.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SearchConfig {
    /// Reciprocal-rank-fusion `k` (default 60).
    #[serde(default)]
    pub rrf_k: Option<u32>,
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
    /// Which chunks the enabled embedding profile applies to (default docs).
    pub embedding_scope: crate::embed::EmbeddingScope,
    /// Resolved reciprocal-rank-fusion `k` (default 60).
    pub rrf_k: u32,
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
        let mut rrf_k = crate::embed::DEFAULT_RRF_K;

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
                if let Some(search) = &config.search
                    && let Some(k) = search.rrf_k.filter(|k| *k > 0)
                {
                    rrf_k = k;
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

        let embedding_scope = match embedding
            .as_ref()
            .and_then(|settings| settings.scope.as_deref())
        {
            Some(name) => crate::embed::EmbeddingScope::parse(name)
                .ok_or_else(|| Error::Config(format!("unknown embedding scope: {name}")))?,
            None => crate::embed::EmbeddingScope::Docs,
        };

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
            embedding_scope,
            rrf_k,
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
        match &self.embedding {
            Some(settings) => {
                let provider = settings.provider.as_deref().unwrap_or("");
                if provider.is_empty() {
                    problems
                        .push("embedding.provider is required when embedding is configured".into());
                } else if !known_provider_names().contains(&provider) {
                    problems.push(format!(
                        "no built-in embedding transport for provider '{provider}' in this build \
                         (the local inference preset is evaluated, not shipped); lexical operation is unaffected"
                    ));
                }
                if self.preset == Preset::Hybrid {
                    if settings.model.as_deref().unwrap_or("").is_empty() {
                        problems.push("embedding.model is required for the hybrid preset".into());
                    }
                    if settings.dimensions.filter(|d| *d > 0).is_none() {
                        problems.push(
                            "embedding.dimensions must be a positive integer for the hybrid preset"
                                .into(),
                        );
                    }
                }
                if let Some(name) = &settings.api_key_env
                    && !name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                {
                    problems.push(format!(
                        "embedding.apiKeyEnv must be a valid environment variable name: {name}"
                    ));
                }
                if settings.batch_size.is_some_and(|v| v == 0) {
                    problems.push("embedding.batchSize must be greater than 0".into());
                }
                if settings.max_retries.is_some_and(|v| v == 0) {
                    problems.push("embedding.maxRetries must be greater than 0".into());
                }
                if settings.max_requests_per_build.is_some_and(|v| v == 0) {
                    problems.push("embedding.maxRequestsPerBuild must be greater than 0".into());
                }
                if settings.max_input_chars_per_build.is_some_and(|v| v == 0) {
                    problems.push("embedding.maxInputCharsPerBuild must be greater than 0".into());
                }
            }
            None if self.preset == Preset::Hybrid => {
                problems.push(
                    "the hybrid preset requires embedding settings (provider, model, dimensions)"
                        .into(),
                );
            }
            None => {}
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
        key.push('\n');
        key.push_str(&format!(
            "embeddingScope|{}\n",
            self.embedding_scope.as_str()
        ));
        if let Some(settings) = &self.embedding {
            key.push('\n');
            key.push_str(&format!(
                "embed|{}|{}|{:?}|{}|{}|{}|{}|{}|{}\n",
                settings.provider.as_deref().unwrap_or(""),
                settings.model.as_deref().unwrap_or(""),
                settings.dimensions,
                settings.api_key_env.as_deref().unwrap_or(""),
                settings.batch_size.unwrap_or(0),
                settings.timeout_ms.unwrap_or(0),
                settings.max_retries.unwrap_or(0),
                settings.max_requests_per_build.unwrap_or(0),
                settings.max_input_chars_per_build.unwrap_or(0),
            ));
        }
        key.push_str(&self.rrf_k.to_string());
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

/// Provider names with a built-in transport in this build.
fn known_provider_names() -> &'static [&'static str] {
    &["openai-compatible"]
}

/// Builds the embedding profile from valid settings, or `None` when the
/// settings cannot form one (missing model/dimensions, unknown provider).
pub fn profile_from_config(settings: &EmbeddingConfig) -> Option<crate::embed::EmbeddingProfile> {
    let provider = settings.provider.as_deref()?;
    if !known_provider_names().contains(&provider) {
        return None;
    }
    let model = settings.model.as_deref()?.trim();
    if model.is_empty() {
        return None;
    }
    let dimension = settings.dimensions?;
    if dimension == 0 {
        return None;
    }
    Some(crate::embed::EmbeddingProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        dimension,
        profile_version: crate::embed::PROFILE_VERSION,
    })
}

/// Builds the effective embedding budgets from settings (defaults fill gaps).
pub fn budgets_from_config(settings: &EmbeddingConfig) -> crate::embed::EmbeddingBudgets {
    let defaults = crate::embed::EmbeddingBudgets::default();
    crate::embed::EmbeddingBudgets {
        batch_size: settings
            .batch_size
            .filter(|v| *v > 0)
            .unwrap_or(defaults.batch_size),
        max_requests_per_build: settings
            .max_requests_per_build
            .filter(|v| *v > 0)
            .unwrap_or(defaults.max_requests_per_build),
        max_input_chars_per_build: settings
            .max_input_chars_per_build
            .filter(|v| *v > 0)
            .unwrap_or(defaults.max_input_chars_per_build),
        timeout: std::time::Duration::from_millis(
            settings
                .timeout_ms
                .filter(|v| *v > 0)
                .unwrap_or(defaults.timeout.as_millis() as u64),
        ),
        max_retries: settings
            .max_retries
            .filter(|v| *v > 0)
            .unwrap_or(defaults.max_retries),
        backoff_initial: defaults.backoff_initial,
        backoff_max: defaults.backoff_max,
    }
}
