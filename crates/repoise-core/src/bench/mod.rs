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

pub mod artifacts;
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

    let plan = plan::TrialPlan::new(&suite);
    let trials = plan.trials(&suite);
    let preflight = preflight::run(&suite, &suite_path)?;

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
            push_not_started(&mut records, trial, "interrupted");
            continue;
        }
        // Experiment wall-time cap: checked between trials (a running trial
        // still completes, bounded by its own wall clock).
        if let Some(deadline) = experiment_deadline
            && Instant::now() >= deadline
        {
            push_not_started(&mut records, trial, "experiment_budget_exceeded");
            continue;
        }
        // Drift: the frozen inputs must still hold before every trial.
        if let Err(reason) = check_drift(suite_path, suite_hash, suite, preflight) {
            push_not_started(&mut records, trial, &reason);
            for remaining in &trials[index + 1..] {
                push_not_started(&mut records, remaining, &reason);
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
                push_not_started(&mut records, trial, &reason);
                for remaining in &trials[index + 1..] {
                    push_not_started(&mut records, remaining, &reason);
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

    let report = build_report(
        suite,
        suite_hash,
        started,
        &started_at,
        report_status,
        failure_reason,
        &records,
    );
    artifacts::write_json(&layout.report_path(), &report)?;
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
/// process environment material only, never serialized.
fn resolve_adapter_env(suite: &suite::Suite) -> Result<Vec<(String, String)>, Error> {
    let mut resolved: Vec<(String, String)> = Vec::new();
    for (name, value) in &suite.adapter.env {
        let value = if let Some(ref_name) = value.strip_prefix("env:") {
            std::env::var(ref_name).map_err(|_| {
                Error::Config(format!(
                    "adapter env '{name}' references env:{ref_name}, which is not set"
                ))
            })?
        } else {
            value.clone()
        };
        resolved.push((name.clone(), value));
    }
    Ok(resolved)
}

/// Re-checks that the frozen inputs still hold: suite bytes unchanged,
/// snapshot revision unchanged. Returns a stable drift reason.
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
    Ok(())
}

fn push_not_started(records: &mut Vec<Value>, trial: &plan::Trial, reason: &str) {
    records.push(record_summary(
        trial,
        &runner::TrialOutcome {
            status: "not_started".into(),
            reason: Some(reason.to_string()),
            summary: format!("{reason}: not started"),
        },
    ));
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

/// The final experiment report (AP-05 owns grading; the quality gate stays
/// null here).
fn build_report(
    suite: &suite::Suite,
    suite_hash: &str,
    started: Instant,
    started_at: &str,
    status: &str,
    failure_reason: Option<String>,
    records: &[Value],
) -> Value {
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    for record in records {
        let key = record["status"].as_str().unwrap_or("unknown").to_string();
        *by_status.entry(key).or_insert(0) += 1;
    }
    serde_json::json!({
        "schemaVersion": 1,
        "experimentId": suite.id,
        "suiteHash": suite_hash,
        "status": status,
        "failureReason": failure_reason,
        "startedAtUtc": started_at,
        "endedAtUtc": runner::now_utc(),
        "durationMs": started.elapsed().as_millis(),
        "trials": records,
        "byStatus": by_status,
        "qualityGate": {
            "status": "not_evaluated",
            "note": "completion criteria and grading are defined by AP-05; this runner records completion and resources only"
        }
    })
}
