//! Benchmark orchestration (AP-04).
//!
//! The `bench` service owns experiment freezing, trial scheduling, workspace
//! isolation, budgets, interruption and artifacts. It never implements an
//! agent loop: each trial's agent process is driven through the versioned
//! agent adapter protocol (AP-03), delegated to the checked-in protocol
//! host (the optional Node companion) so protocol enforcement has one
//! source of truth. Native orchestration runs without Node; Node is only
//! the explicit optional companion required to run agent adapters. See
//! `docs/bench.md`.

pub mod analysis;
pub mod artifacts;
pub mod grading;
pub mod plan;
pub mod preflight;
pub mod runner;
pub mod suite;
pub mod workspace;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::Value;

use crate::adapter::git::GitAdapter;
use crate::error::Error;

/// Stable exit codes for `repoise bench` (global: 0 ok, 1 operational,
/// 2 usage; bench adds 3 preflight/suite and 4 interrupted).
pub const EXIT_OK: u8 = 0;
pub const EXIT_OPERATIONAL: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_PREFLIGHT: u8 = 3;
pub const EXIT_INTERRUPTED: u8 = 4;

/// One `repoise bench` invocation (the CLI is a thin adapter over this).
pub struct BenchRequest {
    /// Suite manifest path (absolute or CWD-relative).
    pub suite_path: PathBuf,
    pub dry_run: bool,
    /// Machine output mode (progress lines go to stderr under JSON).
    pub json: bool,
    /// Runner override (`--runner` or `REPOISE_BENCH_RUNNER`).
    pub runner: Option<PathBuf>,
    /// Repetition override.
    pub repeat: Option<u32>,
    /// Seed override.
    pub seed: Option<u64>,
    /// Output root override.
    pub output: Option<PathBuf>,
    /// The running `repoise` executable (trial index builds, MCP argv).
    pub self_exe: PathBuf,
}

/// Terminal outcome: the exit code, the machine payload (dry-run document
/// or experiment report) and the human summary.
pub struct BenchOutcome {
    pub exit_code: u8,
    pub payload: Value,
    pub summary: String,
}

/// Everything known before the first trial: the validated suite (with
/// overrides applied), the frozen plan, preflight state and the output root.
struct Prepared {
    suite_path: PathBuf,
    suite: suite::Suite,
    suite_hash: String,
    plan: plan::TrialPlan,
    trials: Vec<plan::Trial>,
    preflight: preflight::Preflight,
    overrides: BTreeMap<String, Value>,
    output_root: PathBuf,
}

/// Loads, validates and preflights the suite. Failures here are stable
/// exit-code-3 conditions (reported with an error payload, not a crash).
fn prepare(request: &BenchRequest) -> Result<Prepared, Error> {
    let suite_path = suite::resolve_path(&request.suite_path.to_string_lossy())?;
    let suite_path = std::fs::canonicalize(&suite_path).map_err(|err| {
        Error::Config(format!(
            "suite file {} does not exist: {err}",
            suite_path.display()
        ))
    })?;
    let suite_bytes = std::fs::read(&suite_path).map_err(Error::Io)?;
    let suite_hash = crate::hash::sha256_hex(&suite_bytes);
    let mut suite: suite::Suite =
        serde_json::from_slice(&suite_bytes).map_err(|err| Error::Json(err.to_string()))?;
    let mut overrides: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(repeat) = request.repeat {
        if repeat == 0 {
            return Err(Error::Config("--repeat must be at least 1".into()));
        }
        if repeat != suite.repeat {
            suite.repeat = repeat;
            overrides.insert("repeat".into(), Value::from(repeat));
        }
    }
    if let Some(seed) = request.seed
        && seed != suite.seed
    {
        suite.seed = seed;
        overrides.insert("seed".into(), Value::from(seed));
    }
    if let Some(runner) = &request.runner {
        suite.adapter.executable = runner.to_string_lossy().into_owned();
        overrides.insert(
            "runner".into(),
            Value::from(runner.to_string_lossy().into_owned()),
        );
    }
    suite.validate()?;

    let output_root = match &request.output {
        Some(path) => suite::resolve_path(&path.to_string_lossy())?,
        None => suite::resolve_path(&suite.output.dir)?,
    };
    if request.output.is_some() {
        overrides.insert(
            "output".into(),
            Value::from(output_root.to_string_lossy().into_owned()),
        );
    }

    let plan = plan::TrialPlan::new(&suite);
    let trials = plan.trials(&suite);
    let preflight = preflight::run(&suite, &suite_path, &output_root)?;
    Ok(Prepared {
        suite_path,
        suite,
        suite_hash,
        plan,
        trials,
        preflight,
        overrides,
        output_root,
    })
}

fn preflight_failure(message: String) -> BenchOutcome {
    BenchOutcome {
        exit_code: EXIT_PREFLIGHT,
        payload: serde_json::json!({
            "kind": "error",
            "stage": "preflight",
            "message": message
        }),
        summary: format!("preflight failed: {message}"),
    }
}

/// Runs one bench invocation to completion (or a recorded stop).
pub fn run(request: &BenchRequest, interrupted: Arc<AtomicBool>) -> Result<BenchOutcome, Error> {
    let prepared = match prepare(request) {
        Ok(prepared) => prepared,
        Err(err) => return Ok(preflight_failure(err.to_string())),
    };
    let suite = &prepared.suite;
    let suite_path = &prepared.suite_path;
    let suite_hash = &prepared.suite_hash;
    let plan = &prepared.plan;
    let trials = &prepared.trials;
    let preflight = &prepared.preflight;

    // --- Dry run: inspectable, side-effect free ------------------------------
    if request.dry_run {
        let payload = preflight::dry_run(suite, suite_hash, preflight, trials);
        return Ok(BenchOutcome {
            exit_code: EXIT_OK,
            payload,
            summary: format!(
                "dry run: {} trial(s) for experiment '{}' ({} condition(s), repeat {}, seed {})",
                trials.len(),
                suite.id,
                suite.conditions.len(),
                suite.repeat,
                suite.seed
            ),
        });
    }

    // --- Freeze: layout, manifest, then the trial loop ------------------------
    let layout = match artifacts::Layout::create(&prepared.output_root, &suite.id) {
        Ok(layout) => layout,
        Err(err) => return Ok(preflight_failure(err.to_string())),
    };
    if layout.manifest_path().exists() {
        return Ok(preflight_failure(format!(
            "experiment artifacts for '{}' already exist at {}; this build has no resume — \
             use a new experiment id or a different output directory (records are never \
             merged)",
            suite.id,
            layout.experiment_dir.display()
        )));
    }
    let manifest_input = preflight::ManifestInput {
        suite,
        suite_path,
        suite_hash,
        preflight,
        plan,
        trials,
        overrides: &prepared.overrides,
    };
    preflight::write_manifest(&layout.experiment_dir, &manifest_input)?;

    let adapter_env = resolve_adapter_env(suite)?;
    let ctx = runner::RunContext {
        self_exe: request.self_exe.clone(),
        layout: layout.clone(),
        preflight: preflight.clone(),
        suite: suite.clone(),
        keep_workspaces: suite.output.keep_workspaces,
        adapter_env,
        json: request.json,
    };

    let started = Instant::now();
    // Captured once at start: a multi-hour experiment must report its real
    // start, not the report-build time for both ends.
    let started_at = runner::now_utc();
    let experiment_deadline = suite
        .budgets
        .experiment_wall_clock_ms
        .map(|ms| started + std::time::Duration::from_millis(ms));
    let mut records: Vec<Value> = Vec::new();
    let mut report_status = "completed";
    let mut failure_reason: Option<String> = None;
    let mut previous_end = started;

    for (index, trial) in trials.iter().enumerate() {
        // Interruption: stop scheduling; remaining trials are not_started.
        if interrupted.load(Ordering::SeqCst) {
            push_not_started(&layout, &mut records, trial, "interrupted");
            continue;
        }
        // Experiment wall-time cap: checked between trials (a running trial
        // still completes, bounded by its own wall clock).
        if let Some(deadline) = experiment_deadline
            && Instant::now() >= deadline
        {
            push_not_started(&layout, &mut records, trial, "experiment_budget_exceeded");
            continue;
        }
        // Drift: the frozen inputs must still hold before every trial.
        if let Err(reason) = check_drift(suite_path, suite_hash, suite, preflight) {
            push_not_started(&layout, &mut records, trial, &reason);
            for remaining in &trials[index + 1..] {
                push_not_started(&layout, &mut records, remaining, &reason);
            }
            report_status = "failed";
            failure_reason = Some(reason.clone());
            break;
        }

        let queue_ms = Instant::now()
            .saturating_duration_since(previous_end)
            .as_millis();
        // A coordinator-level failure still records this trial and every
        // remaining trial, so the report is published and the experiment
        // state stays complete.
        let outcome = match ctx.run_trial(trial, queue_ms, &interrupted) {
            Ok(outcome) => outcome,
            Err(err) => {
                let reason = format!("runner_failure: {err}");
                push_not_started(&layout, &mut records, trial, &reason);
                for remaining in &trials[index + 1..] {
                    push_not_started(&layout, &mut records, remaining, &reason);
                }
                report_status = "failed";
                failure_reason = Some(reason);
                break;
            }
        };
        previous_end = Instant::now();
        records.push(record_summary(trial, &outcome));
        runner::progress(ctx.json, &outcome.summary);
    }

    // Interrupted after the loop: mark anything left as not_started.
    if interrupted.load(Ordering::SeqCst) && report_status == "completed" {
        report_status = "interrupted";
    }

    // The report reads the recorded trial + grading artifacts (the same
    // sources a later `bench report` rebuild uses).
    let trial_data = collect_trials(&layout)?;
    let provenance: Vec<suite::TaskProvenance> = suite
        .tasks
        .iter()
        .map(|task| task.provenance())
        .collect::<Result<Vec<_>, _>>()?;
    let input = ReportInput {
        experiment_id: &suite.id,
        suite_hash,
        tasks: &provenance,
        conditions: &suite.conditions,
        gates: suite.gates.as_ref(),
        report_spec: suite.report.as_ref(),
        planned: trials.len(),
        trials: &trial_data,
    };
    let ended_at = runner::now_utc();
    let report = assemble_report(
        &input,
        report_status,
        failure_reason,
        &started_at,
        &ended_at,
        started.elapsed().as_millis(),
    );
    artifacts::write_json(&layout.report_path(), &report)?;
    artifacts::write_text(
        &layout.experiment_dir.join("report.md"),
        &analysis::render_markdown(&report),
    )?;
    let exit_code = match report_status {
        "interrupted" => EXIT_INTERRUPTED,
        "failed" => EXIT_OPERATIONAL,
        _ => EXIT_OK,
    };
    Ok(BenchOutcome {
        exit_code,
        payload: report,
        summary: format!(
            "experiment '{}' {report_status}: {} trial(s) recorded in {} ms; report {}",
            suite.id,
            records.len(),
            started.elapsed().as_millis(),
            layout.report_path().display()
        ),
    })
}

/// Resolves adapter environment values (`env:` references included) —
/// process environment material only, never serialized. The provider's
/// `credentialRef` variable is included as well: adapters read
/// `process.env[<name>]` in their own environment, so the value must reach
/// the adapter process (preflight already verified it is set).
fn resolve_adapter_env(suite: &suite::Suite) -> Result<Vec<(String, String)>, Error> {
    resolve_adapter_env_with(suite, &|name| std::env::var(name).ok())
}

/// `resolve_adapter_env` with an injectable environment lookup (tests).
fn resolve_adapter_env_with(
    suite: &suite::Suite,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<(String, String)>, Error> {
    let mut resolved: Vec<(String, String)> = Vec::new();
    for (name, value) in &suite.adapter.env {
        let value = if let Some(ref_name) = value.strip_prefix("env:") {
            lookup(ref_name).ok_or_else(|| {
                Error::Config(format!(
                    "adapter env '{name}' references env:{ref_name}, which is not set"
                ))
            })?
        } else {
            value.clone()
        };
        resolved.push((name.clone(), value));
    }
    if let Some(ref_name) = suite
        .profile
        .provider
        .as_ref()
        .and_then(|provider| provider.credential_ref.as_deref())
        .and_then(|value| value.strip_prefix("env:"))
        && !resolved.iter().any(|(name, _)| name == ref_name)
    {
        let value = lookup(ref_name).ok_or_else(|| {
            Error::Config(format!("provider credentialRef env:{ref_name} is not set"))
        })?;
        resolved.push((ref_name.to_string(), value));
    }
    Ok(resolved)
}

/// Re-checks that the frozen inputs still hold: suite bytes unchanged,
/// snapshot revision unchanged, and (for a recorded patch snapshot) the
/// patch file bytes unchanged. Returns a stable drift reason.
fn check_drift(
    suite_path: &Path,
    suite_hash: &str,
    suite: &suite::Suite,
    preflight: &preflight::Preflight,
) -> Result<(), String> {
    let current = std::fs::read(suite_path).map_err(|err| err.to_string())?;
    if crate::hash::sha256_hex(&current) != suite_hash {
        return Err("config_drift: the suite file changed after the experiment was frozen".into());
    }
    let adapter = GitAdapter::new(&preflight.root).map_err(|err| err.to_string())?;
    let resolved = adapter
        .run_git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{}^{{commit}}", suite.snapshot.revision),
        ])
        .map(|value| value.trim().to_string())
        .map_err(|err| err.to_string())?;
    if resolved != preflight.revision_sha {
        return Err(
            "config_drift: the repository snapshot revision changed after the experiment was frozen"
                .into(),
        );
    }
    // A recorded patch snapshot re-applies the patch file for every trial,
    // so a mid-experiment edit would silently change the snapshot. Re-hash
    // it against the value frozen at preflight.
    if let Some(patch_file) = &preflight.patch_file {
        let patch_hash = preflight.patch_hash.as_deref().ok_or_else(|| {
            "config_drift: patch snapshot is missing its recorded hash".to_string()
        })?;
        patch_drift_error(patch_file, patch_hash)?;
    }
    Ok(())
}

/// Compares the recorded patch file against the hash frozen at preflight;
/// a changed (or unreadable) patch would otherwise apply a different
/// snapshot to later trials without any drift signal.
fn patch_drift_error(patch_file: &Path, patch_hash: &str) -> Result<(), String> {
    let patch_bytes = std::fs::read(patch_file).map_err(|err| err.to_string())?;
    if crate::hash::sha256_hex(&patch_bytes) != patch_hash {
        return Err(
            "config_drift: the recorded patch file changed after the experiment was frozen".into(),
        );
    }
    Ok(())
}

fn push_not_started(
    layout: &artifacts::Layout,
    records: &mut Vec<Value>,
    trial: &plan::Trial,
    reason: &str,
) {
    let record = record_summary(
        trial,
        &runner::TrialOutcome {
            status: "not_started".into(),
            reason: Some(reason.to_string()),
            summary: format!("{reason}: not started"),
        },
    );
    // Persist the slot so a later `bench report` rebuild sees the same
    // counts; a failure here must not mask the real failure reason.
    let dir = layout.trial_dir(&trial.trial_id);
    let _ = std::fs::create_dir_all(&dir);
    let _ = artifacts::write_json(&dir.join("trial.json"), &record);
    records.push(record);
}

fn record_summary(trial: &plan::Trial, outcome: &runner::TrialOutcome) -> Value {
    serde_json::json!({
        "trialId": trial.trial_id,
        "taskId": trial.task_id,
        "conditionId": trial.condition_id,
        "repetition": trial.repetition,
        "orderedIndex": trial.ordered_index,
        "status": outcome.status,
        "reason": outcome.reason,
        "summary": outcome.summary
    })
}

/// Everything the report needs, decoupled from the live suite so a report
/// rebuild can work from the frozen manifest alone.
struct ReportInput<'a> {
    experiment_id: &'a str,
    suite_hash: &'a str,
    tasks: &'a [suite::TaskProvenance],
    conditions: &'a [suite::Condition],
    gates: Option<&'a suite::Gates>,
    report_spec: Option<&'a suite::ReportSpec>,
    planned: usize,
    trials: &'a [analysis::TrialData],
}

/// The final experiment report (AP-05): status/grading counts, per-task and
/// per-condition grading summaries, the declared paired comparisons (when
/// gates are predeclared) and the predeclared quality gate. The same value
/// is written to `report.json`; `report.md` is rendered from it.
fn assemble_report(
    input: &ReportInput,
    status: &str,
    failure_reason: Option<String>,
    started_at: &str,
    ended_at: &str,
    duration_ms: u128,
) -> Value {
    let records: Vec<Value> = input
        .trials
        .iter()
        .map(|data| {
            serde_json::json!({
                "trialId": data.trial["trialId"],
                "taskId": data.trial["taskId"],
                "conditionId": data.trial["conditionId"],
                "repetition": data.trial["repetition"],
                "status": data.trial["status"],
                "reason": data.trial["reason"],
                "grading": data
                    .grading
                    .as_ref()
                    .and_then(|grading| grading.get("status").cloned())
                    .unwrap_or(Value::Null),
                "timings": data.trial.get("timings").cloned().unwrap_or(Value::Null),
                "usage": data.trial.get("usage").cloned().unwrap_or(Value::Null)
            })
        })
        .collect();
    let counts = analysis::counts(input.trials, input.planned);
    let grading = analysis::grading_summary(input.trials, input.tasks);
    let paired = input.gates.map(|gates| {
        let spec = input.report_spec.unwrap_or(&suite::ReportSpec {
            bootstrap_iterations: 1000,
            bootstrap_seed: 0,
        });
        analysis::paired_analysis(input.trials, input.tasks, gates, spec)
    });
    let gate = analysis::quality_gate(input.trials, input.conditions, input.gates, paired.as_ref());
    serde_json::json!({
        "schemaVersion": 1,
        "experimentId": input.experiment_id,
        "suiteHash": input.suite_hash,
        "status": status,
        "failureReason": failure_reason,
        "startedAtUtc": started_at,
        "endedAtUtc": ended_at,
        "durationMs": duration_ms,
        "trials": records,
        "counts": counts,
        "grading": grading,
        "analysis": paired,
        "qualityGate": gate
    })
}

/// Collects every recorded trial (`trial.json` plus optional
/// `grading.json`) from the experiment's trial directories, sorted by
/// trial id. Directories without a `trial.json` are skipped: a partial
/// write is the only way one can exist, and the status counts then reflect
/// the records actually on disk.
fn collect_trials(layout: &artifacts::Layout) -> Result<Vec<analysis::TrialData>, Error> {
    let mut collected: Vec<analysis::TrialData> = Vec::new();
    let entries = std::fs::read_dir(&layout.trials_dir).map_err(Error::Io)?;
    for entry in entries {
        let entry = entry.map_err(Error::Io)?;
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let trial_path = entry.path().join("trial.json");
        let Some(trial) = artifacts::read_json(&trial_path).ok() else {
            continue;
        };
        let grading = artifacts::read_json(&entry.path().join("grading.json")).ok();
        collected.push(analysis::TrialData { trial, grading });
    }
    collected.sort_by(|a, b| {
        a.trial["trialId"]
            .as_str()
            .unwrap_or("")
            .cmp(b.trial["trialId"].as_str().unwrap_or(""))
    });
    Ok(collected)
}

/// `repoise bench report` (AP-05): re-grades recorded trials offline
/// (deterministic checks re-run; manual grading records are picked up when
/// written) and reassembles `report.json`/`report.md` from the frozen
/// manifest and the trial artifacts. The run facts (start/end/duration,
/// status) carry over from the previous report when present.
pub struct RebuildRequest {
    pub output: PathBuf,
    pub experiment_id: String,
    pub json: bool,
}

pub fn rebuild(request: &RebuildRequest) -> Result<BenchOutcome, Error> {
    let output_root = suite::resolve_path(&request.output.to_string_lossy())?;
    let experiment_dir = output_root.join(&request.experiment_id);
    let manifest = artifacts::read_json(&experiment_dir.join("manifest.json")).map_err(|_| {
        Error::Config(format!(
            "no experiment manifest for '{}' under {}; the experiment has not been run (or the \
             output root is wrong)",
            request.experiment_id,
            output_root.display()
        ))
    })?;
    let manifest_error = |field: &str| -> Error {
        Error::Config(format!(
            "manifest for experiment '{}' is missing or invalid field '{field}'",
            request.experiment_id
        ))
    };
    let tasks: Vec<suite::TaskProvenance> = manifest["tasks"]
        .as_array()
        .ok_or_else(|| manifest_error("tasks"))?
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| Error::Json(err.to_string()))?;
    let conditions: Vec<suite::Condition> = manifest["conditions"]
        .as_array()
        .ok_or_else(|| manifest_error("conditions"))?
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| Error::Json(err.to_string()))?;
    let gates: Option<suite::Gates> = if manifest["gates"].is_null() {
        None
    } else {
        Some(
            serde_json::from_value(manifest["gates"].clone())
                .map_err(|err| Error::Json(err.to_string()))?,
        )
    };
    let report_spec: Option<suite::ReportSpec> = if manifest["report"].is_null() {
        None
    } else {
        Some(
            serde_json::from_value(manifest["report"].clone())
                .map_err(|err| Error::Json(err.to_string()))?,
        )
    };
    let planned = manifest["trials"]
        .as_array()
        .map(|trials| trials.len())
        .unwrap_or(0);

    // Re-grade every recorded trial offline: deterministic checks re-run,
    // valid manual grading records are picked up, engine failures are
    // recorded as evaluator_error.
    let layout = artifacts::Layout {
        experiment_dir: experiment_dir.clone(),
        cache_dir: experiment_dir.join("cache"),
        workspaces_dir: experiment_dir.join("workspaces"),
        trials_dir: experiment_dir.join("trials"),
    };
    let entries = std::fs::read_dir(&layout.trials_dir).map_err(Error::Io)?;
    for entry in entries {
        let entry = entry.map_err(Error::Io)?;
        let trial_dir = entry.path();
        if !trial_dir.join("trial.json").exists() {
            continue;
        }
        let trial = artifacts::read_json(&trial_dir.join("trial.json")).ok();
        let Some(trial) = trial else {
            continue;
        };
        let task_id = trial["taskId"].as_str().unwrap_or("");
        let task = match tasks.iter().find(|task| task.id == task_id) {
            Some(task) => task,
            None => continue,
        };
        let trial_id = trial["trialId"].as_str().unwrap_or(task_id);
        let condition_id = trial["conditionId"].as_str().unwrap_or("");
        let repetition = trial["repetition"].as_u64().unwrap_or(0) as u32;
        let status = trial["status"].as_str().unwrap_or("");
        let reason = trial["reason"].as_str().map(str::to_string);
        let grading_task = task.grading_task();
        if let Err(err) = grading::grade_and_write(
            &grading_task,
            &trial_dir,
            trial_id,
            condition_id,
            repetition,
            status,
            reason.as_deref(),
        ) {
            let record = grading::evaluator_error_record(
                trial_id,
                task_id,
                condition_id,
                repetition,
                task.evaluator
                    .as_ref()
                    .map(|evaluator| evaluator.method.as_str())
                    .unwrap_or("none"),
                &format!("evaluator failure: {err}"),
            );
            let _ = artifacts::write_json(&trial_dir.join("grading.json"), &record);
        }
    }

    let trials = collect_trials(&layout)?;
    // Run facts carry over from the previous report (it recorded the
    // experiment run, not the analysis); the analysis itself is always
    // recomputed.
    let previous = artifacts::read_json(&layout.report_path()).ok();
    let (started_at, ended_at, duration_ms, status) = match previous {
        Some(report) => (
            report["startedAtUtc"].as_str().unwrap_or("").to_string(),
            report["endedAtUtc"].as_str().unwrap_or("").to_string(),
            report["durationMs"].as_u64().unwrap_or(0) as u128,
            report["status"].as_str().unwrap_or("unknown").to_string(),
        ),
        None => (
            manifest["startedAtUtc"].as_str().unwrap_or("").to_string(),
            runner::now_utc(),
            0,
            "unknown".into(),
        ),
    };
    let suite_hash = manifest["suiteHash"].as_str().unwrap_or("");
    let input = ReportInput {
        experiment_id: &request.experiment_id,
        suite_hash,
        tasks: &tasks,
        conditions: &conditions,
        gates: gates.as_ref(),
        report_spec: report_spec.as_ref(),
        planned,
        trials: &trials,
    };
    let report = assemble_report(&input, &status, None, &started_at, &ended_at, duration_ms);
    artifacts::write_json(&layout.report_path(), &report)?;
    artifacts::write_text(
        &layout.experiment_dir.join("report.md"),
        &analysis::render_markdown(&report),
    )?;
    let gate_status = report["qualityGate"]["status"]
        .as_str()
        .unwrap_or("?")
        .to_string();
    Ok(BenchOutcome {
        exit_code: EXIT_OK,
        payload: report,
        summary: format!(
            "report rebuilt for experiment '{}': {} trial(s), quality gate {}; report {}",
            request.experiment_id,
            trials.len(),
            gate_status,
            layout.report_path().display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRED_VAR: &str = "REPOISE_BENCH_TEST_CRED_9C4F";

    fn suite_with_credential_ref() -> suite::Suite {
        suite::Suite {
            schema_version: suite::SUITE_SCHEMA_VERSION,
            id: "exp-test".into(),
            root: "/tmp/repoise-bench-test-root".into(),
            snapshot: suite::Snapshot::default(),
            tasks: vec![suite::Task {
                id: "task-1".into(),
                prompt: Some("do it".into()),
                prompt_file: None,
                version: "1".into(),
                category: None,
                held_out: false,
                allowed_paths: Vec::new(),
                expected_artifacts: Vec::new(),
                evaluator: None,
            }],
            conditions: vec![suite::Condition {
                id: "baseline".into(),
                retrieval_mode: "baseline".into(),
                search_mode: None,
                required_capabilities: Vec::new(),
            }],
            scope: "read".into(),
            repeat: 1,
            seed: 0,
            adapter: suite::AdapterSpec {
                executable: "node".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
            profile: suite::ProfileSpec {
                harness: suite::HarnessSpec {
                    name: "fake".into(),
                    revision: None,
                },
                provider: Some(suite::ProviderSpec {
                    id: "fake-provider".into(),
                    credential_ref: Some(format!("env:{CRED_VAR}")),
                }),
                model: None,
                reasoning: None,
                compaction: None,
                sampling: None,
            },
            limits: suite::Limits::default(),
            budgets: suite::Budgets::default(),
            gates: None,
            report: None,
            output: suite::OutputSpec::default(),
        }
    }

    #[test]
    fn adapter_env_includes_provider_credential_variable() {
        // Simulated environment: the credential variable is set.
        let lookup = |name: &str| (name == CRED_VAR).then(|| "sk-test-value".to_string());
        let suite = suite_with_credential_ref();
        let resolved = resolve_adapter_env_with(&suite, &lookup).expect("credential is set");
        assert!(
            resolved
                .iter()
                .any(|(name, value)| name == CRED_VAR && value == "sk-test-value"),
            "the credentialRef variable must reach the adapter process env"
        );

        // An explicit adapter.env entry for the same name must not duplicate
        // the credential variable.
        let mut suite = suite_with_credential_ref();
        suite
            .adapter
            .env
            .insert(CRED_VAR.to_string(), "sk-test-value".into());
        let resolved = resolve_adapter_env_with(&suite, &lookup).expect("credential is set");
        assert_eq!(
            resolved.iter().filter(|(name, _)| name == CRED_VAR).count(),
            1
        );

        // An unset credential variable is a config error, not a silent drop.
        let suite = suite_with_credential_ref();
        assert!(resolve_adapter_env_with(&suite, &|_| None).is_err());
    }

    /// A recorded patch snapshot must not drift mid-experiment: an edited or
    /// missing patch file is a config_drift, an unchanged one passes.
    #[test]
    fn patch_drift_error_tracks_the_recorded_patch_bytes() {
        let dir =
            std::env::temp_dir().join(format!("repoise-bench-patch-drift-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let patch = dir.join("recorded.patch");
        std::fs::write(&patch, "diff --git a/x b/x\n").unwrap();
        let hash = crate::hash::sha256_hex(std::fs::read(&patch).unwrap().as_slice());

        assert!(patch_drift_error(&patch, &hash).is_ok());

        std::fs::write(&patch, "diff --git a/y b/y\n").unwrap();
        let err = patch_drift_error(&patch, &hash).unwrap_err();
        assert!(err.starts_with("config_drift"), "{err}");

        std::fs::remove_file(&patch).unwrap();
        assert!(patch_drift_error(&patch, &hash).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
