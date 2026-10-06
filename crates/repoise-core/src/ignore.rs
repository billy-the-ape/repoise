//! Explainable include/exclude/secret policy with gitignore semantics.
//!
//! Rules are ordered from lowest to highest precedence. Within that order
//! the last matching rule wins. Directory semantics follow Git: an excluded
//! directory excludes everything below it, and negation cannot re-include
//! a file whose ancestor directory is excluded. Secret deny rules are
//! absolute: no include rule, however broad, can bypass them.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::EffectiveConfig;
use crate::error::Error;
use crate::hash;

/// Where a rule comes from (shown verbatim by `doctor`/`explain`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleSource {
    /// Built-in package defaults (no .gitignore required).
    PackageDefault,
    /// Mandatory secret deny rule (cannot be bypassed).
    SecretDeny,
    /// Rule from the committed `repoise.config.json`.
    ConfigCommitted,
    /// Rule from the untracked local override file.
    ConfigLocal,
    /// Adapter-owned native ignore file (relative path, e.g. `.gitignore`).
    AdapterNative(PathBuf),
}

impl RuleSource {
    /// Stable display name for reports and fingerprints.
    pub fn name(&self) -> String {
        match self {
            RuleSource::PackageDefault => "package-default".into(),
            RuleSource::SecretDeny => "secret-deny".into(),
            RuleSource::ConfigCommitted => "config".into(),
            RuleSource::ConfigLocal => "local-config".into(),
            RuleSource::AdapterNative(path) => format!("native:{}", path.to_string_lossy()),
        }
    }
}

/// Kind of policy rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// Re-includes a path (negation).
    Include,
    /// Excludes a path.
    Exclude,
    /// Unbypassable secret exclusion.
    SecretDeny,
}

/// A parsed policy rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Rule origin (shown by explain/doctor).
    pub source: RuleSource,
    /// Include, exclude, or secret deny.
    pub kind: RuleKind,
    /// Normalized glob (no leading `!` or `/`, no trailing `/`).
    pub pattern: String,
    /// Relative base directory the pattern is anchored to.
    pub base: PathBuf,
    /// `true` when the pattern matches directories only.
    pub dir_only: bool,
    /// `true` when the pattern is anchored to `base`; otherwise it matches
    /// at any depth below `base`.
    pub anchored: bool,
    /// Original text, for display.
    pub raw: String,
}

/// Parses a gitignore-style line (without its leading `!`, which callers
/// strip before choosing the rule kind).
pub fn parse_pattern(raw: &str, base: &Path) -> Result<Rule, String> {
    let mut pattern = raw.trim().to_string();
    if pattern.is_empty() || pattern.starts_with('#') {
        return Err("empty rule".into());
    }
    let mut dir_only = false;
    while pattern.ends_with('/') {
        pattern.pop();
        dir_only = true;
    }
    let anchored = pattern.starts_with('/') || pattern.contains('/');
    if pattern.starts_with('/') {
        pattern = pattern[1..].to_string();
    }
    if pattern.is_empty() {
        return Err("empty rule".into());
    }
    Ok(Rule {
        source: RuleSource::PackageDefault,
        kind: RuleKind::Exclude,
        pattern,
        base: base.to_path_buf(),
        dir_only,
        anchored,
        raw: raw.trim().to_string(),
    })
}

/// Whether `pattern` (glob with `*`, `?`, `**`) matches `path`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let segments: Vec<&str> = pattern.split('/').collect();
    let parts: Vec<&str> = path.split('/').collect();
    let (n, m) = (segments.len(), parts.len());
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[n][m] = true;
    for i in (0..n).rev() {
        for j in (0..=m).rev() {
            if i == n {
                continue;
            }
            if segments[i] == "**" {
                dp[i][j] = dp[i + 1][j] || (j < m && dp[i][j + 1]);
            } else if j < m {
                dp[i][j] = segment_match(segments[i], parts[j]) && dp[i + 1][j + 1];
            }
        }
    }
    dp[0][0]
}

/// Single-segment glob: `*` matches any run of characters, `?` one character.
fn segment_match(segment: &str, part: &str) -> bool {
    let pattern: Vec<char> = segment.chars().collect();
    let text: Vec<char> = part.chars().collect();
    let (n, m) = (pattern.len(), text.len());
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[n][m] = true;
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            if i == n {
                continue;
            }
            if pattern[i] == '*' {
                dp[i][j] = dp[i + 1][j] || (j < m && dp[i][j + 1]);
            } else if j < m && pattern[i] == text[j] {
                dp[i][j] = dp[i + 1][j + 1];
            }
        }
    }
    dp[0][0]
}
/// Built-in package defaults: noisy, generated, or secret paths are excluded
/// even without any .gitignore present.
pub const PACKAGE_DEFAULT_PATTERNS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "bower_components",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".cache",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".docusaurus",
    ".repoise",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.log",
    "logs",
    "*.pyc",
    "*.class",
    "*.o",
    "*.so",
    "*.dll",
    "*.dylib",
    "*.exe",
    "*.bin",
    ".DS_Store",
    "Thumbs.db",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "Gemfile.lock",
    "go.sum",
    "bun.lockb",
    "poetry.lock",
    "uv.lock",
    "composer.lock",
    "flake.lock",
];

/// Mandatory secret deny patterns. They cannot be bypassed by any include
/// rule. Known template names (.env.example/.env.sample/.env.template) are
/// treated as safe and can be re-included explicitly.
pub const SECRET_DENY_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.keystore",
    "*.jks",
    "*.p7b",
    "*.gpg",
    "credentials.json",
    ".npmrc",
    ".netrc",
    ".htpasswd",
];

/// True for safe, explicitly re-includable env template names.
pub fn is_env_template(file_name: &str) -> bool {
    file_name == ".env.example" || file_name == ".env.sample" || file_name == ".env.template"
}

/// Whether a parsed rule matches `relative` (with optional directory hint).
pub fn rule_matches(rule: &Rule, relative: &Path, is_dir: Option<bool>) -> bool {
    let Some(base_rel) = relative.strip_prefix(&rule.base).ok() else {
        return false;
    };
    let text = crate::adapter::to_posix(base_rel);
    let matched = if rule.anchored {
        glob_match(&rule.pattern, &text)
    } else {
        suffix_matches(&rule.pattern, base_rel)
    };
    matched && !(rule.dir_only && is_dir == Some(false))
}

/// Matches a non-anchored pattern against every suffix of `path`.
fn suffix_matches(pattern: &str, path: &Path) -> bool {
    let component_count = path.components().count();
    for start in 0..=component_count {
        let suffix = if start >= component_count {
            None
        } else {
            Some(
                path.components()
                    .skip(start)
                    .fold(PathBuf::new(), |acc, component| acc.join(component)),
            )
        };
        let Some(suffix) = suffix else {
            continue;
        };
        if glob_match(pattern, &crate::adapter::to_posix(&suffix)) {
            return true;
        }
    }
    false
}

impl fmt::Display for RuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(rule_kind_str(*self))
    }
}

/// A matched rule as shown by `explain`/`doctor`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedRule {
    /// Rule origin.
    pub source: String,
    /// Rule kind.
    pub kind: RuleKind,
    /// Rule text with its base directory.
    pub pattern: String,
}

impl MatchedRule {
    /// Builds from a rule, rendering the pattern relative to the root.
    pub fn from_rule(rule: &Rule) -> Self {
        let display = if rule.base.as_os_str().is_empty() {
            rule.raw.clone()
        } else {
            format!("{}/{}", crate::adapter::to_posix(&rule.base), rule.raw)
        };
        Self {
            source: rule.source.name(),
            kind: rule.kind,
            pattern: display,
        }
    }
}

/// Explainable include/exclude decision for one path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// Whether the path enters the corpus.
    pub included: bool,
    /// Every rule that matched, in precedence order.
    pub matched: Vec<MatchedRule>,
    /// The rule that decided the outcome (the winner).
    pub winning_rule: Option<MatchedRule>,
}

/// Ordered policy: package defaults, secrets, config, local, native ignores.
#[derive(Clone, Debug)]
pub struct Policy {
    rules: Vec<Rule>,
}

impl Policy {
    /// Builds the effective policy. `native` lists adapter-owned ignore
    /// files as (base dir, relative label, raw lines), ordered by depth.
    pub fn build(
        eff: &EffectiveConfig,
        native: &[(PathBuf, PathBuf, Vec<String>)],
    ) -> Result<Policy, Error> {
        let mut rules = Vec::new();
        for pattern in PACKAGE_DEFAULT_PATTERNS {
            rules.push(Self::make_rule(
                RuleSource::PackageDefault,
                RuleKind::Exclude,
                pattern,
                Path::new(""),
            )?);
        }
        for pattern in SECRET_DENY_PATTERNS {
            rules.push(Self::make_rule(
                RuleSource::SecretDeny,
                RuleKind::SecretDeny,
                pattern,
                Path::new(""),
            )?);
        }
        for (pattern, _) in &eff.exclude {
            rules.push(Self::make_rule(
                RuleSource::ConfigCommitted,
                RuleKind::Exclude,
                pattern,
                Path::new(""),
            )?);
        }
        for (pattern, _) in &eff.include {
            rules.push(Self::make_rule(
                RuleSource::ConfigCommitted,
                RuleKind::Include,
                pattern,
                Path::new(""),
            )?);
        }
        for (base, file, lines) in native {
            for line in lines {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let (kind, body) = if let Some(negated) = trimmed.strip_prefix('!') {
                    (RuleKind::Include, negated)
                } else {
                    (RuleKind::Exclude, trimmed)
                };
                let mut rule = parse_pattern(body, base).map_err(|message| {
                    Error::Policy(format!("{}: {message}", file.to_string_lossy()))
                })?;
                rule.source = RuleSource::AdapterNative(file.clone());
                rule.kind = kind;
                rules.push(rule);
            }
        }
        Ok(Policy { rules })
    }

    /// Returns the ordered rules (exposed for summaries/tests).
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    fn make_rule(
        source: RuleSource,
        kind: RuleKind,
        raw: &str,
        base: &Path,
    ) -> Result<Rule, Error> {
        let mut rule = parse_pattern(raw, base).map_err(Error::Policy)?;
        rule.source = source;
        rule.kind = kind;
        Ok(rule)
    }
}

impl Policy {
    /// Decides whether `relative` is included, with full rule traceability.
    pub fn decide(&self, relative: &Path) -> Decision {
        let mut matched: Vec<MatchedRule> = Vec::new();
        // 1. Secret deny is absolute: no include rule can bypass it.
        let file_name = relative
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_env_template(&file_name) {
            for rule in &self.rules {
                if rule.kind == RuleKind::SecretDeny && rule_matches(rule, relative, Some(false)) {
                    matched.push(MatchedRule::from_rule(rule));
                    return Decision {
                        included: false,
                        winning_rule: matched.last().cloned(),
                        matched,
                    };
                }
            }
        }
        // 2. Excluded ancestor directories win; negation cannot re-include below.
        let mut directories: Vec<&Path> = relative.ancestors().collect();
        directories.pop(); // drop the file itself
        directories.reverse(); // shallowest first
        for dir in directories {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let mut winner: Option<&Rule> = None;
            for rule in &self.rules {
                if rule.kind != RuleKind::Exclude && rule.kind != RuleKind::Include {
                    continue;
                }
                if is_broad_include(rule) || !rule_matches(rule, dir, Some(true)) {
                    continue;
                }
                matched.push(MatchedRule::from_rule(rule));
                winner = Some(rule);
            }
            if let Some(rule) = winner
                && rule.kind == RuleKind::Exclude
            {
                return Decision {
                    included: false,
                    winning_rule: matched.last().cloned(),
                    matched,
                };
            }
        }
        // 3. The file itself: last matching include/exclude rule wins.
        let mut winner: Option<&Rule> = None;
        for rule in &self.rules {
            if rule.kind != RuleKind::Exclude && rule.kind != RuleKind::Include {
                continue;
            }
            if is_broad_include(rule) || !rule_matches(rule, relative, Some(false)) {
                continue;
            }
            matched.push(MatchedRule::from_rule(rule));
            winner = Some(rule);
        }
        let included = match winner {
            Some(rule) => rule.kind == RuleKind::Include,
            None => true,
        };
        Decision {
            included,
            winning_rule: matched.last().cloned(),
            matched,
        }
    }

    /// Stable fingerprint of the effective policy; changing ignore/config
    /// rules changes the fingerprint and invalidates stored inventories.
    pub fn fingerprint(&self) -> String {
        let mut key = String::new();
        for rule in &self.rules {
            key.push_str(rule_kind_str(rule.kind));
            key.push('|');
            key.push_str(&rule.source.name());
            key.push('|');
            key.push_str(&crate::adapter::to_posix(&rule.base));
            key.push('|');
            key.push_str(&rule.raw);
            key.push('\n');
        }
        hash::sha256_hex(key)
    }
}

/// A universal include (`**`) never overrides ignore rules.
fn is_broad_include(rule: &Rule) -> bool {
    rule.kind == RuleKind::Include && rule.pattern == "**"
}

fn rule_kind_str(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Include => "include",
        RuleKind::Exclude => "exclude",
        RuleKind::SecretDeny => "secret",
    }
}

/// Defense-in-depth content scan for common secret shapes. Returns a
/// short marker name (never content). Not a guarantee of complete
/// secret detection.
pub fn scan_secret_content(text: &str) -> Option<&'static str> {
    if has_prefixed(text, "AKIA", 16, &|c| {
        c.is_ascii_digit() || c.is_ascii_uppercase()
    }) || has_prefixed(text, "ghp_", 36, &|c| c.is_ascii_alphanumeric() || c == '_')
        || has_prefixed(text, "github_pat_", 22, &|c| {
            c.is_ascii_alphanumeric() || c == '_'
        })
        || has_prefixed(text, "xoxb-", 10, &|c| {
            c.is_ascii_alphanumeric() || c == '-'
        })
        || has_prefixed(text, "xoxp-", 10, &|c| {
            c.is_ascii_alphanumeric() || c == '-'
        })
        || (text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----"))
        || has_prefixed(text, "AIza", 35, &|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == '_'
        })
    {
        return Some("secret-content");
    }
    None
}

fn has_prefixed(text: &str, prefix: &str, count: usize, valid: &dyn Fn(char) -> bool) -> bool {
    let mut search_from = 0;
    while let Some(found) = text[search_from..].find(prefix) {
        let after = search_from + found + prefix.len();
        if after + count <= text.len() && text[after..after + count].chars().all(valid) {
            return true;
        }
        search_from = after + 1;
    }
    false
}

/// Defense-in-depth redaction: replaces common secret shapes in text with a
/// `[REDACTED]` marker. Returns the input unchanged when no shape matches.
/// Not a guarantee of complete secret detection.
pub fn redact_secret_content(text: &str) -> String {
    let mut out = text.to_string();
    redact_prefixed(&mut out, "AKIA", 16, &|c| {
        c.is_ascii_digit() || c.is_ascii_uppercase()
    });
    redact_prefixed(&mut out, "ghp_", 36, &|c| {
        c.is_ascii_alphanumeric() || c == '_'
    });
    redact_prefixed(&mut out, "github_pat_", 22, &|c| {
        c.is_ascii_alphanumeric() || c == '_'
    });
    redact_prefixed(&mut out, "xoxb-", 10, &|c| {
        c.is_ascii_alphanumeric() || c == '-'
    });
    redact_prefixed(&mut out, "xoxp-", 10, &|c| {
        c.is_ascii_alphanumeric() || c == '-'
    });
    redact_prefixed(&mut out, "AIza", 35, &|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    });
    // PEM private key blocks: replace from the BEGIN marker to the matching
    // END marker inclusive.
    if out.contains("-----BEGIN") && out.contains("PRIVATE KEY-----") {
        let mut result = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(begin) = rest.find("-----BEGIN") {
            let after_begin = &rest[begin + 9..];
            if let Some(end) = after_begin.find("-----END") {
                let block = &after_begin[..end + 6];
                if block.contains("PRIVATE KEY-----") {
                    result.push_str(&rest[..begin]);
                    result.push_str("[REDACTED]");
                    rest = &after_begin[end + 6..];
                    continue;
                }
            }
            // Not a private key block: keep this marker, keep scanning after it.
            result.push_str(&rest[..begin + 9]);
            rest = after_begin;
        }
        if !rest.is_empty() {
            result.push_str(rest);
        }
        out = result;
    }
    out
}

/// Replaces every `prefix` + `count` valid-char token in `out` in place.
fn redact_prefixed(out: &mut String, prefix: &str, count: usize, valid: &dyn Fn(char) -> bool) {
    let bytes = out.as_str();
    let mut result = String::with_capacity(bytes.len());
    let mut start = 0usize;
    while let Some(found) = bytes[start..].find(prefix) {
        let abs = start + found;
        let after = abs + prefix.len();
        let candidate: Vec<char> = bytes[after..].chars().take(count).collect();
        if candidate.len() == count && candidate.iter().all(|c| valid(*c)) {
            result.push_str(&bytes[start..abs]);
            result.push_str("[REDACTED]");
            start = after + candidate.iter().map(|c| c.len_utf8()).sum::<usize>();
        } else {
            result.push_str(&bytes[start..after]);
            start = after;
        }
    }
    if start > 0 {
        result.push_str(&bytes[start..]);
        *out = result;
    }
}
