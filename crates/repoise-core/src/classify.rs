//! Authority and lifecycle classification (master plan section 4).
//!
//! Roles and lifecycle are separate axes; metadata may be explicit
//! (front matter) or inferred (path rules). Nothing is ever claimed to be
//! implemented from a checkmark, merged PR, or directory name alone.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ignore::{parse_pattern, rule_matches};

/// Document role within the repository corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Standing instructions for agents (AGENTS.md and friends).
    Instruction,
    /// Present-tense documentation of the current system.
    CurrentDoc,
    /// Architectural or design decision record.
    Decision,
    /// Forward-looking implementation plan.
    Plan,
    /// Record of how work was executed.
    ExecutionRecord,
    /// Superseded or archival material.
    Historical,
    /// Source code (explicitly classified corpus).
    Code,
    /// Readable configuration.
    Config,
}

impl Role {
    /// Parses a role name; `None` when unknown.
    pub fn parse(value: &str) -> Option<Role> {
        match value {
            "instruction" => Some(Role::Instruction),
            "current-doc" => Some(Role::CurrentDoc),
            "decision" => Some(Role::Decision),
            "plan" => Some(Role::Plan),
            "execution-record" => Some(Role::ExecutionRecord),
            "historical" => Some(Role::Historical),
            "code" => Some(Role::Code),
            "config" => Some(Role::Config),
            _ => None,
        }
    }

    /// Serializes a role to its canonical name.
    pub fn name(self) -> &'static str {
        match self {
            Role::Instruction => "instruction",
            Role::CurrentDoc => "current-doc",
            Role::Decision => "decision",
            Role::Plan => "plan",
            Role::ExecutionRecord => "execution-record",
            Role::Historical => "historical",
            Role::Code => "code",
            Role::Config => "config",
        }
    }
}

/// Document lifecycle, kept separate from role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lifecycle {
    /// Proposed but not yet accepted.
    Proposed,
    /// Accepted as the current position.
    Accepted,
    /// Implemented (explicitly asserted, never inferred).
    Implemented,
    /// Superseded by newer material.
    Superseded,
    /// Unknown; the default when nothing is asserted.
    Unknown,
}

impl Lifecycle {
    /// Parses a lifecycle name; `None` when unknown.
    pub fn parse(value: &str) -> Option<Lifecycle> {
        match value {
            "proposed" => Some(Lifecycle::Proposed),
            "accepted" => Some(Lifecycle::Accepted),
            "implemented" => Some(Lifecycle::Implemented),
            "superseded" => Some(Lifecycle::Superseded),
            "unknown" => Some(Lifecycle::Unknown),
            _ => None,
        }
    }
}

/// Whether classification metadata was explicit or inferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassificationSource {
    /// From explicit front matter or explicit rules.
    Explicit,
    /// Inferred from path/extension heuristics.
    Inferred,
}

/// Configured role mapping: glob pattern -> role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRule {
    /// Glob pattern (gitignore-style) matched against the relative path.
    pub pattern: String,
    /// Role assigned when the pattern matches.
    pub role: Role,
}

/// Extracts `role`/`status` keys from a leading YAML front-matter block.
pub fn front_matter(content: &str) -> (Option<Role>, Option<Lifecycle>) {
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return (None, None);
    };
    if first.trim() != "---" {
        return (None, None);
    }
    let mut role = None;
    let mut status = None;
    for line in lines {
        if line.trim() == "---" {
            return (role, status);
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        if key == "role" || key == "repoise-role" {
            role = Role::parse(value);
        } else if key == "status" || key == "repoise-status" {
            status = Lifecycle::parse(value);
        }
    }
    (None, None)
}

const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "rb", "java", "c", "cc", "cpp", "h",
    "hpp", "cs", "kt", "swift", "php", "sh", "bash", "zsh", "ps1", "sql", "lua", "pl",
];

const CONFIG_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "toml", "ini", "conf", "proto"];

/// Globs covering every known code extension (docs-only preset excludes these).
pub fn code_extension_globs() -> Vec<String> {
    CODE_EXTENSIONS
        .iter()
        .map(|ext| format!("*.{ext}"))
        .collect()
}

/// Detects a language key from the file extension; `text` when unknown.
pub fn detect_language(relative: &Path) -> String {
    let ext = relative
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if ext.is_empty() {
        return "text".to_string();
    }
    match ext.as_str() {
        "md" | "mdx" => "markdown".to_string(),
        "rst" => "restructuredtext".to_string(),
        "adoc" => "asciidoc".to_string(),
        "txt" => "text".to_string(),
        other if CODE_EXTENSIONS.contains(&other) || CONFIG_EXTENSIONS.contains(&other) => {
            other.to_string()
        }
        _ => "text".to_string(),
    }
}

/// Classifies a file: explicit front matter wins, then configured rules
/// (last match wins), then built-in path heuristics (all inferred).
pub fn classify(
    relative: &Path,
    role_rules: &[RoleRule],
    content: &str,
) -> (Role, Lifecycle, ClassificationSource) {
    let (front_role, front_status) = front_matter(content);
    if front_role.is_some() || front_status.is_some() {
        return (
            front_role.unwrap_or_else(|| infer_role(relative).0),
            front_status.unwrap_or(Lifecycle::Unknown),
            ClassificationSource::Explicit,
        );
    }
    for rule in role_rules.iter().rev() {
        let Ok(pattern) = parse_pattern(&rule.pattern, Path::new("")) else {
            continue;
        };
        if rule_matches(&pattern, relative, Some(false)) {
            return (
                rule.role,
                Lifecycle::Unknown,
                ClassificationSource::Explicit,
            );
        }
    }
    let (role, lifecycle) = infer_role(relative);
    (role, lifecycle, ClassificationSource::Inferred)
}

/// Built-in path heuristics; never claims implementation from structure alone.
pub fn infer_role(relative: &Path) -> (Role, Lifecycle) {
    let path = relative.to_string_lossy().to_lowercase();
    let file_name = relative
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if matches!(
        file_name.as_str(),
        "agents.md" | "claude.md" | "clinerules" | "cursorrules" | "copilot-instructions.md"
    ) {
        return (Role::Instruction, Lifecycle::Accepted);
    }
    let ext = relative
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if CODE_EXTENSIONS.contains(&ext.as_str()) {
        return (Role::Code, Lifecycle::Unknown);
    }
    if CONFIG_EXTENSIONS.contains(&ext.as_str()) {
        return (Role::Config, Lifecycle::Unknown);
    }
    for component in path.split('/') {
        if component == "plans" || component == "plan" {
            return (Role::Plan, Lifecycle::Proposed);
        }
        if component == "decisions" || component == "adr" {
            return (Role::Decision, Lifecycle::Accepted);
        }
        if component == "execution" {
            return (Role::ExecutionRecord, Lifecycle::Unknown);
        }
        if component == "changelog" || component == "releases" || component == "archive" {
            return (Role::Historical, Lifecycle::Superseded);
        }
    }
    (Role::CurrentDoc, Lifecycle::Unknown)
}
