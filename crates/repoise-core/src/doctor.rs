//! `doctor`: explainable effective settings, capabilities, and policy summary.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::adapter::{
    Capability, SnapshotMode, SourceAdapter, SourceKind, filesystem::FilesystemAdapter,
    git::GitAdapter,
};
use crate::config::{CliOverrides, ConfigPaths, EffectiveConfig};
use crate::discovery::native_ignore_files;
use crate::error::Error;
use crate::ignore::{Policy, RuleSource};
use crate::{CONFIG_FILENAME, LOCAL_CONFIG_FILENAME};

/// Summary of the effective ignore policy.
#[derive(Clone, Debug, Serialize)]
pub struct PolicySummary {
    /// Number of built-in package default rules.
    pub package_default_rules: usize,
    /// Number of mandatory secret deny rules.
    pub secret_rules: usize,
    /// Number of config/local include+exclude rules.
    pub config_rules: usize,
    /// Adapter-owned native ignore sources (relative labels).
    pub native_ignore_sources: Vec<PathBuf>,
    /// Number of native ignore rules.
    pub native_rules: usize,
    /// Fingerprint of the effective policy.
    pub fingerprint: String,
}

/// Full doctor report; `diagnostics` non-empty means doctor fails.
#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    /// Canonical repository root.
    pub root: PathBuf,
    /// Adapter kind selected for the root.
    pub adapter: SourceKind,
    /// Capabilities of the selected adapter.
    pub capabilities: Vec<Capability>,
    /// Snapshot mode the default operation uses.
    pub mode: SnapshotMode,
    /// Committed config file when present.
    pub config_file: Option<PathBuf>,
    /// Local override file when present.
    pub local_config_file: Option<PathBuf>,
    /// Sanitized remote identity for Git roots.
    pub remote_identity: Option<String>,
    /// Effective settings with origins.
    pub effective: EffectiveConfig,
    /// Policy summary.
    pub policy: PolicySummary,
    /// Fingerprint of the effective configuration.
    pub config_fingerprint: String,
    /// Validation problems; doctor exits non-empty when present.
    pub diagnostics: Vec<String>,
    /// Non-fatal advisories (doctor still succeeds).
    pub warnings: Vec<String>,
}

/// Runs doctor for the root directory.
pub fn doctor(root: &Path, cli: &CliOverrides, paths: &ConfigPaths) -> Result<DoctorReport, Error> {
    let root = root.canonicalize()?;
    let is_git = root.join(".git").exists();
    let (adapter, mode) = if is_git {
        (
            Box::new(GitAdapter::new(&root)?) as Box<dyn SourceAdapter>,
            SnapshotMode::WorkingTree,
        )
    } else {
        (
            Box::new(FilesystemAdapter::new(&root)?) as Box<dyn SourceAdapter>,
            SnapshotMode::PlainDirectory,
        )
    };
    let effective = EffectiveConfig::resolve_with(&root, paths, cli)?;
    let diagnostics = effective.validate();
    let native = native_ignore_files(adapter.as_ref(), mode, &effective)?;
    let policy = Policy::build(&effective, &native)?;
    let (package_default_rules, secret_rules, config_rules, native_rules) =
        policy.rules().iter().fold((0, 0, 0, 0), |counts, rule| {
            let (a, b, c, d) = counts;
            match rule.source {
                RuleSource::PackageDefault => (a + 1, b, c, d),
                RuleSource::SecretDeny => (a, b + 1, c, d),
                RuleSource::ConfigCommitted | RuleSource::ConfigLocal => (a, b, c + 1, d),
                RuleSource::AdapterNative(_) => (a, b, c, d + 1),
            }
        });
    let mut native_ignore_sources: Vec<PathBuf> = policy
        .rules()
        .iter()
        .filter(|rule| matches!(rule.source, RuleSource::AdapterNative(_)))
        .map(|rule| match &rule.source {
            RuleSource::AdapterNative(path) => path.clone(),
            _ => PathBuf::new(),
        })
        .collect();
    native_ignore_sources.sort();
    native_ignore_sources.dedup();
    let warnings = embedding_endpoint_warnings(&effective);
    Ok(DoctorReport {
        root: root.clone(),
        adapter: adapter.kind(),
        capabilities: adapter.capabilities().list(),
        mode,
        config_file: root
            .join(CONFIG_FILENAME)
            .exists()
            .then(|| root.join(CONFIG_FILENAME)),
        local_config_file: root
            .join(LOCAL_CONFIG_FILENAME)
            .exists()
            .then(|| root.join(LOCAL_CONFIG_FILENAME)),
        remote_identity: adapter.remote_identity()?,
        effective: effective.clone(),
        policy: PolicySummary {
            package_default_rules,
            secret_rules,
            config_rules,
            native_ignore_sources,
            native_rules,
            fingerprint: policy.fingerprint(),
        },
        config_fingerprint: effective.fingerprint(),
        diagnostics,
        warnings,
    })
}

/// Non-fatal advisories about the operator's embedding configuration.
fn embedding_endpoint_warnings(effective: &EffectiveConfig) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
    if let Some(settings) = &effective.embedding {
        let resolves_to_non_tls_remote = settings
            .endpoint
            .as_deref()
            .and_then(|endpoint_ref| endpoint_ref.strip_prefix("env:"))
            .and_then(|name| std::env::var(name).ok())
            .map(|endpoint| {
                let trimmed = endpoint.trim().trim_end_matches('/');
                let Some(host) = trimmed.strip_prefix("http://") else {
                    return false;
                };
                let host = host.split('/').next().unwrap_or("");
                !(host == "127.0.0.1" || host == "localhost" || host == "[::1]")
            })
            .unwrap_or(false);
        if resolves_to_non_tls_remote {
            warnings.push(
                "embedding.endpoint resolves to a non-TLS http:// remote endpoint: \
                 chunk text and the API key travel in cleartext; use https, or a \
                 localhost endpoint for a local inference server"
                    .to_string(),
            );
        }
    }
    warnings
}
