//! Benchmark suite manifest (version 1).
//!
//! The suite is the operator-declared experiment: which repository snapshot,
//! which tasks, which retrieval conditions, how many repetitions, which
//! seed, which adapter/profile/limits/budgets, and where the artifacts go.
//! It is parsed strictly (`deny_unknown_fields` at every level) so a typo
//! fails preflight instead of silently changing the experiment. The
//! checked-in schema is [`SUITE_SCHEMA_FILE`]; see `docs/bench.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

/// Schema version of the suite manifest.
pub const SUITE_SCHEMA_VERSION: u32 = 1;
/// Checked-in schema file published alongside the package.
pub const SUITE_SCHEMA_FILE: &str = "schemas/bench-suite.v1.schema.json";

const fn default_schema_version() -> u32 {
    SUITE_SCHEMA_VERSION
}
fn default_scope() -> String {
    "read".into()
}
fn default_repeat() -> u32 {
    1
}
fn default_task_version() -> String {
    "1".into()
}
fn default_snapshot_kind() -> String {
    "commit".into()
}
fn default_revision() -> String {
    "HEAD".into()
}
fn default_output_dir() -> String {
    "repoise-bench".into()
}

/// One operator-declared benchmark experiment.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Suite {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Stable experiment id (also the artifact directory name).
    pub id: String,
    /// Repository root (absolute, or relative to the current directory).
    pub root: String,
    #[serde(default)]
    pub snapshot: Snapshot,
    pub tasks: Vec<Task>,
    pub conditions: Vec<Condition>,
    /// Workspace scope handed to the adapter for every trial.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Repetitions per (task, condition) pair.
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    /// Recorded seed for condition-order randomization.
    #[serde(default)]
    pub seed: u64,
    pub adapter: AdapterSpec,
    pub profile: ProfileSpec,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub budgets: Budgets,
    #[serde(default)]
    pub output: OutputSpec,
}

/// Snapshot policy. The default is a clean committed snapshot; a recorded
/// patch snapshot is the explicit alternative for dirty inputs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Snapshot {
    /// `commit` (default) or `patch`.
    #[serde(default = "default_snapshot_kind")]
    pub kind: String,
    /// Revision spec for `git rev-parse --verify <revision>^{commit}`
    /// (default `HEAD`).
    #[serde(default = "default_revision")]
    pub revision: String,
    /// git-apply-able patch over `revision`; required for kind `patch`.
    pub patch_file: Option<PathBuf>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            kind: default_snapshot_kind(),
            revision: default_revision(),
            patch_file: None,
        }
    }
}

/// One benchmark task. Exactly one of `prompt`/`prompt_file` must be set.
/// Neither may contain grading answers.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    #[serde(default = "default_task_version")]
    pub version: String,
}

/// One retrieval condition.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Condition {
    /// Unique condition id (e.g. `baseline`, `mcp-lexical`, `mcp-hybrid`).
    pub id: String,
    /// `baseline` (no Repoise tools or guidance) or `repoise`.
    pub retrieval_mode: String,
    /// `lexical` or `hybrid`; required when `retrieval_mode` is `repoise`.
    pub search_mode: Option<String>,
    /// Adapter capabilities this condition requires; missing ones fail the
    /// adapter handshake preflight and are never approximated.
    #[serde(default)]
    pub required_capabilities: Vec<String>,
}

/// The adapter executable the coordinator drives (AP-03 protocol).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterSpec {
    /// Absolute path or bare command name resolved through `PATH`.
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra adapter process environment. Values may be `env:VAR_NAME`
    /// references (resolved at preflight; the raw value never reaches a
    /// manifest, request or artifact).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Agent/model profile (mirrors the AP-03 request `profile` group).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileSpec {
    pub harness: HarnessSpec,
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    #[serde(default)]
    pub model: Option<ModelSpec>,
    #[serde(default)]
    pub reasoning: Option<ReasoningSpec>,
    #[serde(default)]
    pub compaction: Option<CompactionSpec>,
    /// Adapter-specific sampling knobs; free form by design.
    #[serde(default)]
    pub sampling: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HarnessSpec {
    pub name: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderSpec {
    pub id: String,
    /// Opt-in credential reference of the form `env:VAR_NAME`.
    pub credential_ref: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReasoningSpec {
    pub level: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompactionSpec {
    pub mode: String,
}

/// Per-trial limits (mirrors the AP-03 request `limits` group).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Limits {
    /// Per-trial wall-clock cap in milliseconds. When omitted the
    /// coordinator applies [`DEFAULT_TRIAL_WALL_CLOCK_MS`], so a per-trial
    /// wall bound is always in effect (forwarded to the adapter and enforced
    /// by the coordinator's outer watchdog).
    pub wall_clock_ms: Option<u64>,
    pub max_iterations: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
    pub max_events: Option<u64>,
    pub max_event_bytes: Option<u64>,
}

/// Experiment-level budgets.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Budgets {
    /// Experiment wall-time cap in milliseconds, checked between trials: a
    /// trial already running still completes (bounded by its own wall clock);
    /// trials not started once the cap passes are recorded `not_started` (the
    /// report is still published).
    pub experiment_wall_clock_ms: Option<u64>,
}

/// Default per-trial wall clock applied by the coordinator when the suite
/// omits `limits.wallClockMs`: 15 minutes. The per-trial wall bound is
/// always in effect — it is forwarded to the adapter and enforced by the
/// coordinator's outer watchdog regardless of adapter support.
pub const DEFAULT_TRIAL_WALL_CLOCK_MS: u64 = 900_000;

/// Artifact location settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutputSpec {
    /// Output root (absolute, or relative to the current directory).
    #[serde(default = "default_output_dir")]
    pub dir: String,
    /// Keep per-trial worktrees after the run (default: removed).
    #[serde(default)]
    pub keep_workspaces: bool,
}

impl Default for OutputSpec {
    fn default() -> Self {
        Self {
            dir: default_output_dir(),
            keep_workspaces: false,
        }
    }
}

impl Suite {
    /// Validates the semantic rules the schema cannot express.
    pub fn validate(&self) -> Result<(), Error> {
        let mut problems: Vec<String> = Vec::new();
        if self.schema_version != SUITE_SCHEMA_VERSION {
            problems.push(format!(
                "unsupported schemaVersion {}; this build supports {SUITE_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.id.trim().is_empty() {
            problems.push("id must not be empty".into());
        } else if id_charset_problem(&self.id).is_some() {
            problems.push(id_charset_problem(&self.id).unwrap());
        }
        if self.root.trim().is_empty() {
            problems.push("root must not be empty".into());
        }
        if self.tasks.is_empty() {
            problems.push("at least one task is required".into());
        }
        if self.conditions.is_empty() {
            problems.push("at least one condition is required".into());
        }
        if self.repeat == 0 {
            problems.push("repeat must be at least 1".into());
        }
        match self.scope.as_str() {
            "read" | "write" => {}
            other => problems.push(format!("scope must be 'read' or 'write', got '{other}'")),
        }
        let mut task_ids: Vec<&str> = Vec::new();
        for task in &self.tasks {
            if task.id.trim().is_empty() {
                problems.push("task id must not be empty".into());
            } else if id_charset_problem(&task.id).is_some() {
                problems.push(id_charset_problem(&task.id).unwrap());
            } else if task_ids.contains(&task.id.as_str()) {
                problems.push(format!("duplicate task id '{}'", task.id));
            } else {
                task_ids.push(&task.id);
            }
            let has_prompt = task.prompt.is_some();
            let has_file = task.prompt_file.is_some();
            if has_prompt == has_file {
                problems.push(format!(
                    "task '{}' must set exactly one of prompt/promptFile",
                    task.id
                ));
            }
        }
        let mut condition_ids: Vec<&str> = Vec::new();
        for condition in &self.conditions {
            if condition.id.trim().is_empty() {
                problems.push("condition id must not be empty".into());
            } else if id_charset_problem(&condition.id).is_some() {
                problems.push(id_charset_problem(&condition.id).unwrap());
            } else if condition_ids.contains(&condition.id.as_str()) {
                problems.push(format!("duplicate condition id '{}'", condition.id));
            } else {
                condition_ids.push(&condition.id);
            }
            match condition.retrieval_mode.as_str() {
                "baseline" => {
                    if condition.search_mode.is_some() {
                        problems.push(format!(
                            "condition '{}' is baseline; searchMode must be omitted",
                            condition.id
                        ));
                    }
                }
                "repoise" => match condition.search_mode.as_deref() {
                    Some("lexical") | Some("hybrid") => {}
                    Some(other) => problems.push(format!(
                        "condition '{}' searchMode must be 'lexical' or 'hybrid', got '{other}'",
                        condition.id
                    )),
                    None => problems.push(format!(
                        "condition '{}' is repoise; searchMode is required",
                        condition.id
                    )),
                },
                other => problems.push(format!(
                    "condition '{}' retrievalMode must be 'baseline' or 'repoise', got '{other}'",
                    condition.id
                )),
            }
        }
        match self.snapshot.kind.as_str() {
            "commit" => {
                if self.snapshot.patch_file.is_some() {
                    problems.push("snapshot kind 'commit' must not set patchFile".into());
                }
            }
            "patch" => {
                if self.snapshot.patch_file.is_none() {
                    problems.push("snapshot kind 'patch' requires patchFile".into());
                }
            }
            other => {
                problems.push(format!(
                    "snapshot.kind must be 'commit' or 'patch', got '{other}'"
                ));
            }
        }
        if self.adapter.executable.trim().is_empty() {
            problems.push("adapter.executable must not be empty".into());
        }
        if self.profile.harness.name.trim().is_empty() {
            problems.push("profile.harness.name must not be empty".into());
        }
        if let Some(reasoning) = &self.profile.reasoning {
            match reasoning.level.as_str() {
                "none" | "low" | "medium" | "high" | "xhigh" => {}
                other => problems.push(format!(
                    "profile.reasoning.level must be none/low/medium/high/xhigh, got '{other}'"
                )),
            }
        }
        if let Some(compaction) = &self.profile.compaction {
            match compaction.mode.as_str() {
                "agentic" | "basic" | "off" => {}
                other => problems.push(format!(
                    "profile.compaction.mode must be agentic/basic/off, got '{other}'"
                )),
            }
        }
        if let Some(problem) = problems.first() {
            return Err(Error::Config(problem.clone()));
        }
        Ok(())
    }

    /// The task with the given id (validated ids are unique).
    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }

    /// The condition with the given id (validated ids are unique).
    pub fn condition(&self, id: &str) -> Option<&Condition> {
        self.conditions.iter().find(|condition| condition.id == id)
    }
}

/// Experiment/task/condition ids must be filesystem-safe and stable under
/// `sanitize` (artifact directories and trial ids are derived from them):
/// any other character would let two distinct ids collide after sanitizing
/// or let an artifact directory escape its root (`..`, `.`).
fn id_charset_problem(id: &str) -> Option<String> {
    if id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        None
    } else {
        Some(format!(
            "id '{id}' must contain only [A-Za-z0-9_-]: artifact directories \
             and trial ids are derived from it, so other characters could \
             make two ids collide or escape the artifact root"
        ))
    }
}

/// Loads and validates one suite file; the raw bytes are hashed by the
/// caller for the frozen manifest and drift checks.
pub fn load(path: &Path) -> Result<Suite, Error> {
    let bytes = std::fs::read(path).map_err(Error::Io)?;
    let suite: Suite =
        serde_json::from_slice(&bytes).map_err(|err| Error::Json(err.to_string()))?;
    suite.validate()?;
    Ok(suite)
}

/// Resolves a possibly-relative path against the current directory.
pub fn resolve_path(raw: &str) -> Result<PathBuf, Error> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir().map_err(Error::Io)?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_suite_json() -> Value {
        serde_json::json!({
            "id": "exp",
            "root": "/tmp/repo",
            "tasks": [{ "id": "task-a", "prompt": "do it" }],
            "conditions": [{ "id": "baseline", "retrievalMode": "baseline" }],
            "adapter": { "executable": "node" },
            "profile": { "harness": { "name": "fake" } }
        })
    }

    fn suite_with(overrides: Value) -> Suite {
        let mut value = valid_suite_json();
        if let (Some(array), Some(items)) = (value["tasks"].as_array_mut(), overrides.as_array()) {
            array.clear();
            array.extend(items.iter().cloned());
        }
        let suite: Suite = serde_json::from_value(value).unwrap();
        suite
    }

    #[test]
    fn valid_ids_pass_validation() {
        let suite: Suite = serde_json::from_value(valid_suite_json()).unwrap();
        assert!(suite.validate().is_ok());
    }

    #[test]
    fn ids_with_other_characters_are_rejected() {
        let suite = suite_with(serde_json::json!([
            { "id": "a.b", "prompt": "x" }
        ]));
        let err = suite.validate().unwrap_err();
        assert!(err.to_string().contains("[A-Za-z0-9_-]"), "{}", err);

        let mut value = valid_suite_json();
        value["conditions"] = serde_json::json!([{ "id": "x y", "retrievalMode": "baseline" }]);
        let suite: Suite = serde_json::from_value(value).unwrap();
        let err = suite.validate().unwrap_err();
        assert!(err.to_string().contains("[A-Za-z0-9_-]"), "{}", err);
    }

    #[test]
    fn experiment_ids_cannot_escape_the_artifact_root() {
        // The experiment id is the first artifact path component; `..` and
        // `.` would place artifacts in (or on) the output root's parent.
        for id in ["..", ".", "..hidden", "a/b", "a\\b"] {
            let mut value = valid_suite_json();
            value["id"] = serde_json::json!(id);
            let suite: Suite = serde_json::from_value(value).unwrap();
            let err = suite.validate().unwrap_err();
            assert!(err.to_string().contains("[A-Za-z0-9_-]"), "{id}: {err}");
        }
    }
}
