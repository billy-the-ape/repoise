//! Benchmark suite manifest (version 1).
//!
//! The suite is the operator-declared experiment: which repository snapshot,
//! which tasks, which retrieval conditions, how many repetitions, which
//! seed, which adapter/profile/limits/budgets, and where the artifacts go.
//! It is parsed strictly (`deny_unknown_fields` at every level) so a typo
//! fails preflight instead of silently changing the experiment. The
//! checked-in schema is [`SUITE_SCHEMA_FILE`]; see `docs/bench.md`.

use std::collections::{BTreeMap, BTreeSet};
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
fn default_evaluator_method() -> String {
    "deterministic".into()
}
fn default_answer_field() -> String {
    "answer".into()
}
fn default_check_scope() -> String {
    "answer".into()
}
fn default_min_correctness() -> f64 {
    1.0
}
fn default_bootstrap_iterations() -> u32 {
    1000
}

/// Task categories (AP-05). Each names the assessment a task is expected to
/// exercise; `other` is the escape hatch for baseline-friendly tasks and
/// tasks that need no retrieval.
pub const TASK_CATEGORIES: &[&str] = &[
    "exact-identifier",
    "conceptual-discovery",
    "cross-file-explanation",
    "debugging",
    "small-implementation",
    "documentation-reconciliation",
    "other",
];
/// Evaluator methods (AP-05). Deterministic checks run in-process after the
/// trial; `humanRubric` and `modelJudge` are declared-but-not-graded in this
/// build (their grading results must be written as a manual grading record).
pub const EVALUATOR_METHODS: &[&str] = &["deterministic", "humanRubric", "modelJudge"];
/// Deterministic check types: substring assertions over the answer text or a
/// declared artifact file. Data-only; nothing is ever executed.
pub const CHECK_TYPES: &[&str] = &["containsAny", "containsAll", "notContains"];
/// Improvement metrics a quality gate can measure as median paired reductions.
pub const IMPROVEMENT_METRICS: &[&str] = &["agentMs", "inputTokens"];

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
    /// Predeclared quality gate and paired-comparison configuration (AP-05).
    /// Omitted: the report is produced without a gate (`not_configured`) and
    /// without paired analysis.
    #[serde(default)]
    pub gates: Option<Gates>,
    /// Report analysis configuration (AP-05).
    #[serde(default)]
    pub report: Option<ReportSpec>,
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
    /// Task category (AP-05 assessment taxonomy; see `TASK_CATEGORIES`).
    #[serde(default)]
    pub category: Option<String>,
    /// Development/tuning tasks must stay distinct from held-out tasks:
    /// creating a benchmark and then tuning to all of it invalidates a
    /// held-out claim.
    #[serde(default)]
    pub held_out: bool,
    /// Path-prefix constraints for write-scope trials (relative to the
    /// workspace root); empty means no extra change constraint beyond
    /// `scope`.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Declared names of files the agent is expected to leave in the trial's
    /// adapter artifact directory (e.g. a patch). Names only — never paths:
    /// artifact checks read `adapter/<name>` and nothing else.
    #[serde(default)]
    pub expected_artifacts: Vec<String>,
    /// Independent grading for this task (AP-05); absent means the task is
    /// never graded (`not_graded: no evaluator declared`).
    #[serde(default)]
    pub evaluator: Option<Evaluator>,
}

/// How one task is graded after the trial, outside agent visibility.
/// Grading is data-only: the engine inspects the recorded answer text and
/// declared artifact files; it never executes transcript or repository
/// content.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Evaluator {
    /// `deterministic` (engine-run checks), `humanRubric` (blinded review;
    /// the operator writes the grading record) or `modelJudge` (declared
    /// only: this build does not run a model judge and refuses to grade with
    /// the tested model itself).
    #[serde(default = "default_evaluator_method")]
    pub method: String,
    /// Field of the final event's `data` object holding the answer text.
    #[serde(default = "default_answer_field")]
    pub answer_field: String,
    /// Human-readable rubric (required for `humanRubric`).
    #[serde(default)]
    pub rubric: Option<String>,
    /// Deterministic checks (required for `deterministic`, forbidden for the
    /// declared-only methods).
    #[serde(default)]
    pub checks: Vec<Check>,
}

/// One deterministic assertion over the answer text or a declared artifact
/// file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Check {
    /// Unique within the task.
    pub id: String,
    /// `answer` (default) or `artifact`.
    #[serde(default = "default_check_scope")]
    pub scope: String,
    /// Artifact file name (required exactly when `scope` is `artifact`);
    /// must be listed in the task's `expectedArtifacts`.
    #[serde(default)]
    pub artifact: Option<String>,
    /// `containsAny`, `containsAll` or `notContains` (substring, case-
    /// sensitive).
    #[serde(rename = "type")]
    pub kind: String,
    /// Non-empty list of literal substrings. `containsAny` accepts
    /// alternative correct evidence; exact string equality is deliberately
    /// not the default.
    pub values: Vec<String>,
    /// What this check asserts (report text).
    #[serde(default)]
    pub note: Option<String>,
}

/// Predeclared quality gate (AP-05). The thresholds are decided before the
/// experiment, not fitted to its results.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Gates {
    /// Condition id treated as the comparison baseline.
    pub baseline: String,
    /// Every condition with at least one graded trial must have a
    /// pass/graded rate of at least this fraction (default 1.0).
    #[serde(default = "default_min_correctness")]
    pub min_correctness: f64,
    /// For an improvement check, the candidate's correctness rate may fall
    /// below the baseline's by at most this fraction (default 0.0).
    #[serde(default)]
    pub correctness_margin: f64,
    /// Declared paired comparisons against `baseline`.
    #[serde(default)]
    pub improvements: Vec<Improvement>,
}

/// One declared paired comparison: the median per-task reduction of
/// `metric` for `candidate` relative to the baseline, measured only over
/// tasks with measured data on both sides.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Improvement {
    /// Condition id compared against the baseline (never the baseline
    /// itself).
    pub candidate: String,
    /// `agentMs` or `inputTokens`.
    pub metric: String,
    /// Practical improvement threshold as a fraction in (0, 1]: the median
    /// paired reduction must be at least this large.
    pub threshold: f64,
}

/// Report analysis configuration (AP-05). The task-cluster bootstrap keeps
/// all repetitions of a resampled task, so repeated trials of one task stay
/// correlated inside each draw.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReportSpec {
    /// Bootstrap resamples for the paired interval estimate (default 1000).
    #[serde(default = "default_bootstrap_iterations")]
    pub bootstrap_iterations: u32,
    /// Recorded seed so the interval is reproducible (default 0).
    #[serde(default)]
    pub bootstrap_seed: u64,
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

/// Declared artifact names are read as `adapter/<name>` in the trial's
/// adapter artifact directory, so they must be plain names: no path
/// separators, no `.`/`..`, no empty entries.
fn artifact_name_problem(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        Some("must not be empty".into())
    } else if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        Some(
            "must be a plain file name (no path separators or dot entries); artifact \
             files are read as adapter/<name> only"
                .into(),
        )
    } else {
        None
    }
}

/// Resolves the task's prompt text (exactly one of `prompt`/`prompt_file`
/// after validation), so prompt provenance (hashes) has one source of truth.
impl Task {
    pub fn resolved_prompt(&self) -> Result<String, Error> {
        match (&self.prompt, &self.prompt_file) {
            (Some(prompt), None) => Ok(prompt.clone()),
            (None, Some(file)) => {
                let path = resolve_path(&file.to_string_lossy())?;
                super::artifacts::read_to_string(&path)
            }
            _ => Err(Error::Config(
                "suite must set exactly one of prompt/promptFile".into(),
            )),
        }
    }

    /// SHA-256 of the resolved prompt text (recorded in the frozen manifest).
    pub fn prompt_hash(&self) -> Result<String, Error> {
        Ok(crate::hash::sha256_hex(self.resolved_prompt()?.as_bytes()))
    }
}

/// The task provenance frozen into the manifest (AP-05): the prompt itself
/// never repeats in the manifest — only its SHA-256 — while the evaluator
/// declaration and assessment metadata travel with it so a report rebuild
/// needs no live suite file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskProvenance {
    pub id: String,
    #[serde(default = "default_task_version")]
    pub version: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub held_out: bool,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub expected_artifacts: Vec<String>,
    #[serde(default)]
    pub evaluator: Option<Evaluator>,
    /// SHA-256 of the resolved prompt text at preflight.
    pub prompt_hash: String,
}

impl Task {
    /// The provenance record frozen for the manifest.
    pub fn provenance(&self) -> Result<TaskProvenance, Error> {
        Ok(TaskProvenance {
            id: self.id.clone(),
            version: self.version.clone(),
            category: self.category.clone(),
            held_out: self.held_out,
            allowed_paths: self.allowed_paths.clone(),
            expected_artifacts: self.expected_artifacts.clone(),
            evaluator: self.evaluator.clone(),
            prompt_hash: self.prompt_hash()?,
        })
    }
}

impl TaskProvenance {
    /// A `Task` view for offline grading (`bench report` rebuilds work from
    /// the frozen manifest, not the live suite file). The prompt text is
    /// irrelevant to grading — only the evaluator declaration is — so it is
    /// a placeholder documenting the frozen prompt hash.
    pub fn grading_task(&self) -> Task {
        Task {
            id: self.id.clone(),
            prompt: Some(format!("<frozen prompt sha256:{}>", self.prompt_hash)),
            prompt_file: None,
            version: self.version.clone(),
            category: self.category.clone(),
            held_out: self.held_out,
            allowed_paths: self.allowed_paths.clone(),
            expected_artifacts: self.expected_artifacts.clone(),
            evaluator: self.evaluator.clone(),
        }
    }
}

/// Per-task validation problems. Shared by [`Suite::validate`] and the
/// report rebuild, which re-validates the task declarations frozen into the
/// manifest so a copied or hand-edited manifest fails the same rules.
pub(crate) fn task_problems(task: &Task) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    if task.id.trim().is_empty() {
        problems.push("task id must not be empty".into());
    } else if let Some(problem) = id_charset_problem(&task.id) {
        problems.push(problem);
    }
    let has_prompt = task.prompt.is_some();
    let has_file = task.prompt_file.is_some();
    if has_prompt == has_file {
        problems.push(format!(
            "task '{}' must set exactly one of prompt/promptFile",
            task.id
        ));
    }
    if let Some(category) = &task.category
        && !TASK_CATEGORIES.contains(&category.as_str())
    {
        problems.push(format!(
            "task '{}' category '{}' is not one of: {}",
            task.id,
            category,
            TASK_CATEGORIES.join(", ")
        ));
    }
    for prefix in &task.allowed_paths {
        if prefix.trim().is_empty() {
            problems.push(format!(
                "task '{}' allowedPaths must not contain empty entries",
                task.id
            ));
        }
    }
    let mut artifact_names: Vec<&str> = Vec::new();
    for artifact in &task.expected_artifacts {
        match artifact_name_problem(artifact) {
            Some(problem) => problems.push(format!(
                "task '{}' expectedArtifacts entry '{}': {}",
                task.id, artifact, problem
            )),
            None if artifact_names.contains(&artifact.as_str()) => problems.push(format!(
                "task '{}' expectedArtifacts entry '{}' is duplicated",
                task.id, artifact
            )),
            None => artifact_names.push(artifact.as_str()),
        }
    }
    if let Some(evaluator) = &task.evaluator {
        if !EVALUATOR_METHODS.contains(&evaluator.method.as_str()) {
            problems.push(format!(
                "task '{}' evaluator.method '{}' is not one of: {}",
                task.id,
                evaluator.method,
                EVALUATOR_METHODS.join(", ")
            ));
        }
        if evaluator.answer_field.trim().is_empty()
            || !evaluator
                .answer_field
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            problems.push(format!(
                "task '{}' evaluator.answerField '{}' must be a non-empty [A-Za-z0-9_] field name",
                task.id, evaluator.answer_field
            ));
        }
        match evaluator.method.as_str() {
            "deterministic" => {
                if evaluator.checks.is_empty() {
                    problems.push(format!(
                        "task '{}' deterministic evaluator requires at least one check",
                        task.id
                    ));
                }
            }
            "humanRubric" => {
                if evaluator
                    .rubric
                    .as_deref()
                    .is_none_or(|text| text.trim().is_empty())
                {
                    problems.push(format!(
                        "task '{}' humanRubric evaluator requires a non-empty rubric",
                        task.id
                    ));
                }
                if !evaluator.checks.is_empty() {
                    problems.push(format!(
                        "task '{}' humanRubric evaluator must not declare checks (grading is written as a manual grading record)",
                        task.id
                    ));
                }
            }
            "modelJudge" if !evaluator.checks.is_empty() => {
                problems.push(format!(
                    "task '{}' modelJudge evaluator must not declare checks (no model judge runs in this build; grading must be independent of the tested model)",
                    task.id
                ))
            }
            _ => {}
        }
        let mut check_ids: Vec<&str> = Vec::new();
        for check in &evaluator.checks {
            let check_where = format!("task '{}' check '{}'", task.id, check.id);
            if check.id.trim().is_empty() {
                problems.push(format!("task '{}' has a check with an empty id", task.id));
            } else if check_ids.contains(&check.id.as_str()) {
                problems.push(format!("{} id is duplicated", check_where));
            } else {
                check_ids.push(check.id.as_str());
            }
            if !CHECK_TYPES.contains(&check.kind.as_str()) {
                problems.push(format!(
                    "{}: type '{}' is not one of: {}",
                    check_where,
                    check.kind,
                    CHECK_TYPES.join(", ")
                ));
            }
            if check.values.is_empty() {
                problems.push(format!("{}: values must not be empty", check_where));
            }
            match check.scope.as_str() {
                "answer" => {
                    if check.artifact.is_some() {
                        problems.push(format!(
                            "{}: scope 'answer' must not set artifact",
                            check_where
                        ));
                    }
                }
                "artifact" => match check.artifact.as_deref() {
                    Some(artifact) if artifact_names.contains(&artifact) => {}
                    Some(artifact) => {
                        problems.push(format!(
                            "{}: artifact '{}' is not listed in the task's expectedArtifacts",
                            check_where, artifact
                        ));
                    }
                    None => {
                        problems.push(format!(
                            "{}: scope 'artifact' requires artifact",
                            check_where
                        ));
                    }
                },
                other => {
                    problems.push(format!(
                        "{}: scope must be 'answer' or 'artifact', got '{}'",
                        check_where, other
                    ));
                }
            }
        }
    }
    problems
}

/// Per-condition validation problems (including cross-condition id
/// uniqueness). Shared by [`Suite::validate`] and the report rebuild.
pub(crate) fn condition_problems(conditions: &[Condition]) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    let mut condition_ids: Vec<&str> = Vec::new();
    for condition in conditions {
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
    problems
}

/// Gate declaration problems checked against the declared conditions.
/// Shared by [`Suite::validate`] and the report rebuild.
pub(crate) fn gates_problems(gates: &Gates, conditions: &[Condition]) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    if !conditions
        .iter()
        .any(|condition| condition.id == gates.baseline)
    {
        problems.push(format!(
            "gates.baseline '{}' is not a declared condition id",
            gates.baseline
        ));
    }
    if !(0.0..=1.0).contains(&gates.min_correctness) {
        problems.push("gates.minCorrectness must be a fraction in [0, 1]".into());
    }
    if !(0.0..=1.0).contains(&gates.correctness_margin) {
        problems.push("gates.correctnessMargin must be a fraction in [0, 1]".into());
    }
    let mut candidates: Vec<&str> = Vec::new();
    for improvement in &gates.improvements {
        if !conditions
            .iter()
            .any(|condition| condition.id == improvement.candidate)
        {
            problems.push(format!(
                "gates.improvements candidate '{}' is not a declared condition id",
                improvement.candidate
            ));
        }
        if improvement.candidate == gates.baseline {
            problems.push(format!(
                "gates.improvements candidate '{}' must differ from the baseline",
                improvement.candidate
            ));
        }
        if !IMPROVEMENT_METRICS.contains(&improvement.metric.as_str()) {
            problems.push(format!(
                "gates.improvements metric '{}' must be one of: {}",
                improvement.metric,
                IMPROVEMENT_METRICS.join(", ")
            ));
        }
        if !(improvement.threshold > 0.0 && improvement.threshold <= 1.0) {
            problems.push("gates.improvements threshold must be a fraction in (0, 1]".into());
        }
        if candidates.contains(&improvement.candidate.as_str()) {
            problems.push(format!(
                "gates.improvements candidate '{}' is declared twice",
                improvement.candidate
            ));
        } else {
            candidates.push(improvement.candidate.as_str());
        }
    }
    problems
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
            let id_usable = !task.id.trim().is_empty() && id_charset_problem(&task.id).is_none();
            if id_usable && task_ids.contains(&task.id.as_str()) {
                problems.push(format!("duplicate task id '{}'", task.id));
            } else if id_usable {
                task_ids.push(&task.id);
            }
            problems.extend(task_problems(task));
        }
        problems.extend(condition_problems(&self.conditions));

        // Trial ids are generated from sanitized task/condition ids joined
        // with `-`, so distinct ids can collide across the task/condition
        // boundary (task `a` + condition `r0-x` and task `a-r0` +
        // condition `x` both generate `t-a-r0-r0-x`). Trial directories,
        // workspaces and artifacts are keyed by the id, so reject duplicates
        // here instead of overwriting artifacts.
        let mut generated_trial_ids: BTreeSet<String> = BTreeSet::new();
        for task in &self.tasks {
            for repetition in 0..self.repeat {
                for condition in &self.conditions {
                    let trial_id = super::plan::trial_id(&task.id, repetition, &condition.id);
                    if !generated_trial_ids.insert(trial_id.clone()) {
                        problems.push(format!(
                            "trial id '{trial_id}' is generated by more than one \
                             (task, condition, repetition) triple; pick task/condition \
                             ids that stay distinguishable after joining"
                        ));
                    }
                }
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
        if let Some(gates) = &self.gates {
            problems.extend(gates_problems(gates, &self.conditions));
        }
        if let Some(report) = &self.report
            && report.bootstrap_iterations == 0
        {
            problems.push("report.bootstrapIterations must be at least 1".into());
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
/// any other character would let an artifact directory escape its root
/// (`..`, `.`) or make ids ambiguous after sanitizing. Cross-id trial id
/// collisions from the `-` joiner are checked separately in `validate`.
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

    #[test]
    fn human_rubric_requires_a_rubric() {
        // A declared humanRubric evaluator without any rubric fails
        // validation (the rubric is what the operator reviews against); a
        // whitespace-only rubric is rejected the same way.
        for evaluator in [
            serde_json::json!({ "method": "humanRubric" }),
            serde_json::json!({ "method": "humanRubric", "rubric": "   " }),
        ] {
            let mut value = valid_suite_json();
            value["tasks"][0]["evaluator"] = evaluator.clone();
            let suite: Suite = serde_json::from_value(value).unwrap();
            let err = suite.validate().unwrap_err();
            assert!(
                err.to_string().contains("requires a non-empty rubric"),
                "{evaluator}: {err}"
            );
        }
        // With a rubric the declaration validates.
        let mut value = valid_suite_json();
        value["tasks"][0]["evaluator"] =
            serde_json::json!({ "method": "humanRubric", "rubric": "answer the question" });
        let suite: Suite = serde_json::from_value(value).unwrap();
        assert!(suite.validate().is_ok());
    }

    #[test]
    fn colliding_generated_trial_ids_are_rejected() {
        // Task `a` + condition `r0-x` and task `a-r0` + condition `x` both
        // generate `t-a-r0-r0-x` at repetition 0; the trial directories,
        // workspaces and artifacts would be shared.
        let value = serde_json::json!({
            "id": "exp",
            "root": "/tmp/repo",
            "tasks": [
                { "id": "a", "prompt": "x" },
                { "id": "a-r0", "prompt": "y" }
            ],
            "conditions": [
                { "id": "r0-x", "retrievalMode": "baseline" },
                { "id": "x", "retrievalMode": "baseline" }
            ],
            "adapter": { "executable": "node" },
            "profile": { "harness": { "name": "fake" } }
        });
        let suite: Suite = serde_json::from_value(value).unwrap();
        let err = suite.validate().unwrap_err();
        assert!(err.to_string().contains("t-a-r0-r0-x"), "{err}");

        // The same ids without the ambiguous task/condition pairing pass.
        let mut value = serde_json::json!({
            "id": "exp",
            "root": "/tmp/repo",
            "tasks": [
                { "id": "a", "prompt": "x" },
                { "id": "a-r0", "prompt": "y" }
            ],
            "conditions": [{ "id": "x", "retrievalMode": "baseline" }],
            "adapter": { "executable": "node" },
            "profile": { "harness": { "name": "fake" } }
        });
        value["tasks"] = serde_json::json!([{ "id": "a", "prompt": "x" }]);
        let suite: Suite = serde_json::from_value(value).unwrap();
        assert!(suite.validate().is_ok());
    }
}
