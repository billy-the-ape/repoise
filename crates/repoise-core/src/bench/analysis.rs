//! Metrics, paired comparisons and the predeclared quality gate (AP-05).
//!
//! All of this reads only the recorded trial and grading artifacts and turns
//! them into machine-readable report data plus a human-readable summary.
//! The rules that keep the numbers honest:
//!
//! - correctness denominators are graded trials; completed, invalid,
//!   cancelled, timed-out and not-started counts are reported separately and
//!   never silently dropped;
//! - timeouts are censored observations: they keep their recorded agent
//!   time (at or above the per-trial cap) and are labelled, never counted as
//!   fast completed runs;
//! - repeated trials of one task are correlated, so paired comparisons are
//!   task-level and the bootstrap resamples whole tasks;
//! - a zero baseline makes a reduction ratio undefined: it is reported as
//!   null with the affected tasks named, never approximated;
//! - small samples (fewer than 5 paired tasks) carry an "exploratory" label
//!   alongside the percentile interval.

use serde_json::Value;

use super::plan::splitmix64;
use super::suite::{Condition, Gates, ReportSpec, TaskProvenance};

/// One collected trial: the raw `trial.json` and its optional `grading.json`.
#[derive(Clone, Debug)]
pub struct TrialData {
    pub trial: Value,
    pub grading: Option<Value>,
}

/// Timed-out trials: the coordinator records the host wall-clock timeout
/// (`wall_clock_exceeded`) and the outer watchdog (`host_watchdog`).
pub fn is_timed_out(trial: &Value) -> bool {
    trial.get("status").and_then(Value::as_str) == Some("error")
        && trial
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| matches!(reason, "wall_clock_exceeded" | "host_watchdog"))
}

fn grading_status(trial: &TrialData) -> Option<&str> {
    trial
        .grading
        .as_ref()
        .and_then(|grading| grading["status"].as_str())
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    let index = ((p / 100.0) * (n as f64 - 1.0)) as usize;
    sorted[index.min(n - 1)]
}

/// Status and grading counts. Nothing is dropped: every planned trial slot
/// lands in exactly one status bucket, and every graded trial lands in
/// exactly one grading outcome.
pub fn counts(trials: &[TrialData], planned: usize) -> Value {
    let mut completed = 0usize;
    let mut timed_out = 0usize;
    let mut cancelled = 0usize;
    let mut invalid = 0usize;
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut not_graded = 0usize;
    let mut evaluator_error = 0usize;
    for trial in trials {
        match trial.trial["status"].as_str() {
            Some("completed") => completed += 1,
            Some("error") if is_timed_out(&trial.trial) => timed_out += 1,
            Some("error") | Some("infra_error") => invalid += 1,
            Some("cancelled") => cancelled += 1,
            _ => {}
        }
        match grading_status(trial) {
            Some("pass") => pass += 1,
            Some("fail") => fail += 1,
            Some("not_graded") => not_graded += 1,
            Some("evaluator_error") => evaluator_error += 1,
            _ => {}
        }
    }
    let recorded_not_started = trials
        .iter()
        .filter(|trial| trial.trial["status"].as_str() == Some("not_started"))
        .count();
    // Slots with no artifact at all are also not started.
    let not_started = recorded_not_started + planned.saturating_sub(trials.len());
    let attempted = planned.saturating_sub(not_started);
    let graded = pass + fail;
    serde_json::json!({
        "planned": planned,
        "attempted": attempted,
        "completed": completed,
        "timedOut": timed_out,
        "cancelled": cancelled,
        "invalid": invalid,
        "notStarted": not_started,
        "graded": graded,
        "pass": pass,
        "fail": fail,
        "notGraded": not_graded,
        "evaluatorError": evaluator_error
    })
}

fn bump_grading_entry(entry: &mut Value, status: Option<&str>) {
    match status {
        Some("pass") => entry["pass"] = (entry["pass"].as_u64().unwrap_or(0) + 1).into(),
        Some("fail") => entry["fail"] = (entry["fail"].as_u64().unwrap_or(0) + 1).into(),
        Some("not_graded") => {
            entry["notGraded"] = (entry["notGraded"].as_u64().unwrap_or(0) + 1).into()
        }
        Some("evaluator_error") => {
            entry["evaluatorError"] = (entry["evaluatorError"].as_u64().unwrap_or(0) + 1).into()
        }
        _ => {}
    }
    match status {
        Some("pass") | Some("fail") => {
            entry["graded"] = (entry["graded"].as_u64().unwrap_or(0) + 1).into()
        }
        _ => {}
    }
    if let Some(graded) = entry["graded"].as_u64()
        && let Some(pass) = entry["pass"].as_u64()
        && graded > 0
    {
        entry["correctnessRate"] = (pass as f64 / graded as f64).into();
    }
}

/// Grading summaries overall, per condition and per task. The correctness
/// rate denominator is graded trials; attempted/completed/timed-out counts
/// stay visible so censoring and infrastructure loss are never hidden.
pub fn grading_summary(trials: &[TrialData], tasks: &[TaskProvenance]) -> Value {
    let mut by_condition: Vec<Value> = Vec::new();
    for trial in trials {
        let condition = trial.trial["conditionId"].as_str().unwrap_or("");
        let existing = by_condition
            .iter_mut()
            .find(|entry| entry["conditionId"].as_str() == Some(condition));
        match existing {
            Some(entry) => bump_grading_entry(entry, grading_status(trial)),
            None => {
                let attempted = trials
                    .iter()
                    .filter(|t| {
                        t.trial["conditionId"].as_str() == Some(condition)
                            && t.trial["status"].as_str() != Some("not_started")
                    })
                    .count();
                let completed = trials
                    .iter()
                    .filter(|t| {
                        t.trial["conditionId"].as_str() == Some(condition)
                            && t.trial["status"].as_str() == Some("completed")
                    })
                    .count();
                let timed_out = trials
                    .iter()
                    .filter(|t| {
                        t.trial["conditionId"].as_str() == Some(condition) && is_timed_out(&t.trial)
                    })
                    .count();
                by_condition.push(serde_json::json!({
                    "conditionId": condition,
                    "attempted": attempted,
                    "completed": completed,
                    "timedOut": timed_out,
                    "graded": 0,
                    "pass": 0,
                    "fail": 0,
                    "notGraded": 0,
                    "evaluatorError": 0,
                    "correctnessRate": null
                }));
                bump_grading_entry(by_condition.last_mut().unwrap(), grading_status(trial));
            }
        }
    }
    let mut overall: Value = serde_json::json!({
        "graded": 0,
        "pass": 0,
        "fail": 0,
        "notGraded": 0,
        "evaluatorError": 0,
        "correctnessRate": null
    });
    for trial in trials {
        bump_grading_entry(&mut overall, grading_status(trial));
    }
    let by_task: Vec<Value> = tasks
        .iter()
        .map(|task| {
            let mut entry: Value = serde_json::json!({
                "taskId": task.id,
                "category": task.category,
                "heldOut": task.held_out,
                "graded": 0,
                "pass": 0,
                "fail": 0,
                "notGraded": 0,
                "evaluatorError": 0,
                "correctnessRate": null
            });
            for trial in trials {
                if trial.trial["taskId"].as_str() == Some(task.id.as_str()) {
                    bump_grading_entry(&mut entry, grading_status(trial));
                }
            }
            entry
        })
        .collect();
    serde_json::json!({
        "overall": overall,
        "heldOutTasks": tasks.iter().filter(|task| task.held_out).count(),
        "developmentTasks": tasks.iter().filter(|task| !task.held_out).count(),
        "byCondition": by_condition,
        "byTask": by_task
    })
}
/// The measured metric values for one (task, condition) cell. `agentMs`
/// comes from `timings.agentMs` (timed-out trials keep their recorded time:
/// censored at or above the per-trial cap); `inputTokens` from
/// `usage.inputTokens`. Missing measurements are excluded — never replaced
/// with zero.
fn metric_values(
    trials: &[TrialData],
    task_id: &str,
    condition_id: &str,
    metric: &str,
) -> Vec<f64> {
    let mut values: Vec<f64> = Vec::new();
    for trial in trials {
        if trial.trial["taskId"].as_str() != Some(task_id)
            || trial.trial["conditionId"].as_str() != Some(condition_id)
        {
            continue;
        }
        let value = match metric {
            "agentMs" => trial.trial.pointer("/timings/agentMs"),
            "inputTokens" => trial.trial.pointer("/usage/inputTokens"),
            _ => unreachable!("validated metric"),
        };
        if let Some(value) = value.and_then(Value::as_f64) {
            values.push(value);
        }
    }
    values
}

fn condition_metric_values(trials: &[TrialData], condition_id: &str, metric: &str) -> Vec<f64> {
    trials
        .iter()
        .filter(|trial| trial.trial["conditionId"].as_str() == Some(condition_id))
        .filter_map(|trial| {
            trial
                .trial
                .pointer(match metric {
                    "agentMs" => "/timings/agentMs",
                    "inputTokens" => "/usage/inputTokens",
                    _ => unreachable!("validated metric"),
                })
                .and_then(Value::as_f64)
        })
        .collect()
}

/// Descriptive stats for one condition over all its trials (not just the
/// paired subset), with the success-only median explicitly labelled as a
/// selection-biased secondary metric.
fn descriptive_condition_stats(trials: &[TrialData], condition_id: &str) -> Value {
    let attempts = trials
        .iter()
        .filter(|trial| {
            trial.trial["conditionId"].as_str() == Some(condition_id)
                && trial.trial["status"].as_str() != Some("not_started")
        })
        .count();
    let completed = trials
        .iter()
        .filter(|trial| {
            trial.trial["conditionId"].as_str() == Some(condition_id)
                && trial.trial["status"].as_str() == Some("completed")
        })
        .count();
    let timed_out = trials
        .iter()
        .filter(|trial| {
            trial.trial["conditionId"].as_str() == Some(condition_id) && is_timed_out(&trial.trial)
        })
        .count();
    let agent_ms = condition_metric_values(trials, condition_id, "agentMs");
    let success_only_agent_ms = trials
        .iter()
        .filter(|trial| {
            trial.trial["conditionId"].as_str() == Some(condition_id)
                && trial.trial["status"].as_str() == Some("completed")
        })
        .filter_map(|trial| {
            trial
                .trial
                .pointer("/timings/agentMs")
                .and_then(Value::as_f64)
        })
        .collect::<Vec<_>>();
    let input_tokens = condition_metric_values(trials, condition_id, "inputTokens");
    serde_json::json!({
        "attempts": attempts,
        "completed": completed,
        "timedOut": timed_out,
        "timeoutRate": (attempts > 0).then(|| timed_out as f64 / attempts as f64),
        "meanAgentMs": (!agent_ms.is_empty()).then(|| mean(&agent_ms)),
        "medianAgentMs": (!agent_ms.is_empty()).then(|| median(&agent_ms)),
        "successOnlyMedianAgentMs": (!success_only_agent_ms.is_empty()).then(|| {
            median(&success_only_agent_ms)
        }),
        "successOnlySelectionBias": "computed over completed trials only: failures, timeouts and cancellations are excluded, so this metric is not comparable to the uncensored median",
        "meanInputTokens": (!input_tokens.is_empty()).then(|| mean(&input_tokens)),
        "medianInputTokens": (!input_tokens.is_empty()).then(|| median(&input_tokens)),
        "measuredAgentMsTrials": agent_ms.len(),
        "measuredInputTokensTrials": input_tokens.len()
    })
}

/// Paired baseline-vs-candidate comparisons for the declared improvements.
/// Task-level: each task's mean over its repetitions per condition, paired
/// differences across tasks, and a seeded task-cluster bootstrap (resampled
/// tasks keep all of their repetitions). Missing telemetry excludes the task
/// for that metric and names the reason; zero baselines null the ratio.
pub fn paired_analysis(
    trials: &[TrialData],
    tasks: &[TaskProvenance],
    gates: &Gates,
    report_spec: &ReportSpec,
) -> Value {
    let mut comparisons: Vec<Value> = Vec::new();
    for improvement in &gates.improvements {
        let mut paired_diffs: Vec<f64> = Vec::new();
        let mut paired_reductions: Vec<f64> = Vec::new();
        let mut zero_baseline_tasks: Vec<String> = Vec::new();
        let mut excluded_tasks: Vec<Value> = Vec::new();
        for task in tasks {
            let baseline_values =
                metric_values(trials, &task.id, &gates.baseline, &improvement.metric);
            let candidate_values = metric_values(
                trials,
                &task.id,
                &improvement.candidate,
                &improvement.metric,
            );
            match (baseline_values.is_empty(), candidate_values.is_empty()) {
                (true, true) => {
                    excluded_tasks.push(serde_json::json!({
                        "taskId": task.id,
                        "reason": "no measured data for this metric under either condition"
                    }));
                    continue;
                }
                (true, false) => {
                    excluded_tasks.push(serde_json::json!({
                        "taskId": task.id,
                        "reason": format!("no measured {} data under baseline condition '{}'", improvement.metric, gates.baseline)
                    }));
                    continue;
                }
                (false, true) => {
                    excluded_tasks.push(serde_json::json!({
                        "taskId": task.id,
                        "reason": format!("no measured {} data under candidate condition '{}'", improvement.metric, improvement.candidate)
                    }));
                    continue;
                }
                (false, false) => {}
            }
            let baseline_mean = mean(&metric_values(
                trials,
                &task.id,
                &gates.baseline,
                &improvement.metric,
            ));
            let candidate_mean = mean(&metric_values(
                trials,
                &task.id,
                &improvement.candidate,
                &improvement.metric,
            ));
            paired_diffs.push(candidate_mean - baseline_mean);
            if baseline_mean > 0.0 {
                paired_reductions.push((baseline_mean - candidate_mean) / baseline_mean);
            } else {
                zero_baseline_tasks.push(task.id.clone());
            }
        }
        let paired_tasks = paired_diffs.len();
        let ci = if paired_tasks >= 2 {
            let mut medians: Vec<f64> = Vec::new();
            let mut state = splitmix64(report_spec.bootstrap_seed);
            for _ in 0..report_spec.bootstrap_iterations {
                let mut drawn: Vec<f64> = Vec::new();
                for _ in 0..paired_tasks {
                    state = splitmix64(state);
                    let index = (state % (paired_tasks as u64)) as usize;
                    drawn.push(paired_diffs[index]);
                }
                medians.push(median(&drawn));
            }
            medians.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(serde_json::json!({
                "low": percentile(&medians, 2.5),
                "high": percentile(&medians, 97.5),
                "iterations": report_spec.bootstrap_iterations,
                "seed": report_spec.bootstrap_seed,
                "method": "task-cluster bootstrap: resample whole tasks with replacement (all repetitions of a task stay in the same draw); 2.5/97.5 percentiles of the resampled median paired difference"
            }))
        } else {
            None
        };
        comparisons.push(serde_json::json!({
            "baseline": gates.baseline,
            "candidate": improvement.candidate,
            "metric": improvement.metric,
            "threshold": improvement.threshold,
            "pairedTasks": paired_tasks,
            "excludedTasks": excluded_tasks,
            "zeroBaselineTasks": zero_baseline_tasks,
            "zeroBaselineNote": if zero_baseline_tasks.is_empty() {
                Value::Null
            } else {
                Value::String(
                    "reduction is undefined (null) for tasks whose baseline mean is zero; they are listed in zeroBaselineTasks and excluded from the median reduction"
                        .into(),
                )
            },
            "medianPairedDifference": (paired_tasks > 0).then(|| median(&paired_diffs)),
            "medianPairedReduction": (!paired_reductions.is_empty()).then(|| {
                median(&paired_reductions)
            }),
            "pairedDifferenceCi": ci,
            "smallSampleNote": if paired_tasks < 5 {
                Value::String(
                    "exploratory: fewer than 5 paired tasks; the interval estimate is uncalibrated and the percentiles are descriptive only"
                        .into(),
                )
            } else {
                Value::Null
            },
            "descriptive": {
                gates.baseline.as_str(): descriptive_condition_stats(trials, &gates.baseline),
                improvement.candidate.as_str(): descriptive_condition_stats(trials, &improvement.candidate)
            }
        }));
    }
    serde_json::json!({
        "method": "paired per-task means; positive medianPairedDifference means the candidate is worse (larger) than the baseline for the metric; medianPairedReduction is (baseline - candidate) / baseline",
        "weighting": "tasks are weighted equally within this experiment; repeated trials of one task are correlated and stay inside one task in the bootstrap; cross-repository generalization requires repository-level clustering, which this build does not claim",
        "timingPolicy": "censored: timed-out trials keep their recorded agentMs (at or above the per-trial cap) and are labelled in the descriptive stats; success-only medians are a labelled secondary metric",
        "baseline": gates.baseline,
        "comparisons": comparisons
    })
}
fn condition_grading(trials: &[TrialData], condition_id: &str) -> (usize, usize) {
    let mut pass = 0usize;
    let mut graded = 0usize;
    for trial in trials {
        if trial.trial["conditionId"].as_str() != Some(condition_id) {
            continue;
        }
        match grading_status(trial) {
            Some("pass") => {
                pass += 1;
                graded += 1;
            }
            Some("fail") => graded += 1,
            _ => {}
        }
    }
    (pass, graded)
}

/// Evaluates the predeclared quality gate against the collected trials.
/// Checks are declared before the experiment; this only measures them.
/// Overall status: `insufficient_data` when no check was declared or any
/// check could not be evaluated, `fail` when any evaluated check fails, and
/// `pass` only when every declared check was evaluated and passed.
pub fn quality_gate(
    trials: &[TrialData],
    conditions: &[Condition],
    gates: Option<&Gates>,
    paired: Option<&Value>,
) -> Value {
    let Some(gates) = gates else {
        return serde_json::json!({
            "status": "not_configured",
            "note": "no gate declared in the suite; add 'gates' to predeclare the acceptable correctness margin and the practical improvement threshold before running the experiment"
        });
    };
    let mut checks: Vec<Value> = Vec::new();
    for condition in conditions {
        let (pass, graded) = condition_grading(trials, &condition.id);
        let check: Value = if graded == 0 {
            serde_json::json!({
                "name": format!("correctness:{}", condition.id),
                "observed": null,
                "threshold": gates.min_correctness,
                "status": "insufficient_data",
                "note": "no graded trials for this condition"
            })
        } else {
            let rate = pass as f64 / graded as f64;
            let status = if rate >= gates.min_correctness {
                "passed"
            } else {
                "failed"
            };
            serde_json::json!({
                "name": format!("correctness:{}", condition.id),
                "observed": rate,
                "threshold": gates.min_correctness,
                "graded": graded,
                "pass": pass,
                "status": status
            })
        };
        checks.push(check);
    }
    for improvement in &gates.improvements {
        let comparison = paired
            .and_then(|paired| paired["comparisons"].as_array())
            .and_then(|comparisons| {
                comparisons.iter().find(|comparison| {
                    comparison["candidate"].as_str() == Some(improvement.candidate.as_str())
                        && comparison["metric"].as_str() == Some(improvement.metric.as_str())
                })
            });
        let reduction =
            comparison.and_then(|comparison| comparison["medianPairedReduction"].as_f64());
        checks.push(match reduction {
            Some(reduction) => serde_json::json!({
                "name": format!("improvement:{}:{}", improvement.candidate, improvement.metric),
                "observed": reduction,
                "threshold": improvement.threshold,
                "status": if reduction >= improvement.threshold { "passed" } else { "failed" }
            }),
            None => serde_json::json!({
                "name": format!("improvement:{}:{}", improvement.candidate, improvement.metric),
                "observed": null,
                "threshold": improvement.threshold,
                "status": "insufficient_data",
                "note": "no paired tasks with measured data on both sides"
            }),
        });
        let (candidate_pass, candidate_graded) = condition_grading(trials, &improvement.candidate);
        let (baseline_pass, baseline_graded) = condition_grading(trials, &gates.baseline);
        checks.push(match (candidate_graded > 0, baseline_graded > 0) {
            (true, true) => {
                let candidate_rate = candidate_pass as f64 / candidate_graded as f64;
                let baseline_rate = baseline_pass as f64 / baseline_graded as f64;
                let allowed_floor = baseline_rate - gates.correctness_margin;
                serde_json::json!({
                    "name": format!("correctness-margin:{}", improvement.candidate),
                    "observed": candidate_rate,
                    "allowedFloor": allowed_floor,
                    "baseline": baseline_rate,
                    "margin": gates.correctness_margin,
                    "status": if candidate_rate >= allowed_floor { "passed" } else { "failed" }
                })
            }
            _ => serde_json::json!({
                "name": format!("correctness-margin:{}", improvement.candidate),
                "observed": null,
                "allowedFloor": null,
                "margin": gates.correctness_margin,
                "status": "insufficient_data",
                "note": "candidate and baseline both need at least one graded trial"
            }),
        });
    }
    // The gate passes only when every predeclared check was evaluated and
    // passed: a speed gain with insufficient correctness data is not a pass.
    let status = if checks.is_empty()
        || checks
            .iter()
            .any(|check| check["status"].as_str() == Some("insufficient_data"))
    {
        "insufficient_data"
    } else if checks
        .iter()
        .any(|check| check["status"].as_str() == Some("failed"))
    {
        "fail"
    } else {
        "pass"
    };
    serde_json::json!({
        "status": status,
        "baseline": gates.baseline,
        "minCorrectness": gates.min_correctness,
        "correctnessMargin": gates.correctness_margin,
        "checks": checks
    })
}

fn fmt_rate(rate: Option<f64>) -> String {
    rate.map(|rate| format!("{rate:.3}"))
        .unwrap_or("n/a (no graded trials)".into())
}

/// The human-readable report (`report.md`), derived from the same report
/// object as `report.json` so the two never diverge.
pub fn render_markdown(report: &Value) -> String {
    let mut md = String::new();
    md.push_str(&format!(
        "# Repoise bench report: {}\n\n",
        report["experimentId"].as_str().unwrap_or("unknown")
    ));
    md.push_str(&format!(
        "- Status: {}\n",
        report["status"].as_str().unwrap_or("?")
    ));
    if let Some(reason) = report["failureReason"].as_str() {
        md.push_str(&format!("- Failure reason: {reason}\n"));
    }
    md.push_str(&format!(
        "- Started: {}\n- Ended: {}\n- Duration: {} ms\n\n",
        report["startedAtUtc"].as_str().unwrap_or("?"),
        report["endedAtUtc"].as_str().unwrap_or("?"),
        report["durationMs"].as_u64().unwrap_or(0)
    ));
    md.push_str("## Trial counts\n\n| Metric | Count |\n|---|---|\n");
    if let Some(counts) = report["counts"].as_object() {
        for (key, value) in counts {
            if let Some(number) = value.as_u64() {
                md.push_str(&format!("| {key} | {number} |\n"));
            }
        }
    }
    md.push('\n');
    if let Some(grading) = report["grading"].as_object() {
        md.push_str("## Grading\n\n");
        let overall = &grading["overall"];
        md.push_str(&format!(
            "Overall correctness: {}/{} graded ({}).\n",
            overall["pass"].as_u64().unwrap_or(0),
            overall["graded"].as_u64().unwrap_or(0),
            fmt_rate(overall["correctnessRate"].as_f64())
        ));
        md.push_str(&format!(
            "Held-out tasks: {}; development tasks: {}.\n\n",
            grading["heldOutTasks"].as_u64().unwrap_or(0),
            grading["developmentTasks"].as_u64().unwrap_or(0)
        ));
        md.push_str("| Condition | Attempted | Completed | Timed out | Graded | Pass | Fail | Not graded | Evaluator errors | Correctness |\n");
        md.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
        if let Some(rows) = grading["byCondition"].as_array() {
            for row in rows {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                    row["conditionId"].as_str().unwrap_or("?"),
                    row["attempted"].as_u64().unwrap_or(0),
                    row["completed"].as_u64().unwrap_or(0),
                    row["timedOut"].as_u64().unwrap_or(0),
                    row["graded"].as_u64().unwrap_or(0),
                    row["pass"].as_u64().unwrap_or(0),
                    row["fail"].as_u64().unwrap_or(0),
                    row["notGraded"].as_u64().unwrap_or(0),
                    row["evaluatorError"].as_u64().unwrap_or(0),
                    fmt_rate(row["correctnessRate"].as_f64())
                ));
            }
        }
        md.push('\n');
    }
    if let Some(paired) = report["analysis"].as_object() {
        md.push_str("## Paired analysis\n\n");
        md.push_str(&format!("{}\n\n", paired["method"].as_str().unwrap_or("")));
        if let Some(comparisons) = paired["comparisons"].as_array() {
            for comparison in comparisons {
                md.push_str(&format!(
                    "### {} -> {} ({})\n\n",
                    comparison["baseline"].as_str().unwrap_or("?"),
                    comparison["candidate"].as_str().unwrap_or("?"),
                    comparison["metric"].as_str().unwrap_or("?")
                ));
                md.push_str(&format!(
                    "- Paired tasks: {} (excluded: {})\n",
                    comparison["pairedTasks"].as_u64().unwrap_or(0),
                    comparison["excludedTasks"]
                        .as_array()
                        .map(|excluded| excluded.len())
                        .unwrap_or(0)
                ));
                if let Some(diff) = comparison["medianPairedDifference"].as_f64() {
                    md.push_str(&format!(
                        "- Median paired difference (candidate - baseline): {diff}\n"
                    ));
                }
                match comparison["medianPairedReduction"].as_f64() {
                    Some(reduction) => {
                        md.push_str(&format!(
                            "- Median paired reduction: {reduction:.3} (threshold {})\n",
                            comparison["threshold"].as_f64().unwrap_or(0.0)
                        ));
                    }
                    None => md.push_str(
                        "- Median paired reduction: undefined (zero or missing baselines)\n",
                    ),
                }
                if let Some(ci) = comparison["pairedDifferenceCi"].as_object() {
                    md.push_str(&format!(
                        "- 95% percentile CI of the median paired difference: [{:.3}, {:.3}] ({} iterations, seed {})\n",
                        ci["low"].as_f64().unwrap_or(0.0),
                        ci["high"].as_f64().unwrap_or(0.0),
                        ci["iterations"].as_u64().unwrap_or(0),
                        ci["seed"].as_u64().unwrap_or(0)
                    ));
                }
                if let Some(note) = comparison["zeroBaselineNote"].as_str() {
                    md.push_str(&format!("- {note}\n"));
                }
                if let Some(note) = comparison["smallSampleNote"].as_str() {
                    md.push_str(&format!("- {note}\n"));
                }
                if let Some(descriptive) = comparison["descriptive"].as_object() {
                    for (condition, stats) in descriptive {
                        md.push_str(&format!(
                            "\nDescriptive ({condition}):\n\n- attempts {} / completed {} / timed out {} (timeout rate {})\n- median agentMs {}; success-only median agentMs {} — {} \n- median inputTokens {}\n",
                            stats["attempts"].as_u64().unwrap_or(0),
                            stats["completed"].as_u64().unwrap_or(0),
                            stats["timedOut"].as_u64().unwrap_or(0),
                            stats["timeoutRate"].as_f64().map(|rate| format!("{rate:.3}")).unwrap_or("n/a".into()),
                            stats["medianAgentMs"].as_f64().map(|v| format!("{v:.0} ms")).unwrap_or("n/a".into()),
                            stats["successOnlyMedianAgentMs"].as_f64().map(|v| format!("{v:.0} ms")).unwrap_or("n/a".into()),
                            stats["successOnlySelectionBias"].as_str().unwrap_or(""),
                            stats["medianInputTokens"].as_f64().map(|v| format!("{v:.0}")).unwrap_or("n/a".into()),
                        ));
                    }
                }
                md.push('\n');
            }
        }
    }
    if let Some(gate) = report["qualityGate"].as_object() {
        md.push_str(&format!(
            "## Quality gate: {}\n\n",
            gate["status"].as_str().unwrap_or("?")
        ));
        if let Some(note) = gate.get("note").and_then(Value::as_str) {
            md.push_str(&format!("{note}\n\n"));
        }
        if let Some(checks) = gate.get("checks").and_then(Value::as_array) {
            for check in checks {
                md.push_str(&format!(
                    "- {}: observed {} vs threshold {} -> {}\n",
                    check["name"].as_str().unwrap_or("?"),
                    check["observed"]
                        .as_f64()
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or("n/a".into()),
                    check
                        .get("threshold")
                        .and_then(Value::as_f64)
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or("n/a".into()),
                    check["status"].as_str().unwrap_or("?")
                ));
            }
        }
        md.push('\n');
    }
    md.push_str("## Notes\n\n");
    md.push_str("- Timeouts are censored observations: recorded at or above the per-trial cap, never counted as fast completed runs.\n");
    md.push_str("- Success-only medians are labelled secondary metrics (selection bias).\n");
    md.push_str("- Tasks are weighted equally; repeated trials of one task are correlated (task-cluster bootstrap).\n");
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn trial(
        task: &str,
        condition: &str,
        status: &str,
        reason: Option<&str>,
        agent_ms: Option<u64>,
        input_tokens: Option<f64>,
        grading: Option<&str>,
    ) -> TrialData {
        TrialData {
            trial: json!({
                "trialId": format!("t-{task}-{condition}-0"),
                "taskId": task,
                "conditionId": condition,
                "repetition": 0,
                "status": status,
                "reason": reason,
                "timings": { "agentMs": agent_ms },
                "usage": input_tokens.map(|tokens| json!({ "inputTokens": tokens }))
            }),
            grading: grading.map(|status| json!({ "status": status })),
        }
    }

    fn base_tasks() -> Vec<TaskProvenance> {
        vec![
            TaskProvenance {
                id: "task-a".into(),
                version: "1".into(),
                category: Some("exact-identifier".into()),
                held_out: false,
                allowed_paths: Vec::new(),
                expected_artifacts: Vec::new(),
                evaluator: None,
                prompt_hash: "hash-a".into(),
            },
            TaskProvenance {
                id: "task-b".into(),
                version: "1".into(),
                category: Some("other".into()),
                held_out: true,
                allowed_paths: Vec::new(),
                expected_artifacts: Vec::new(),
                evaluator: None,
                prompt_hash: "hash-b".into(),
            },
        ]
    }

    fn gates() -> Gates {
        serde_json::from_value(json!({
            "baseline": "baseline",
            "minCorrectness": 1.0,
            "improvements": [{ "candidate": "candidate", "metric": "agentMs", "threshold": 0.10 }]
        }))
        .unwrap()
    }

    fn report_spec() -> ReportSpec {
        ReportSpec {
            bootstrap_iterations: 100,
            bootstrap_seed: 7,
        }
    }

    fn conditions() -> Vec<Condition> {
        vec![
            Condition {
                id: "baseline".into(),
                retrieval_mode: "baseline".into(),
                search_mode: None,
                required_capabilities: Vec::new(),
            },
            Condition {
                id: "candidate".into(),
                retrieval_mode: "repoise".into(),
                search_mode: Some("lexical".into()),
                required_capabilities: Vec::new(),
            },
        ]
    }

    #[test]
    fn counts_separate_timeout_and_never_drop_a_bucket() {
        let trials = vec![
            trial(
                "a",
                "baseline",
                "completed",
                None,
                Some(1000),
                Some(10.0),
                Some("pass"),
            ),
            trial(
                "a",
                "candidate",
                "error",
                Some("wall_clock_exceeded"),
                Some(60000),
                None,
                Some("not_graded"),
            ),
            trial(
                "a",
                "candidate",
                "error",
                Some("adapter_crashed"),
                Some(3000),
                None,
                Some("not_graded"),
            ),
            trial(
                "b",
                "baseline",
                "cancelled",
                Some("coordinator_interrupt"),
                None,
                None,
                None,
            ),
            trial(
                "b",
                "candidate",
                "infra_error",
                Some("workspace_materialization"),
                None,
                None,
                Some("evaluator_error"),
            ),
        ];
        let counts = counts(&trials, 6);
        assert_eq!(counts["planned"], 6);
        assert_eq!(counts["attempted"], 5);
        assert_eq!(counts["completed"], 1);
        assert_eq!(counts["timedOut"], 1);
        assert_eq!(counts["invalid"], 2);
        assert_eq!(counts["cancelled"], 1);
        assert_eq!(counts["notStarted"], 1);
        assert_eq!(counts["graded"], 1);
        assert_eq!(counts["pass"], 1);
        assert_eq!(counts["notGraded"], 2);
        assert_eq!(counts["evaluatorError"], 1);
    }

    #[test]
    fn grading_rate_uses_graded_trials_as_denominator() {
        let trials = vec![
            trial(
                "a",
                "baseline",
                "completed",
                None,
                Some(1000),
                None,
                Some("pass"),
            ),
            trial(
                "a",
                "baseline",
                "error",
                Some("adapter_crashed"),
                Some(100),
                None,
                None,
            ),
            trial(
                "a",
                "baseline",
                "error",
                Some("wall_clock_exceeded"),
                Some(60000),
                None,
                Some("not_graded"),
            ),
        ];
        let summary = grading_summary(&trials, &base_tasks());
        let baseline = &summary["byCondition"][0];
        assert_eq!(baseline["conditionId"], "baseline");
        assert_eq!(baseline["attempted"], 3);
        assert_eq!(baseline["completed"], 1);
        assert_eq!(baseline["timedOut"], 1);
        assert_eq!(baseline["graded"], 1);
        assert_eq!(baseline["correctnessRate"], 1.0);
        assert_eq!(summary["heldOutTasks"], 1);
        assert_eq!(summary["developmentTasks"], 1);
    }

    #[test]
    fn paired_analysis_pairs_task_means_and_labels_its_limits() {
        // task-a: baseline mean 2000 (reps 1000/3000), candidate mean 800
        // (reps 700/900): reduction 0.6, difference -1200.
        // task-b: baseline measured as 0 (a real value) and candidate 400:
        // zero baseline named, reduction undefined, difference +400.
        // task-c: no telemetry under baseline -> excluded with a reason.
        let tasks = {
            let mut tasks = base_tasks();
            tasks.push(TaskProvenance {
                id: "task-c".into(),
                version: "1".into(),
                category: None,
                held_out: false,
                allowed_paths: Vec::new(),
                expected_artifacts: Vec::new(),
                evaluator: None,
                prompt_hash: "hash-c".into(),
            });
            tasks
        };
        let trials = vec![
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(1000),
                None,
                Some("pass"),
            ),
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(3000),
                None,
                Some("pass"),
            ),
            trial(
                "task-a",
                "candidate",
                "completed",
                None,
                Some(700),
                None,
                Some("pass"),
            ),
            trial(
                "task-a",
                "candidate",
                "completed",
                None,
                Some(900),
                None,
                Some("pass"),
            ),
            trial(
                "task-b",
                "baseline",
                "completed",
                None,
                Some(0),
                None,
                Some("pass"),
            ),
            trial(
                "task-b",
                "candidate",
                "completed",
                None,
                Some(400),
                None,
                Some("pass"),
            ),
            trial(
                "task-c",
                "baseline",
                "completed",
                None,
                None,
                None,
                Some("pass"),
            ),
            trial(
                "task-c",
                "candidate",
                "completed",
                None,
                Some(500),
                None,
                Some("pass"),
            ),
        ];
        let paired = paired_analysis(&trials, &tasks, &gates(), &report_spec());
        let comparison = &paired["comparisons"][0];
        assert_eq!(comparison["candidate"], "candidate");
        assert_eq!(comparison["pairedTasks"], 2);
        assert_eq!(comparison["excludedTasks"][0]["taskId"], "task-c");
        assert_eq!(comparison["zeroBaselineTasks"][0], "task-b");
        // Task-a's 0.6 reduction is the only defined reduction.
        assert_eq!(comparison["medianPairedReduction"], 0.6);
        // Differences: -1200 and +400 -> median -400.
        assert_eq!(comparison["medianPairedDifference"], -400.0);
        assert!(comparison["smallSampleNote"].is_string());
        // Deterministic bootstrap: the same seed reproduces the interval.
        let again = paired_analysis(&trials, &tasks, &gates(), &report_spec());
        assert_eq!(
            comparison["pairedDifferenceCi"],
            again["comparisons"][0]["pairedDifferenceCi"]
        );
        let ci = comparison["pairedDifferenceCi"].as_object().unwrap();
        assert!(ci["low"].as_f64().unwrap() <= ci["high"].as_f64().unwrap());
    }

    #[test]
    fn faster_but_wrong_fails_the_quality_gate() {
        // Candidate is 50% faster but fails half its graded trials.
        let trials = vec![
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(2000),
                None,
                Some("pass"),
            ),
            trial(
                "task-b",
                "baseline",
                "completed",
                None,
                Some(2000),
                None,
                Some("pass"),
            ),
            trial(
                "task-a",
                "candidate",
                "completed",
                None,
                Some(1000),
                None,
                Some("fail"),
            ),
            trial(
                "task-b",
                "candidate",
                "completed",
                None,
                Some(1000),
                None,
                Some("pass"),
            ),
        ];
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        // The improvement check passes: reduction 0.5 >= 0.1. The
        // correctness checks fail: candidate 0.5 < 1.0.
        let gate = quality_gate(&trials, &conditions(), Some(&gates), Some(&paired));
        assert_eq!(gate["status"], "fail");
        let names: Vec<&str> = gate["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| check["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "correctness:baseline",
                "correctness:candidate",
                "improvement:candidate:agentMs",
                "correctness-margin:candidate"
            ]
        );
        assert_eq!(gate["checks"][2]["status"], "passed");
        assert_eq!(gate["checks"][2]["observed"], 0.5);
        assert_eq!(gate["checks"][1]["status"], "failed");
        assert_eq!(gate["checks"][3]["status"], "failed");
    }

    #[test]
    fn gate_without_declaration_or_data_is_not_a_pass() {
        let trials = vec![trial(
            "task-a",
            "baseline",
            "completed",
            None,
            Some(100),
            None,
            None,
        )];
        assert_eq!(
            quality_gate(&trials, &[], None, None)["status"],
            "not_configured"
        );
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        // Nothing graded and no paired data for the candidate: the gate
        // must not pass.
        let gate = quality_gate(&trials, &conditions(), Some(&gates), Some(&paired));
        assert_eq!(gate["status"], "insufficient_data");
    }

    #[test]
    fn gate_is_insufficient_when_any_check_lacks_data() {
        // The candidate adapter errors on every trial: correctness and
        // margin are insufficient (no graded candidate trials), but the
        // instant failures make the paired reduction look ~98% faster.
        // The gate must not pass on partial data.
        let trials = vec![
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(2000),
                None,
                Some("pass"),
            ),
            trial(
                "task-b",
                "baseline",
                "completed",
                None,
                Some(2000),
                None,
                Some("pass"),
            ),
            trial(
                "task-a",
                "candidate",
                "error",
                Some("adapter_failure"),
                Some(50),
                None,
                None,
            ),
            trial(
                "task-b",
                "candidate",
                "error",
                Some("adapter_failure"),
                Some(50),
                None,
                None,
            ),
        ];
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let gate = quality_gate(&trials, &conditions(), Some(&gates), Some(&paired));
        assert_eq!(gate["status"], "insufficient_data");
        let checks: Vec<(&str, &str)> = gate["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|check| {
                (
                    check["name"].as_str().unwrap(),
                    check["status"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            checks,
            vec![
                ("correctness:baseline", "passed"),
                ("correctness:candidate", "insufficient_data"),
                ("improvement:candidate:agentMs", "passed"),
                ("correctness-margin:candidate", "insufficient_data"),
            ]
        );
    }

    #[test]
    fn condition_attempted_and_timeout_rate_exclude_not_started_slots() {
        // 2 attempted (1 completed, 1 timed out) + 4 not-started slots for
        // the baseline condition: the per-condition numbers must match the
        // global counts (attempted 2, timeoutRate 1/2, not 1/4).
        let trials = vec![
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(2000),
                None,
                Some("pass"),
            ),
            trial(
                "task-b",
                "baseline",
                "error",
                Some("wall_clock_exceeded"),
                Some(150000),
                None,
                None,
            ),
            trial(
                "task-a",
                "baseline",
                "not_started",
                Some("budget_exceeded"),
                None,
                None,
                None,
            ),
            trial(
                "task-b",
                "baseline",
                "not_started",
                Some("budget_exceeded"),
                None,
                None,
                None,
            ),
        ];
        let grading = grading_summary(&trials, &base_tasks());
        let baseline = grading["byCondition"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["conditionId"].as_str() == Some("baseline"))
            .unwrap();
        assert_eq!(baseline["attempted"], 2);
        assert_eq!(baseline["completed"], 1);
        assert_eq!(baseline["timedOut"], 1);

        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let descriptive = &paired["comparisons"][0]["descriptive"];
        assert_eq!(descriptive["baseline"]["attempts"], 2);
        assert_eq!(descriptive["baseline"]["timeoutRate"], 0.5);
    }

    #[test]
    fn markdown_report_renders_the_same_numbers() {
        let trials = vec![
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(2000),
                Some(100.0),
                Some("pass"),
            ),
            trial(
                "task-a",
                "candidate",
                "completed",
                None,
                Some(1000),
                Some(50.0),
                Some("pass"),
            ),
        ];
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let gate = quality_gate(&trials, &conditions(), Some(&gates), Some(&paired));
        let report = json!({
            "experimentId": "exp-test",
            "status": "completed",
            "startedAtUtc": "2026-01-01T00:00:00Z",
            "endedAtUtc": "2026-01-01T00:01:00Z",
            "durationMs": 60000,
            "counts": counts(&trials, 2),
            "grading": grading_summary(&trials, &base_tasks()),
            "analysis": paired,
            "qualityGate": gate
        });
        let md = render_markdown(&report);
        assert!(md.contains("# Repoise bench report: exp-test"));
        assert!(md.contains("| planned | 2 |"));
        assert!(md.contains("Overall correctness: 2/2 graded (1.000)"));
        assert!(md.contains("Median paired reduction: 0.500 (threshold 0.1)"));
        assert!(md.contains("success-only median agentMs"));
        assert!(md.contains("## Quality gate: pass"));
        assert!(md.contains("censored"));
    }
}
