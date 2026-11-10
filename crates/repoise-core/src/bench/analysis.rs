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
//!   alongside the percentile interval;
//! - the gate verdict is claim-bearing only when the declared
//!   evidence-sufficiency policy holds over the frozen task/condition/
//!   repetition plan ([`evidence_summary`]): coverage divides by the plan
//!   (never by the surviving records), the retrieval audit's condition
//!   fidelity is reported per condition, and predeclared execution claims
//!   must be supported by observed server-side evidence — unknown audit
//!   evidence stays unknown;
//! - evidence validity is kept separate from the quality metrics: raw
//!   counts, censored-timeout semantics and every descriptive statistic
//!   are reported unchanged, and missing evidence is never converted into
//!   a wrong answer, a zero or a deleted trial.

use std::collections::BTreeMap;

use serde_json::Value;

use super::plan::splitmix64;
use super::suite::{Condition, Gates, ReportSpec, TaskProvenance};

/// One collected trial: the raw `trial.json` and its optional `grading.json`.
#[derive(Clone, Debug)]
pub struct TrialData {
    pub trial: Value,
    pub grading: Option<Value>,
}

/// Timed-out trials: the coordinator records the per-trial wall-clock
/// timeout as status `error` with reason `wall_clock_exceeded`; when the
/// outer watchdog force-kills the bridge instead, the trial is recorded as
/// `infra_error` (or `cancelled` on a coincident interrupt) with reason
/// `host_watchdog`. Both are censored timeouts: the agent ran out of its
/// wall-clock budget, so the trial keeps its recorded agentMs (at or above
/// the cap) and is never read as an infra failure.
pub fn is_timed_out(trial: &Value) -> bool {
    trial
        .get("reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| matches!(reason, "wall_clock_exceeded" | "host_watchdog"))
}

/// Harness version drift across completed trials: the distinct observed
/// harness versions (`harnessVersion` per trial) among completed trials.
/// With more than one version, pooled per-condition statistics span
/// different harness builds — the records are preserved per trial and the
/// drift is labeled here, never silently pooled. (A pinned run cannot drift:
/// mismatched trials fail closed and are never completed; this surfaces
/// unpinned runs.)
pub fn harness_drift(trials: &[TrialData]) -> Value {
    let mut versions: Vec<String> = Vec::new();
    for trial in trials {
        if trial.trial["status"].as_str() == Some("completed")
            && let Some(version) = trial.trial["harnessVersion"].as_str()
            && !versions.iter().any(|existing| existing == version)
        {
            versions.push(version.to_string());
        }
    }
    versions.sort();
    serde_json::json!({
        "versions": versions,
        "drift": versions.len() > 1,
        "note": if versions.len() > 1 {
            Value::String(format!(
                "completed trials ran on {} different harness versions ({}): per-trial \
                 records are preserved and pooled statistics span those versions — \
                 pin profile.harness.revision to enforce a single version fail-closed",
                versions.len(),
                versions.join(", ")
            ))
        } else {
            Value::Null
        }
    })
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
    if n == 0 {
        // Defense in depth: a validated report spec always resamples at
        // least once; an empty draw reports a null interval (NaN
        // serializes as JSON null) instead of panicking.
        return f64::NAN;
    }
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
            _ if is_timed_out(&trial.trial) => timed_out += 1,
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
/// Trials that contribute to metric values: completed trials and timed-out
/// trials (which keep their recorded time, censored at or above the
/// per-trial cap). Non-timeout errors, cancellations, infra errors and
/// never-started slots carried no useful measurement (a fast failure is not
/// a fast success) and are excluded from every metric.
fn metric_eligible(trial: &Value) -> bool {
    trial["status"].as_str() == Some("completed") || is_timed_out(trial)
}

/// The measured metric values for one (task, condition) cell, plus the count
/// of trials excluded as non-eligible. `agentMs` comes from `timings.agentMs`
/// (timed-out trials keep their recorded time: censored at or above the
/// per-trial cap); `inputTokens` from `usage.inputTokens`. Missing
/// measurements are excluded — never replaced with zero.
fn metric_values(
    trials: &[TrialData],
    task_id: &str,
    condition_id: &str,
    metric: &str,
) -> (Vec<f64>, usize) {
    let mut values: Vec<f64> = Vec::new();
    let mut excluded = 0usize;
    for trial in trials {
        if trial.trial["taskId"].as_str() != Some(task_id)
            || trial.trial["conditionId"].as_str() != Some(condition_id)
        {
            continue;
        }
        if !metric_eligible(&trial.trial) {
            excluded += 1;
            continue;
        }
        let value = match metric {
            "agentMs" => trial.trial.pointer("/timings/agentMs"),
            "inputTokens" => trial.trial.pointer("/usage/inputTokens"),
            // An unvalidated metric (a hand-edited manifest racing the
            // rebuild validation) measures nothing: the task is excluded
            // with the reason naming the metric.
            _ => None,
        };
        if let Some(value) = value.and_then(Value::as_f64) {
            values.push(value);
        }
    }
    (values, excluded)
}

fn condition_metric_values(trials: &[TrialData], condition_id: &str, metric: &str) -> Vec<f64> {
    trials
        .iter()
        .filter(|trial| trial.trial["conditionId"].as_str() == Some(condition_id))
        .filter(|trial| metric_eligible(&trial.trial))
        .filter_map(|trial| {
            trial
                .trial
                .pointer(match metric {
                    "agentMs" => "/timings/agentMs",
                    "inputTokens" => "/usage/inputTokens",
                    _ => "/never",
                })
                .and_then(Value::as_f64)
        })
        .collect()
}

/// Descriptive stats for one condition over its metric-eligible trials
/// (completed and timed-out; not just the paired subset), with the
/// success-only median explicitly labelled as a selection-biased secondary
/// metric.
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
        if !super::suite::IMPROVEMENT_METRICS.contains(&improvement.metric.as_str()) {
            // A hand-edited manifest can declare an unsupported metric; name
            // it per task instead of panicking or silently comparing nothing.
            comparisons.push(serde_json::json!({
                "baseline": gates.baseline,
                "candidate": improvement.candidate,
                "metric": improvement.metric,
                "threshold": improvement.threshold,
                "pairedTasks": 0,
                "excludedTasks": tasks
                    .iter()
                    .map(|task| serde_json::json!({
                        "taskId": task.id,
                        "reason": format!(
                            "declared metric '{}' is not a supported metric ({})",
                            improvement.metric,
                            super::suite::IMPROVEMENT_METRICS.join(", ")
                        )
                    }))
                    .collect::<Vec<_>>(),
                "zeroBaselineTasks": Vec::<String>::new(),
                "zeroBaselineNote": Value::Null,
                "medianPairedDifference": Value::Null,
                "medianPairedReduction": Value::Null,
                "pairedDifferenceCi": Value::Null,
                "smallSampleNote": Value::Null,
                "descriptive": {
                    gates.baseline.as_str(): descriptive_condition_stats(trials, &gates.baseline),
                    improvement.candidate.as_str(): descriptive_condition_stats(
                        trials, &improvement.candidate
                    )
                }
            }));
            continue;
        }
        let mut paired_diffs: Vec<f64> = Vec::new();
        let mut paired_reductions: Vec<f64> = Vec::new();
        let mut zero_baseline_tasks: Vec<String> = Vec::new();
        let mut excluded_tasks: Vec<Value> = Vec::new();
        for task in tasks {
            let (baseline_values, baseline_excluded) =
                metric_values(trials, &task.id, &gates.baseline, &improvement.metric);
            let (candidate_values, candidate_excluded) = metric_values(
                trials,
                &task.id,
                &improvement.candidate,
                &improvement.metric,
            );
            match (baseline_values.is_empty(), candidate_values.is_empty()) {
                (true, true) => {
                    excluded_tasks.push(serde_json::json!({
                        "taskId": task.id,
                        "reason": format!(
                            "no measured data for this metric under either condition ({} trial(s) excluded: not completed or timed out)",
                            baseline_excluded + candidate_excluded
                        )
                    }));
                    continue;
                }
                (true, false) => {
                    excluded_tasks.push(serde_json::json!({
                        "taskId": task.id,
                        "reason": format!(
                            "no measured {} data under baseline condition '{}' ({} trial(s) excluded: not completed or timed out)",
                            improvement.metric, gates.baseline, baseline_excluded
                        )
                    }));
                    continue;
                }
                (false, true) => {
                    excluded_tasks.push(serde_json::json!({
                        "taskId": task.id,
                        "reason": format!(
                            "no measured {} data under candidate condition '{}' ({} trial(s) excluded: not completed or timed out)",
                            improvement.metric, improvement.candidate, candidate_excluded
                        )
                    }));
                    continue;
                }
                (false, false) => {}
            }
            let baseline_mean = mean(&baseline_values);
            let candidate_mean = mean(&candidate_values);
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
        "timingPolicy": "censored: timed-out trials keep their recorded agentMs (at or above the per-trial cap) and are labelled in the descriptive stats; non-timeout failures, cancellations, infra errors and never-started slots are excluded from every metric value (counted in the exclusion reasons); success-only medians are a labelled secondary metric",
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

// --- Evidence sufficiency and condition fidelity --------------------------

/// One planned trial slot from the frozen task/condition/repetition plan.
/// Evidence sufficiency always divides by these slots, never by the
/// surviving records: a missing or unreadable `trial.json` cannot shrink a
/// denominator.
#[derive(Clone, Debug)]
pub struct PlannedSlot {
    pub task_id: String,
    pub condition_id: String,
}

fn planned_slots_for(slots: &[PlannedSlot], condition: &str) -> usize {
    slots
        .iter()
        .filter(|slot| slot.condition_id == condition)
        .count()
}

/// A trial's summarized server-side audit (`trial.json` `audit`), when one
/// was recorded. Older trial records without the field carry no audit
/// evidence: unknown, never "no tool use".
fn audit_of(trial: &TrialData) -> Option<&Value> {
    trial.trial.get("audit").filter(|audit| audit.is_object())
}

fn audit_counter(audit: &Value, path: &str) -> u64 {
    audit.pointer(path).and_then(Value::as_u64).unwrap_or(0)
}

/// The paired-task count a comparison recorded (0 when the comparison is
/// missing or carried no measured pair).
fn paired_task_count(paired: Option<&Value>, candidate: &str, metric: &str) -> u64 {
    paired
        .and_then(|value| value["comparisons"].as_array())
        .and_then(|comparisons| {
            comparisons.iter().find(|comparison| {
                comparison["candidate"].as_str() == Some(candidate)
                    && comparison["metric"].as_str() == Some(metric)
            })
        })
        .and_then(|comparison| comparison["pairedTasks"].as_u64())
        .unwrap_or(0)
}

/// The `reasons` array of an evidence record, joined for a gate check.
fn join_reasons(record: &Value) -> String {
    record["reasons"]
        .as_array()
        .map(|reasons| {
            reasons
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// Classify one attempted trial's audit evidence for a claim kind:
/// `supported` (observed proof), `refuted` (observed counter-evidence:
/// no search for `search-used`, degraded/fallback/non-hybrid searches
/// for `hybrid-executed`), `not_proven` (a clean observation that simply
/// does not prove the claim) or `unknown` (missing or truncated evidence —
/// never zero use, never support, never a refutation).
fn classify_claim_trial(kind: &str, audit: Option<&Value>) -> (&'static str, String) {
    let Some(audit) = audit else {
        return (
            "unknown",
            "no server-side audit summary was recorded on this trial: the executed retrieval \
             mode is unknown, never assumed"
                .to_string(),
        );
    };
    match audit["status"].as_str() {
        Some("observed") => {
            let succeeded = audit_counter(audit, "/searches/succeeded");
            let failed = audit_counter(audit, "/searches/failed");
            let degraded = audit_counter(audit, "/searches/degraded");
            let hybrid = audit_counter(audit, "/searches/effective/hybrid");
            let modes_total: u64 = audit
                .pointer("/searches/effective")
                .and_then(Value::as_object)
                .map(|modes| modes.values().filter_map(Value::as_u64).sum())
                .unwrap_or(0);
            let non_hybrid = modes_total.saturating_sub(hybrid);
            if kind == "hybrid-executed" {
                if degraded > 0 {
                    (
                        "refuted",
                        format!(
                            "{degraded} completed search(es) reported a degraded fallback: a \
                             hybrid-executed claim fails on observed fallback evidence"
                        ),
                    )
                } else if non_hybrid > 0 {
                    (
                        "refuted",
                        format!(
                            "{non_hybrid} completed search(es) reported a non-hybrid effective \
                             mode (a silent lexical fallback): a hybrid-executed claim fails on \
                             observed fallback evidence"
                        ),
                    )
                } else if hybrid > 0 && hybrid == succeeded {
                    (
                        "supported",
                        format!(
                            "all {hybrid} completed search(es) reported the hybrid effective \
                             mode with no degradation"
                        ),
                    )
                } else if succeeded == 0 {
                    (
                        "not_proven",
                        "searches were attempted but none completed: the hybrid mode was never \
                         observably executed"
                            .to_string(),
                    )
                } else {
                    (
                        "not_proven",
                        format!(
                            "{} completed search(es) recorded no effective mode: mode coverage \
                             is incomplete, the claim stays unproven",
                            succeeded.saturating_sub(modes_total)
                        ),
                    )
                }
            } else if succeeded + failed > 0 {
                (
                    "supported",
                    format!(
                        "the trial's server recorded {} search tool call(s)",
                        succeeded + failed
                    ),
                )
            } else {
                (
                    "unknown",
                    "audit status is observed but no search record was counted: inconsistent \
                     evidence stays unknown"
                        .to_string(),
                )
            }
        }
        Some("not_observed") => {
            if kind == "hybrid-executed" {
                (
                    "not_proven",
                    "a complete audit stream recorded no search call: a real offered-tool \
                     workflow outcome, but it cannot prove hybrid engine execution"
                        .to_string(),
                )
            } else {
                (
                    "refuted",
                    "a complete audit stream recorded no search tool call: the declared search \
                     was not exercised on a retrieval-relevant task"
                        .to_string(),
                )
            }
        }
        Some("incomplete") => (
            "unknown",
            "the audit stream never reached a clean server end: only the recorded tail is \
             known; the rest is unknown, never zero use"
                .to_string(),
        ),
        Some("unavailable") => (
            "unknown",
            "the trial's audit sink is absent or unreadable: missing evidence is unknown, never \
             zero use and never support"
                .to_string(),
        ),
        _ => (
            "unknown",
            "the audit summary carries an unrecognized status: unknown evidence stays unknown"
                .to_string(),
        ),
    }
}

/// Evaluates the declared evidence-sufficiency policy over the frozen plan
/// and summarizes the retrieval audit's condition fidelity (a completed
/// fast/correct subset must not certify a claim-bearing verdict while other
/// planned slots errored, stayed ungraded or silently degraded).
///
/// The quality metrics are untouched: raw counts, censored-timeout
/// semantics and every descriptive statistic stay exactly as recorded.
/// This decides only whether a `pass` may stand:
///
/// - coverage denominators are the frozen task/condition/repetition slots
///   (`slots`), never the surviving records; timed-out trials stay
///   metric-eligible (they keep their capped time), infra failures and
///   cancelled trials do not, and ungraded slots (including slots whose
///   record is missing) count against grading completeness — without ever
///   being converted into wrong answers;
/// - per condition, the server-side audit fidelity is reported
///   descriptively: observed tool use, clean no-search outcomes (a real
///   workflow outcome, kept in the primary comparison — never deleted),
///   incomplete/unavailable/missing audit, searches by effective mode and
///   degradation counts;
/// - predeclared execution claims (`gates.evidence.claims`) are evaluated
///   over attempted trials of retrieval-relevant tasks only (tasks
///   explicitly designated as retrieval controls — category `other` — and
///   baseline conditions are excluded): observed fallback or non-use
///   refutes a claim, missing or truncated evidence stays unknown and
///   keeps the claim unproven.
pub fn evidence_summary(
    trials: &[TrialData],
    tasks: &[TaskProvenance],
    conditions: &[Condition],
    gates: &Gates,
    slots: &[PlannedSlot],
    paired: Option<&Value>,
) -> Value {
    let policy = gates.evidence.clone();
    let min_trial = policy
        .as_ref()
        .map_or(1.0, |policy| policy.min_trial_coverage);
    let min_graded = policy
        .as_ref()
        .map_or(1.0, |policy| policy.min_graded_coverage);
    let min_paired = policy
        .as_ref()
        .map_or(1.0, |policy| policy.min_paired_task_fraction);
    let claims = policy.map(|policy| policy.claims).unwrap_or_default();

    let mut reasons: Vec<String> = Vec::new();

    let by_condition: Vec<Value> = conditions
        .iter()
        .map(|condition| {
            let planned = planned_slots_for(slots, &condition.id);
            let mut completed = 0usize;
            let mut timed_out = 0usize;
            let mut infra_failed = 0usize;
            let mut cancelled = 0usize;
            let mut not_started = 0usize;
            let mut graded = 0usize;
            let mut observed_use = 0usize;
            let mut clean_no_search = 0usize;
            let mut audit_incomplete = 0usize;
            let mut audit_unavailable = 0usize;
            let mut audit_missing = 0usize;
            let mut searches_succeeded = 0u64;
            let mut searches_failed = 0u64;
            let mut searches_degraded = 0u64;
            let mut malformed_records = 0u64;
            let mut effective_modes: BTreeMap<String, u64> = BTreeMap::new();
            for trial in trials
                .iter()
                .filter(|data| data.trial["conditionId"].as_str() == Some(condition.id.as_str()))
            {
                let status = trial.trial["status"].as_str().unwrap_or("unknown");
                match status {
                    "completed" => completed += 1,
                    _ if is_timed_out(&trial.trial) => timed_out += 1,
                    "error" | "infra_error" => infra_failed += 1,
                    "cancelled" => cancelled += 1,
                    _ => not_started += 1,
                }
                if matches!(grading_status(trial), Some("pass") | Some("fail")) {
                    graded += 1;
                }
                if condition.retrieval_mode == "repoise" && status != "not_started" {
                    match audit_of(trial).and_then(|audit| audit["status"].as_str()) {
                        None => audit_missing += 1,
                        Some("observed") => observed_use += 1,
                        Some("not_observed") => clean_no_search += 1,
                        Some("incomplete") => audit_incomplete += 1,
                        _ => audit_unavailable += 1,
                    }
                    if let Some(audit) = audit_of(trial) {
                        searches_succeeded += audit_counter(audit, "/searches/succeeded");
                        searches_failed += audit_counter(audit, "/searches/failed");
                        searches_degraded += audit_counter(audit, "/searches/degraded");
                        malformed_records += audit_counter(audit, "/malformedRecords");
                        if let Some(modes) =
                            audit.pointer("/searches/effective").and_then(Value::as_object)
                        {
                            for (mode, count) in modes {
                                *effective_modes.entry(mode.clone()).or_insert(0) +=
                                    count.as_u64().unwrap_or(0);
                            }
                        }
                    }
                }
            }
            let recorded = completed + timed_out + infra_failed + cancelled + not_started;
            let missing = planned.saturating_sub(recorded);
            let attempted = completed + timed_out + infra_failed + cancelled;
            let metric_eligible = completed + timed_out;
            let trial_coverage = if planned > 0 {
                metric_eligible as f64 / planned as f64
            } else {
                0.0
            };
            let graded_coverage = if planned > 0 {
                graded as f64 / planned as f64
            } else {
                0.0
            };
            let mut condition_reasons: Vec<String> = Vec::new();
            if planned == 0 {
                condition_reasons.push(format!(
                    "condition '{}': the frozen plan contains no planned slot for it, so no \
                     coverage denominator exists",
                    condition.id
                ));
            }
            if planned > 0 && trial_coverage < min_trial {
                condition_reasons.push(format!(
                    "condition '{}': only {} of {} planned trial slots are metric-eligible \
                     (completed or censored timeout; {} infra/adapter failure(s), {} cancelled, \
                     {} recorded not started, {} never recorded at all) — trial coverage \
                     {trial_coverage:.3} is below the declared minimum {min_trial:.3}",
                    condition.id,
                    metric_eligible,
                    planned,
                    infra_failed,
                    cancelled,
                    not_started,
                    missing
                ));
            }
            if planned > 0 && graded_coverage < min_graded {
                condition_reasons.push(format!(
                    "condition '{}': only {} of {} planned trial slots carry a pass/fail \
                     grading record — graded coverage {graded_coverage:.3} is below the \
                     declared minimum {min_graded:.3} (ungraded, evaluator-error and \
                     missing-outcome slots count against completeness; they are never \
                     converted into wrong answers)",
                    condition.id, graded, planned
                ));
            }
            let sufficient = condition_reasons.is_empty();
            reasons.extend(condition_reasons.iter().cloned());
            let is_repoise = condition.retrieval_mode == "repoise";
            serde_json::json!({
                "conditionId": condition.id,
                "retrievalMode": condition.retrieval_mode,
                "searchMode": condition.search_mode,
                "planned": planned,
                "recorded": recorded,
                "missing": missing,
                "attempted": attempted,
                "completed": completed,
                "timedOut": timed_out,
                "infraFailed": infra_failed,
                "cancelled": cancelled,
                "notStarted": not_started,
                "metricEligible": metric_eligible,
                "graded": graded,
                "trialCoverage": trial_coverage,
                "gradedCoverage": graded_coverage,
                "sufficient": sufficient,
                "reasons": condition_reasons,
                "observedUse": is_repoise.then_some(observed_use),
                "cleanNoSearch": is_repoise.then_some(clean_no_search),
                "auditIncomplete": is_repoise.then_some(audit_incomplete),
                "auditUnavailable": is_repoise.then_some(audit_unavailable),
                "auditMissing": is_repoise.then_some(audit_missing),
                "searches": is_repoise.then(|| serde_json::json!({
                    "succeeded": searches_succeeded,
                    "failed": searches_failed,
                    "degraded": searches_degraded,
                    "effective": effective_modes
                })),
                "malformedAuditRecords": is_repoise.then_some(malformed_records),
                "observedUseNote": is_repoise.then(|| "observedUse counts trials whose server-side audit recorded at least one search: a selection-biased subset reported separately — the primary workflow comparison always covers every assigned trial and never filters on observed tool use".to_string())
            })
        })
        .collect();

    let by_comparison: Vec<Value> = gates
        .improvements
        .iter()
        .map(|improvement| {
            let mut baseline_tasks: Vec<&str> = Vec::new();
            let mut candidate_tasks: Vec<&str> = Vec::new();
            for slot in slots {
                let tasks_with = if slot.condition_id == gates.baseline {
                    &mut baseline_tasks
                } else if slot.condition_id == improvement.candidate {
                    &mut candidate_tasks
                } else {
                    continue;
                };
                if !tasks_with.contains(&slot.task_id.as_str()) {
                    tasks_with.push(&slot.task_id);
                }
            }
            let planned_paired = baseline_tasks
                .iter()
                .filter(|task| candidate_tasks.contains(task))
                .count();
            let paired_tasks =
                paired_task_count(paired, &improvement.candidate, &improvement.metric);
            let fraction = if planned_paired > 0 {
                paired_tasks as f64 / planned_paired as f64
            } else {
                0.0
            };
            let mut comparison_reasons: Vec<String> = Vec::new();
            if planned_paired == 0 {
                comparison_reasons.push(format!(
                    "improvement {}:{}: the frozen plan declares no task slotted under both \
                     '{}' and '{}', so the paired comparison has no denominator",
                    improvement.candidate,
                    improvement.metric,
                    gates.baseline,
                    improvement.candidate
                ));
            } else if fraction < min_paired {
                comparison_reasons.push(format!(
                    "improvement {}:{}: only {} of {} planned tasks carry measured data under \
                     both '{}' and '{}' — paired task fraction {fraction:.3} is below the \
                     declared minimum {min_paired:.3}",
                    improvement.candidate,
                    improvement.metric,
                    paired_tasks,
                    planned_paired,
                    gates.baseline,
                    improvement.candidate
                ));
            }
            let sufficient = comparison_reasons.is_empty();
            reasons.extend(comparison_reasons.iter().cloned());
            serde_json::json!({
                "baseline": gates.baseline,
                "candidate": improvement.candidate,
                "metric": improvement.metric,
                "pairedTasks": paired_tasks,
                "plannedPairedTasks": planned_paired,
                "pairedTaskFraction": fraction,
                "sufficient": sufficient,
                "reasons": comparison_reasons
            })
        })
        .collect();

    // Tasks explicitly designated as retrieval controls (category `other`
    // is the declared escape hatch for tasks that need no retrieval) are
    // excluded from claim denominators: a claim about Repoise execution
    // can neither be carried nor refuted by a task that never needs
    // retrieval.
    let control_tasks: Vec<&str> = tasks
        .iter()
        .filter(|task| task.category.as_deref() == Some("other"))
        .map(|task| task.id.as_str())
        .collect();
    let claim_records: Vec<Value> = claims
        .iter()
        .map(|claim| {
            let relevant_planned = slots
                .iter()
                .filter(|slot| {
                    slot.condition_id == claim.condition
                        && !control_tasks.contains(&slot.task_id.as_str())
                })
                .count();
            let mut supported = 0usize;
            let mut refuted = 0usize;
            let mut not_proven = 0usize;
            let mut unknown = 0usize;
            let mut control_trials = 0usize;
            let mut attempted = 0usize;
            let mut trial_evidence: Vec<Value> = Vec::new();
            for trial in trials
                .iter()
                .filter(|data| data.trial["conditionId"].as_str() == Some(claim.condition.as_str()))
            {
                let status = trial.trial["status"].as_str().unwrap_or("unknown");
                if status == "not_started" {
                    continue;
                }
                let task_id = trial.trial["taskId"].as_str().unwrap_or("");
                if control_tasks.contains(&task_id) {
                    control_trials += 1;
                    continue;
                }
                attempted += 1;
                let (classification, note) = classify_claim_trial(&claim.kind, audit_of(trial));
                match classification {
                    "supported" => supported += 1,
                    "refuted" => refuted += 1,
                    "not_proven" => not_proven += 1,
                    _ => unknown += 1,
                }
                trial_evidence.push(serde_json::json!({
                    "trialId": trial.trial["trialId"],
                    "classification": classification,
                    "note": note
                }));
            }
            let verdict = if refuted > 0 {
                "not_supported"
            } else if supported > 0 && unknown == 0 && not_proven == 0 {
                "supported"
            } else {
                "insufficient_data"
            };
            let mut claim_reasons: Vec<String> = Vec::new();
            if refuted > 0 {
                claim_reasons.push(format!(
                    "claim ({}, '{}'): refuted by {} attempted trial(s) whose server-side audit \
                     observed fallback or non-use on retrieval-relevant tasks",
                    claim.kind, claim.condition, refuted
                ));
            }
            if attempted == 0 {
                claim_reasons.push(format!(
                    "claim ({}, '{}'): no attempted trial on a retrieval-relevant task, so the \
                     claim has no evidence either way",
                    claim.kind, claim.condition
                ));
            } else if supported > 0 && (unknown > 0 || not_proven > 0) {
                claim_reasons.push(format!(
                    "claim ({}, '{}'): partially proven — {} supported but {} not proven and {} \
                     unknown; unknown audit evidence stays unknown and cannot support the claim",
                    claim.kind, claim.condition, supported, not_proven, unknown
                ));
            } else if supported == 0 && refuted == 0 {
                claim_reasons.push(format!(
                    "claim ({}, '{}'): no attempted trial produced observed support ({} not \
                     proven, {} unknown)",
                    claim.kind, claim.condition, not_proven, unknown
                ));
            }
            let not_started_relevant = relevant_planned.saturating_sub(attempted);
            if not_started_relevant > 0 {
                claim_reasons.push(format!(
                    "claim ({}, '{}'): {} relevant planned slot(s) never started; the claim \
                     stands on attempted trials only",
                    claim.kind, claim.condition, not_started_relevant
                ));
            }
            if verdict != "supported" {
                reasons.extend(claim_reasons.iter().cloned());
            }
            serde_json::json!({
                "condition": claim.condition,
                "kind": claim.kind,
                "relevantPlannedSlots": relevant_planned,
                "attempted": attempted,
                "controlTaskTrials": control_trials,
                "supported": supported,
                "refuted": refuted,
                "notProven": not_proven,
                "unknown": unknown,
                "verdict": verdict,
                "reasons": claim_reasons,
                "trialEvidence": trial_evidence
            })
        })
        .collect();

    let sufficient = reasons.is_empty();
    serde_json::json!({
        "policy": {
            "minTrialCoverage": min_trial,
            "minGradedCoverage": min_graded,
            "minPairedTaskFraction": min_paired,
            "claims": claims
                .iter()
                .map(|claim| serde_json::json!({ "condition": claim.condition, "kind": claim.kind }))
                .collect::<Vec<Value>>(),
            "defaultsApplied": gates.evidence.is_none(),
            "note": "coverage divides by the frozen task/condition/repetition plan: missing \
                     records never shrink a denominator; unmet coverage yields \
                     'insufficient_data', never a pass, and never converts a missing outcome \
                     into a wrong answer, a zero or a deleted trial. A suite without \
                     gates.evidence carries these conservative 1.0 defaults; relax them \
                     explicitly to accept partial coverage"
        },
        "planned": slots.len(),
        "byCondition": by_condition,
        "byComparison": by_comparison,
        "claims": claim_records,
        "sufficient": sufficient,
        "reasons": reasons
    })
}

/// Evaluates the predeclared quality gate against the collected trials.
/// Checks are declared before the experiment; this only measures them.
/// `evidence` is the [`evidence_summary`] record for the same gate: its
/// coverage shortfalls and claim verdicts become gate checks
/// (`coverage:<condition>`, `paired-coverage:<candidate>:<metric>` and
/// `claim:<kind>:<condition>`), so a claim-bearing `pass` can never stand
/// on insufficient evidence: unmet coverage and unproven claims are
/// `insufficient_data`, observed fallback or no-use refutes the claim
/// (`failed`). The quality metrics above are untouched either way.
/// Overall status: `insufficient_data` when no check was declared or any
/// check could not be evaluated, `fail` when any evaluated check fails, and
/// `pass` only when every declared check was evaluated and passed.
pub fn quality_gate(
    trials: &[TrialData],
    conditions: &[Condition],
    gates: Option<&Gates>,
    paired: Option<&Value>,
    evidence: Option<&Value>,
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
    // Evidence validity checks (kept separate from the quality metrics
    // above): a coverage shortfall or an unproven execution claim makes
    // the claim-bearing verdict insufficient_data; observed fallback or
    // no-use refutes a claim (failed). None of these changes any measured
    // number: raw counts and descriptive statistics stand on their own.
    if let Some(evidence) = evidence {
        if let Some(entries) = evidence["byCondition"].as_array() {
            for entry in entries {
                checks.push(serde_json::json!({
                    "name": format!("coverage:{}", entry["conditionId"].as_str().unwrap_or("?")),
                    "observed": entry["trialCoverage"],
                    "threshold": evidence["policy"]["minTrialCoverage"],
                    "gradedObserved": entry["gradedCoverage"],
                    "gradedThreshold": evidence["policy"]["minGradedCoverage"],
                    "planned": entry["planned"],
                    "metricEligible": entry["metricEligible"],
                    "graded": entry["graded"],
                    "status": if entry["sufficient"].as_bool().unwrap_or(false) {
                        "passed"
                    } else {
                        "insufficient_data"
                    },
                    "reason": join_reasons(entry)
                }));
            }
        }
        if let Some(entries) = evidence["byComparison"].as_array() {
            for entry in entries {
                checks.push(serde_json::json!({
                    "name": format!(
                        "paired-coverage:{}:{}",
                        entry["candidate"].as_str().unwrap_or("?"),
                        entry["metric"].as_str().unwrap_or("?")
                    ),
                    "observed": entry["pairedTaskFraction"],
                    "threshold": evidence["policy"]["minPairedTaskFraction"],
                    "pairedTasks": entry["pairedTasks"],
                    "plannedPairedTasks": entry["plannedPairedTasks"],
                    "status": if entry["sufficient"].as_bool().unwrap_or(false) {
                        "passed"
                    } else {
                        "insufficient_data"
                    },
                    "reason": join_reasons(entry)
                }));
            }
        }
        if let Some(entries) = evidence["claims"].as_array() {
            for entry in entries {
                let verdict = entry["verdict"].as_str().unwrap_or("insufficient_data");
                checks.push(serde_json::json!({
                    "name": format!(
                        "claim:{}:{}",
                        entry["kind"].as_str().unwrap_or("?"),
                        entry["condition"].as_str().unwrap_or("?")
                    ),
                    "verdict": verdict,
                    "supported": entry["supported"],
                    "refuted": entry["refuted"],
                    "notProven": entry["notProven"],
                    "unknown": entry["unknown"],
                    "status": match verdict {
                        "supported" => "passed",
                        "not_supported" => "failed",
                        _ => "insufficient_data"
                    },
                    "reason": join_reasons(entry)
                }));
            }
        }
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
    if let Some(harness) = report["harness"].as_object()
        && harness["drift"].as_bool() == Some(true)
    {
        let versions = harness["versions"]
            .as_array()
            .map(|versions| {
                versions
                    .iter()
                    .filter_map(|version| version.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        md.push_str(&format!(
            "## Harness version drift\n\n- Completed trials ran on: {versions}.\n\n"
        ));
        if let Some(note) = harness["note"].as_str() {
            md.push_str(&format!("- {note}\n\n"));
        }
    }
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
    if let Some(evidence) = report["evidence"].as_object() {
        md.push_str("## Evidence sufficiency\n\n");
        md.push_str(
            "- A gate `pass` is claim-bearing only when these checks hold; unmet coverage is \
             `insufficient_data`, never a pass, and never converts a missing outcome into a \
             wrong answer, a zero or a deleted trial.\n",
        );
        let policy = &evidence["policy"];
        md.push_str(&format!(
            "- Policy: trial coverage >= {}, graded coverage >= {}, paired task fraction >= {}{}.\n\n",
            policy["minTrialCoverage"]
                .as_f64()
                .map(|v| format!("{v:.3}"))
                .unwrap_or("n/a".into()),
            policy["minGradedCoverage"]
                .as_f64()
                .map(|v| format!("{v:.3}"))
                .unwrap_or("n/a".into()),
            policy["minPairedTaskFraction"]
                .as_f64()
                .map(|v| format!("{v:.3}"))
                .unwrap_or("n/a".into()),
            if policy["defaultsApplied"].as_bool() == Some(true) {
                " (the suite declared no gates.evidence: conservative defaults applied)"
            } else {
                ""
            }
        ));
        if let Some(rows) = evidence["byCondition"].as_array() {
            md.push_str("| Condition | Planned | Completed | Timed out | Infra failed | Cancelled | Not started | Missing | Metric-eligible | Graded | Verdict |\n");
            md.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
            for row in rows {
                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                    row["conditionId"].as_str().unwrap_or("?"),
                    row["planned"].as_u64().unwrap_or(0),
                    row["completed"].as_u64().unwrap_or(0),
                    row["timedOut"].as_u64().unwrap_or(0),
                    row["infraFailed"].as_u64().unwrap_or(0),
                    row["cancelled"].as_u64().unwrap_or(0),
                    row["notStarted"].as_u64().unwrap_or(0),
                    row["missing"].as_u64().unwrap_or(0),
                    row["metricEligible"].as_u64().unwrap_or(0),
                    row["graded"].as_u64().unwrap_or(0),
                    if row["sufficient"].as_bool() == Some(true) {
                        "sufficient"
                    } else {
                        "INSUFFICIENT"
                    }
                ));
                if let Some(reasons) = row["reasons"].as_array() {
                    for reason in reasons.iter().filter_map(Value::as_str) {
                        md.push_str(&format!(
                            "- `{}`: {reason}\n",
                            row["conditionId"].as_str().unwrap_or("?")
                        ));
                    }
                }
            }
            md.push('\n');
        }
        if let Some(rows) = evidence["byComparison"].as_array() {
            for row in rows {
                md.push_str(&format!(
                    "- Paired coverage {} vs {} ({}): {}/{} planned tasks paired -> {}.\n",
                    row["candidate"].as_str().unwrap_or("?"),
                    row["baseline"].as_str().unwrap_or("?"),
                    row["metric"].as_str().unwrap_or("?"),
                    row["pairedTasks"].as_u64().unwrap_or(0),
                    row["plannedPairedTasks"].as_u64().unwrap_or(0),
                    if row["sufficient"].as_bool() == Some(true) {
                        "sufficient"
                    } else {
                        "INSUFFICIENT"
                    }
                ));
            }
            md.push('\n');
        }
        if let Some(claims) = evidence["claims"].as_array() {
            for claim in claims {
                md.push_str(&format!(
                    "- Claim `{}` on `{}`: {} ({} supported, {} refuted, {} not proven, {} unknown; {} attempted trial(s), {} control-task trial(s)).\n",
                    claim["kind"].as_str().unwrap_or("?"),
                    claim["condition"].as_str().unwrap_or("?"),
                    claim["verdict"].as_str().unwrap_or("?"),
                    claim["supported"].as_u64().unwrap_or(0),
                    claim["refuted"].as_u64().unwrap_or(0),
                    claim["notProven"].as_u64().unwrap_or(0),
                    claim["unknown"].as_u64().unwrap_or(0),
                    claim["attempted"].as_u64().unwrap_or(0),
                    claim["controlTaskTrials"].as_u64().unwrap_or(0)
                ));
                if let Some(reasons) = claim["reasons"].as_array() {
                    for reason in reasons.iter().filter_map(Value::as_str) {
                        md.push_str(&format!("  - {reason}\n"));
                    }
                }
            }
            md.push('\n');
        }
        md.push_str(&format!(
            "**Evidence sufficient for a claim-bearing pass:** {}\n\n",
            if evidence["sufficient"].as_bool() == Some(true) {
                "yes"
            } else {
                "no"
            }
        ));
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

    /// The frozen plan: one slot per (task, condition) for the given ids.
    fn slots_for(tasks: &[&str], condition_ids: &[&str]) -> Vec<PlannedSlot> {
        tasks
            .iter()
            .flat_map(|task| {
                condition_ids.iter().map(move |condition| PlannedSlot {
                    task_id: (*task).to_string(),
                    condition_id: (*condition).to_string(),
                })
            })
            .collect()
    }

    /// Gate over the standard two-task plan (task-a, task-b) with the
    /// declared gates and the evidence record computed from the same plan.
    fn gated(trials: &[TrialData], gates: &Gates, paired: Option<&Value>) -> Value {
        let slots = slots_for(&["task-a", "task-b"], &["baseline", "candidate"]);
        let evidence =
            evidence_summary(trials, &base_tasks(), &conditions(), gates, &slots, paired);
        quality_gate(trials, &conditions(), Some(gates), paired, Some(&evidence))
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
        let gate = gated(&trials, &gates, Some(&paired));
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
                "correctness-margin:candidate",
                "coverage:baseline",
                "coverage:candidate",
                "paired-coverage:candidate:agentMs"
            ]
        );
        assert_eq!(gate["checks"][2]["status"], "passed");
        assert_eq!(gate["checks"][2]["observed"], 0.5);
        assert_eq!(gate["checks"][1]["status"], "failed");
        assert_eq!(gate["checks"][3]["status"], "failed");
        // Fully covered plan: the evidence checks pass but cannot rescue a
        // failed quality metric.
        assert_eq!(gate["checks"][4]["status"], "passed");
        assert_eq!(gate["checks"][5]["status"], "passed");
        assert_eq!(gate["checks"][6]["status"], "passed");
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
            quality_gate(&trials, &[], None, None, None)["status"],
            "not_configured"
        );
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        // Nothing graded and no paired data for the candidate: the gate
        // must not pass.
        let gate = gated(&trials, &gates, Some(&paired));
        assert_eq!(gate["status"], "insufficient_data");
    }

    #[test]
    fn gate_is_insufficient_when_any_check_lacks_data() {
        // The candidate adapter errors on every trial: non-timeout failures
        // carry no measurement, so correctness, margin and the paired
        // reduction are all insufficient. The gate must not pass on partial
        // data.
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
        let gate = gated(&trials, &gates, Some(&paired));
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
                ("improvement:candidate:agentMs", "insufficient_data"),
                ("correctness-margin:candidate", "insufficient_data"),
                ("coverage:baseline", "passed"),
                ("coverage:candidate", "insufficient_data"),
                ("paired-coverage:candidate:agentMs", "insufficient_data"),
            ]
        );
    }

    #[test]
    fn partially_graded_candidate_cannot_pass_the_gate_on_fast_failures() {
        // A fast failure must not count as a fast success: the candidate
        // errors on task-a (50 ms) and passes task-b in 1900 ms against a
        // 2000 ms baseline. task-a drops out of the paired comparison (its
        // only candidate trial is non-eligible), the observed reduction is
        // 5%, below the 10% threshold, so the improvement check fails — and
        // the evidence checks independently mark the run insufficient: half
        // the planned candidate slots never produced a measurement.
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
                "completed",
                None,
                Some(1900),
                None,
                Some("pass"),
            ),
        ];
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let comparison = &paired["comparisons"][0];
        // task-a is excluded, with its excluded trial counted in the reason.
        assert_eq!(comparison["excludedTasks"][0]["taskId"], "task-a");
        assert!(
            comparison["excludedTasks"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("1 trial(s) excluded"),
            "{:?}",
            comparison["excludedTasks"][0]
        );
        assert_eq!(comparison["pairedTasks"], 1);
        assert!((comparison["medianPairedReduction"].as_f64().unwrap() - 0.05).abs() < 1e-9);

        let gate = gated(&trials, &gates, Some(&paired));
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
                ("correctness:candidate", "passed"),
                ("improvement:candidate:agentMs", "failed"),
                ("correctness-margin:candidate", "passed"),
                ("coverage:baseline", "passed"),
                ("coverage:candidate", "insufficient_data"),
                ("paired-coverage:candidate:agentMs", "insufficient_data"),
            ]
        );
        // The descriptive statistics are untouched by the evidence verdict:
        // the surviving paired reduction is still reported exactly.
        assert_eq!(gate["checks"][2]["observed"], 0.05);
    }

    #[test]
    fn large_surviving_gain_on_partial_coverage_is_insufficient_not_pass() {
        // The counterexample the old gate allowed through: the candidate
        // errors on task-a but beats the baseline by 50% on task-b. The
        // surviving paired reduction (0.5) clears the 10% threshold and the
        // graded trials are all correct, so every predeclared quality check
        // passes — but the surviving subset cannot certify the result: the
        // gate must report insufficient_data, never pass.
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
                "completed",
                None,
                Some(1000),
                None,
                Some("pass"),
            ),
        ];
        let gates = gates();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        assert_eq!(paired["comparisons"][0]["pairedTasks"], 1);
        assert!(
            (paired["comparisons"][0]["medianPairedReduction"]
                .as_f64()
                .unwrap()
                - 0.5)
                .abs()
                < 1e-9
        );
        let gate = gated(&trials, &gates, Some(&paired));
        assert_eq!(gate["status"], "insufficient_data");
        // Every predeclared quality check passes; the evidence checks are
        // what withhold the pass.
        let by_name: Vec<(&str, &str)> = gate["checks"]
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
            by_name,
            vec![
                ("correctness:baseline", "passed"),
                ("correctness:candidate", "passed"),
                ("improvement:candidate:agentMs", "passed"),
                ("correctness-margin:candidate", "passed"),
                ("coverage:baseline", "passed"),
                ("coverage:candidate", "insufficient_data"),
                ("paired-coverage:candidate:agentMs", "insufficient_data"),
            ]
        );
        // The unmeasured slot is named in the shortfall, and the raw counts
        // elsewhere in the report are unchanged.
        let coverage = &gate["checks"][5];
        assert_eq!(coverage["planned"], 2);
        assert_eq!(coverage["metricEligible"], 1);
        assert!(
            coverage["reason"]
                .as_str()
                .unwrap()
                .contains("infra/adapter failure")
        );
    }

    #[test]
    fn missing_trial_record_cannot_shrink_the_coverage_denominator() {
        // A planned slot whose trial.json never landed (or is unreadable)
        // stays planned: coverage divides by the frozen plan, so the record
        // cannot go missing its way into a pass.
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
                "task-a",
                "candidate",
                "completed",
                None,
                Some(1000),
                None,
                Some("pass"),
            ),
        ];
        let gates = gates();
        let slots = slots_for(&["task-a", "task-b"], &["baseline", "candidate"]);
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let evidence = evidence_summary(
            &trials,
            &base_tasks(),
            &conditions(),
            &gates,
            &slots,
            Some(&paired),
        );
        assert_eq!(evidence["planned"], 4);
        assert_eq!(evidence["byCondition"][0]["missing"], 1);
        assert_eq!(evidence["byCondition"][0]["trialCoverage"], 0.5);
        assert_eq!(evidence["byCondition"][0]["sufficient"], false);
        assert_eq!(evidence["byCondition"][1]["missing"], 1);
        assert_eq!(evidence["sufficient"], false);
        let gate = quality_gate(
            &trials,
            &conditions(),
            Some(&gates),
            Some(&paired),
            Some(&evidence),
        );
        assert_eq!(gate["status"], "insufficient_data");
    }

    #[test]
    fn ungraded_trials_and_unrun_tasks_hold_the_verdict_insufficient() {
        // All slots completed and are fast, but grading records are missing
        // for one task and one planned task never ran: correctness
        // denominators stay honest (no wrong answers are invented), and the
        // graded-coverage policy withholds the pass.
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
                "task-a",
                "candidate",
                "completed",
                None,
                Some(1000),
                None,
                None, // no grading record at all
            ),
        ];
        let gates = gates();
        let slots = slots_for(&["task-a", "task-b"], &["baseline", "candidate"]);
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let evidence = evidence_summary(
            &trials,
            &base_tasks(),
            &conditions(),
            &gates,
            &slots,
            Some(&paired),
        );
        // task-b was planned for both conditions but never recorded.
        assert_eq!(evidence["byCondition"][0]["missing"], 1);
        assert_eq!(evidence["byCondition"][1]["graded"], 0);
        assert_eq!(evidence["byCondition"][1]["gradedCoverage"], 0.0);
        let gate = quality_gate(
            &trials,
            &conditions(),
            Some(&gates),
            Some(&paired),
            Some(&evidence),
        );
        assert_eq!(gate["status"], "insufficient_data");
        // The ungraded completed trial is still not a correctness failure.
        let condition_grading = &gate["checks"][1];
        assert_eq!(condition_grading["status"], "insufficient_data");
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
        // One planned task (task-a), both conditions completed and graded:
        // the evidence is sufficient and the gate passes.
        let slots = slots_for(&["task-a"], &["baseline", "candidate"]);
        let evidence = evidence_summary(
            &trials,
            &base_tasks(),
            &conditions(),
            &gates,
            &slots,
            Some(&paired),
        );
        assert_eq!(evidence["sufficient"], true);
        let gate = quality_gate(
            &trials,
            &conditions(),
            Some(&gates),
            Some(&paired),
            Some(&evidence),
        );
        let report = json!({
            "experimentId": "exp-test",
            "status": "completed",
            "startedAtUtc": "2026-01-01T00:00:00Z",
            "endedAtUtc": "2026-01-01T00:01:00Z",
            "durationMs": 60000,
            "counts": counts(&trials, 2),
            "grading": grading_summary(&trials, &base_tasks()),
            "analysis": paired,
            "evidence": evidence,
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
        assert!(md.contains("## Evidence sufficiency"));
        assert!(md.contains("**Evidence sufficient for a claim-bearing pass:** yes"));
    }

    #[test]
    fn markdown_labels_harness_version_drift_with_plain_versions() {
        let mut trials = vec![
            trial(
                "task-a",
                "baseline",
                "completed",
                None,
                Some(1000),
                Some(10.0),
                Some("pass"),
            ),
            trial(
                "task-a",
                "candidate",
                "completed",
                None,
                Some(900),
                Some(9.0),
                Some("pass"),
            ),
        ];
        // Observed versions differ across completed trials (an unpinned run).
        trials[0].trial["harnessVersion"] = json!("3.0.74");
        trials[1].trial["harnessVersion"] = json!("1.0.0-stub");
        let report = json!({
            "experimentId": "exp-drift",
            "status": "completed",
            "startedAtUtc": "2026-01-01T00:00:00Z",
            "endedAtUtc": "2026-01-01T00:01:00Z",
            "durationMs": 60000,
            "counts": counts(&trials, 2),
            "grading": grading_summary(&trials, &base_tasks()),
            "harness": harness_drift(&trials),
            "qualityGate": quality_gate(&trials, &conditions(), None, None, None)
        });
        let md = render_markdown(&report);
        // The version list must be plain strings: `Value::to_string()` keeps
        // the JSON quotes and would render `"1.0.0-stub", "3.0.74"`.
        assert!(md.contains("## Harness version drift"));
        assert!(md.contains("- Completed trials ran on: 1.0.0-stub, 3.0.74."));
        assert!(!md.contains("\"1.0.0-stub\""));
        assert!(!md.contains("\"3.0.74\""));
        // The note below the list uses the same plain versions.
        assert!(md.contains("different harness versions (1.0.0-stub, 3.0.74)"));
        // A single observed version is not drift: no section at all.
        let mut single = vec![trial(
            "task-a",
            "baseline",
            "completed",
            None,
            Some(1000),
            Some(10.0),
            Some("pass"),
        )];
        single[0].trial["harnessVersion"] = json!("3.0.74");
        let single_report = json!({
            "experimentId": "exp-single",
            "status": "completed",
            "startedAtUtc": "2026-01-01T00:00:00Z",
            "endedAtUtc": "2026-01-01T00:01:00Z",
            "durationMs": 60000,
            "counts": counts(&single, 1),
            "grading": grading_summary(&single, &base_tasks()),
            "harness": harness_drift(&single),
            "qualityGate": quality_gate(&single, &conditions(), None, None, None)
        });
        let single_md = render_markdown(&single_report);
        assert!(!single_md.contains("## Harness version drift"));
    }

    #[test]
    fn host_watchdog_kill_is_a_censored_timeout() {
        // The outer watchdog force-kills the bridge: with no result.json
        // the trial is recorded as `infra_error` (not `error`) with reason
        // `host_watchdog`. It must still count as a timed-out trial and
        // contribute its recorded (capped) agentMs to the paired metric.
        let trials = vec![
            trial(
                "task-a",
                "baseline",
                "infra_error",
                Some("host_watchdog"),
                Some(900_000),
                None,
                None,
            ),
            trial(
                "task-a",
                "candidate",
                "completed",
                None,
                Some(1_000),
                None,
                Some("pass"),
            ),
        ];
        let counts = counts(&trials, 2);
        assert_eq!(counts["timedOut"], 1);
        assert_eq!(counts["invalid"], 0);
        assert_eq!(counts["completed"], 1);

        let paired = paired_analysis(&trials, &base_tasks(), &gates(), &report_spec());
        let reduction = paired["comparisons"][0]["medianPairedReduction"]
            .as_f64()
            .unwrap();
        assert!((reduction - (900_000.0 - 1_000.0) / 900_000.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_declared_metric_is_named_not_panicked() {
        // A hand-edited manifest can declare an unsupported gate metric. The
        // comparison names it per task and the gate check is
        // insufficient_data, never a panic.
        let trials = vec![trial(
            "task-a",
            "baseline",
            "completed",
            None,
            Some(1_000),
            None,
            Some("pass"),
        )];
        let gates: Gates = serde_json::from_value(json!({
            "baseline": "baseline",
            "minCorrectness": 1.0,
            "improvements": [{ "candidate": "candidate", "metric": "tokens", "threshold": 0.10 }]
        }))
        .unwrap();
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let comparison = &paired["comparisons"][0];
        assert_eq!(comparison["pairedTasks"], 0);
        assert!(
            comparison["excludedTasks"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("not a supported metric")
        );
        let gate = gated(&trials, &gates, Some(&paired));
        assert_eq!(gate["status"], "insufficient_data");
    }

    /// Gates that predeclare one execution claim on the candidate.
    fn claim_gates(kind: &str) -> Gates {
        serde_json::from_value(json!({
            "baseline": "baseline",
            "minCorrectness": 1.0,
            "improvements": [{ "candidate": "candidate", "metric": "agentMs", "threshold": 0.10 }],
            "evidence": { "claims": [{ "condition": "candidate", "kind": kind }] }
        }))
        .unwrap()
    }

    /// Fully covered two-task run (all completed, all graded); the task-a
    /// candidate trial carries `audit` when given. task-b is a category
    /// "other" retrieval control: its trial can neither carry nor refute a
    /// claim.
    fn covered_run_with_audit(audit: Option<Value>) -> Vec<TrialData> {
        let mut trials = vec![
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
                Some("pass"),
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
        if let Some(audit) = audit {
            trials[2].trial["audit"] = audit;
        }
        trials
    }

    fn verdict_for(kind: &str, audit: Option<Value>) -> (Value, Value) {
        let trials = covered_run_with_audit(audit);
        let gates = claim_gates(kind);
        let slots = slots_for(&["task-a", "task-b"], &["baseline", "candidate"]);
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let evidence = evidence_summary(
            &trials,
            &base_tasks(),
            &conditions(),
            &gates,
            &slots,
            Some(&paired),
        );
        let gate = quality_gate(
            &trials,
            &conditions(),
            Some(&gates),
            Some(&paired),
            Some(&evidence),
        );
        (evidence["claims"][0].clone(), gate)
    }

    #[test]
    fn search_used_claim_is_supported_only_by_observed_server_calls() {
        let (claim, gate) = verdict_for(
            "search-used",
            Some(json!({
                "status": "observed",
                "searches": { "attempted": 2, "succeeded": 2, "failed": 0, "degraded": 0,
                              "effective": { "lexical": 2 } }
            })),
        );
        assert_eq!(claim["verdict"], "supported");
        assert_eq!(claim["supported"], 1);
        assert_eq!(claim["controlTaskTrials"], 1);
        assert_eq!(gate["status"], "pass");
    }

    #[test]
    fn clean_no_search_trial_refutes_a_search_used_claim() {
        // The audit stream was complete: the agent never called the offered
        // tool on the retrieval-relevant task. The trial stays in the
        // primary comparison, but the claim is refuted and the gate fails.
        let (claim, gate) = verdict_for("search-used", Some(json!({ "status": "not_observed" })));
        assert_eq!(claim["verdict"], "not_supported");
        assert_eq!(claim["refuted"], 1);
        let claim_check = gate["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "claim:search-used:candidate")
            .unwrap();
        assert_eq!(claim_check["status"], "failed");
        assert_eq!(gate["status"], "fail");
    }

    #[test]
    fn missing_or_truncated_audit_evidence_stays_unknown() {
        // No audit recorded (older/failed sink): unknown, never zero use.
        let (claim, gate) = verdict_for("search-used", None);
        assert_eq!(claim["verdict"], "insufficient_data");
        assert_eq!(claim["unknown"], 1);
        assert_eq!(claim["refuted"], 0);
        assert_eq!(gate["status"], "insufficient_data");
        // Truncated stream: the recorded tail is all that is known.
        let (claim, gate) = verdict_for("search-used", Some(json!({ "status": "incomplete" })));
        assert_eq!(claim["verdict"], "insufficient_data");
        assert_eq!(claim["unknown"], 1);
        assert_eq!(gate["status"], "insufficient_data");
        // Unreadable sink: same unknown treatment.
        let (claim, _) = verdict_for("search-used", Some(json!({ "status": "unavailable" })));
        assert_eq!(claim["verdict"], "insufficient_data");
        assert_eq!(claim["unknown"], 1);
    }

    #[test]
    fn hybrid_claim_requires_actual_hybrid_searches_not_pooled_fallback() {
        let hybrid_audit = |effective: Value, degraded: u64, succeeded: u64| {
            json!({
                "status": "observed",
                "searches": { "attempted": succeeded, "succeeded": succeeded, "failed": 0,
                              "degraded": degraded, "effective": effective }
            })
        };
        // All searches observably hybrid: supported.
        let (claim, gate) = verdict_for(
            "hybrid-executed",
            Some(hybrid_audit(json!({ "hybrid": 2 }), 0, 2)),
        );
        assert_eq!(claim["verdict"], "supported");
        assert_eq!(gate["status"], "pass");
        // A silently pooled lexical search inside a "hybrid" condition:
        // refuted on observed fallback evidence.
        let (claim, gate) = verdict_for(
            "hybrid-executed",
            Some(hybrid_audit(json!({ "hybrid": 1, "lexical": 1 }), 0, 2)),
        );
        assert_eq!(claim["verdict"], "not_supported");
        assert_eq!(claim["refuted"], 1);
        assert_eq!(gate["status"], "fail");
        // A degraded (fallback) search: refuted.
        let (claim, _) = verdict_for(
            "hybrid-executed",
            Some(hybrid_audit(json!({ "hybrid": 1 }), 1, 2)),
        );
        assert_eq!(claim["verdict"], "not_supported");
        // A clean no-search workflow outcome cannot prove hybrid execution,
        // but it is a real workflow outcome: not proven, not refuted, and
        // the trial is never dropped from the primary comparison.
        let (claim, gate) =
            verdict_for("hybrid-executed", Some(json!({ "status": "not_observed" })));
        assert_eq!(claim["verdict"], "insufficient_data");
        assert_eq!(claim["notProven"], 1);
        assert_eq!(claim["refuted"], 0);
        assert_eq!(gate["status"], "insufficient_data");
        // Unknown audit stays unknown.
        let (claim, _) = verdict_for("hybrid-executed", None);
        assert_eq!(claim["verdict"], "insufficient_data");
        assert_eq!(claim["unknown"], 1);
    }

    #[test]
    fn declared_relaxed_coverage_policy_may_accept_partial_coverage() {
        // The suite may explicitly predeclare the coverage it accepts. With
        // 0.5 thresholds the single measured pair carries the run, and the
        // shortfall stays visible in the evidence record (coverage values
        // and reasons are still reported).
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
                "completed",
                None,
                Some(1000),
                None,
                Some("pass"),
            ),
        ];
        let gates: Gates = serde_json::from_value(json!({
            "baseline": "baseline",
            "minCorrectness": 1.0,
            "improvements": [{ "candidate": "candidate", "metric": "agentMs", "threshold": 0.10 }],
            "evidence": {
                "minTrialCoverage": 0.5,
                "minGradedCoverage": 0.5,
                "minPairedTaskFraction": 0.5
            }
        }))
        .unwrap();
        let slots = slots_for(&["task-a", "task-b"], &["baseline", "candidate"]);
        let paired = paired_analysis(&trials, &base_tasks(), &gates, &report_spec());
        let evidence = evidence_summary(
            &trials,
            &base_tasks(),
            &conditions(),
            &gates,
            &slots,
            Some(&paired),
        );
        assert_eq!(evidence["policy"]["defaultsApplied"], false);
        assert_eq!(evidence["byCondition"][1]["trialCoverage"], 0.5);
        assert_eq!(evidence["byCondition"][1]["sufficient"], true);
        let gate = quality_gate(
            &trials,
            &conditions(),
            Some(&gates),
            Some(&paired),
            Some(&evidence),
        );
        assert_eq!(gate["status"], "pass");
    }

    #[test]
    fn percentile_of_an_empty_draw_is_nan() {
        // Defense in depth: an empty draw must not panic (usize underflow
        // in debug, out-of-bounds index in release) and must serialize to
        // JSON null in the interval object.
        assert!(percentile(&[], 2.5).is_nan());
        assert!(
            serde_json::to_string(&serde_json::json!({
                "low": percentile(&[], 2.5)
            }))
            .unwrap()
            .contains("null")
        );
    }
}
