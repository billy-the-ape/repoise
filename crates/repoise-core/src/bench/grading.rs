//! Independent post-trial grading (AP-05).
//!
//! Grading runs after the agent interval, outside agent visibility, and is
//! data-only: it inspects the recorded final answer (a field of the final
//! event's `data` object) and declared artifact files as text. Transcripts
//! and repository content are untrusted data — nothing read here is ever
//! executed.
//!
//! Outcomes per trial: `pass`, `fail`, `not_graded` (no evaluator declared,
//! a declared human/model method whose result must be written as a manual
//! grading record, or a censored trial status that is not a completed
//! answer) and `evaluator_error` (the engine could not evaluate the checks).
//! A trial's own assertions, exit code or process status are never treated
//! as correctness.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::artifacts;
use super::runner::now_utc;
use super::suite::Task;

/// Grading record schema version (checked-in: `schemas/bench-grading.v1.schema.json`).
pub const GRADING_SCHEMA_VERSION: u32 = 1;
/// Checked-in schema file for the grading record.
pub const GRADING_SCHEMA_FILE: &str = "schemas/bench-grading.v1.schema.json";

/// One grading result for a trial.
#[derive(Clone, Debug)]
pub struct Grading {
    /// `pass`, `fail`, `not_graded` or `evaluator_error`.
    pub status: String,
    /// `deterministic`, `humanRubric`, `modelJudge` or `none`.
    pub method: String,
    pub reason: Option<String>,
    /// Per-check results (deterministic grading only).
    pub checks: Vec<Value>,
    /// Whether the declared answer field was present in the final event.
    pub answer_present: bool,
}

impl Grading {
    fn not_graded(method: &str, reason: &str) -> Self {
        Self {
            status: "not_graded".into(),
            method: method.into(),
            reason: Some(reason.into()),
            checks: Vec::new(),
            answer_present: false,
        }
    }
}

/// The one trial grading record (`trials/<id>/grading.json`).
pub fn record(
    trial_id: &str,
    task_id: &str,
    condition_id: &str,
    repetition: u32,
    grading: &Grading,
) -> Value {
    serde_json::json!({
        "schemaVersion": GRADING_SCHEMA_VERSION,
        "trialId": trial_id,
        "taskId": task_id,
        "conditionId": condition_id,
        "repetition": repetition,
        "status": grading.status,
        "method": grading.method,
        "reason": grading.reason.clone(),
        "checks": grading.checks.clone(),
        "answerPresent": grading.answer_present,
        "gradedAtUtc": now_utc(),
        "source": "engine",
        "instrumentation": format!("repoise {} bench-grading v{}", env!("CARGO_PKG_VERSION"), GRADING_SCHEMA_VERSION)
    })
}

/// Reads the stored grading record when present.
pub fn load(trial_dir: &Path) -> Option<Value> {
    artifacts::read_json(&trial_dir.join("grading.json")).ok()
}

/// Validates an operator-written manual grading record: schema version, a
/// pass/fail status, the method matching the task's declared evaluator
/// method, and a non-empty reason.
pub fn manual_grading_is_valid(raw: &Value, expected_method: &str) -> bool {
    raw.get("schemaVersion").and_then(Value::as_u64) == Some(GRADING_SCHEMA_VERSION as u64)
        && matches!(
            raw.get("status").and_then(Value::as_str),
            Some("pass") | Some("fail")
        )
        && raw
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| method == expected_method)
        && raw
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.trim().is_empty())
}

/// A minimal `evaluator_error` record written when the grading engine
/// itself fails (missing or corrupt evidence, an invalid manual record).
pub fn evaluator_error_record(
    trial_id: &str,
    task_id: &str,
    condition_id: &str,
    repetition: u32,
    method: &str,
    reason: &str,
) -> Value {
    record_fields(
        trial_id,
        task_id,
        condition_id,
        repetition,
        &RecordBody {
            status: "evaluator_error".into(),
            method: method.into(),
            reason: Some(reason.to_string()),
            checks: Vec::new(),
            answer_present: false,
        },
    )
}

fn record_fields(
    trial_id: &str,
    task_id: &str,
    condition_id: &str,
    repetition: u32,
    body: &RecordBody,
) -> Value {
    serde_json::json!({
        "schemaVersion": GRADING_SCHEMA_VERSION,
        "trialId": trial_id,
        "taskId": task_id,
        "conditionId": condition_id,
        "repetition": repetition,
        "status": body.status,
        "method": body.method,
        "reason": body.reason,
        "checks": body.checks,
        "answerPresent": body.answer_present,
        "gradedAtUtc": now_utc(),
        "source": "engine",
        "instrumentation": format!("repoise {} bench-grading v{}", env!("CARGO_PKG_VERSION"), GRADING_SCHEMA_VERSION)
    })
}

/// The grading-specific payload shared by `record` and `evaluator_error_record`.
struct RecordBody {
    status: String,
    method: String,
    reason: Option<String>,
    checks: Vec<Value>,
    answer_present: bool,
}

/// Grades one trial for its task and writes `grading.json`.
///
/// Manual grading records (human rubric / model judge) are honored when
/// valid and kept unchanged; everything else is (re)computed. A `Result::Err`
/// means the engine itself failed and the caller records `evaluator_error`.
pub fn grade_and_write(
    task: &Task,
    trial_dir: &Path,
    trial_id: &str,
    condition_id: &str,
    repetition: u32,
    trial_status: &str,
    trial_reason: Option<&str>,
) -> Result<Grading, String> {
    let adapter_dir = trial_dir.join("adapter");
    let grading = match task.evaluator.as_ref() {
        None => Grading::not_graded(
            "none",
            &format!("no evaluator declared for task '{}'", task.id),
        ),
        Some(evaluator) => match evaluator.method.as_str() {
            "humanRubric" | "modelJudge" => {
                let method = evaluator.method.as_str();
                if trial_status != "completed" {
                    // A manual result can only stand behind a completed trial.
                    let reason = match trial_reason {
                        Some(reason) => format!("{trial_status} ({reason})"),
                        None => trial_status.to_string(),
                    };
                    Grading::not_graded(
                        method,
                        &format!(
                            "trial status '{reason}' is censored: manual grading records \
                             apply to completed trials only"
                        ),
                    )
                } else {
                    // Engine-written records (censored slots, pending manual
                    // state) are replaced; only operator-written records are
                    // validated and kept.
                    let existing = load(trial_dir)
                        .filter(|raw| raw.get("source").and_then(Value::as_str) != Some("engine"));
                    match existing {
                        Some(raw) if manual_grading_is_valid(&raw, method) => Grading {
                            status: raw["status"].as_str().unwrap_or("not_graded").into(),
                            method: method.into(),
                            reason: raw["reason"].as_str().map(str::to_string),
                            checks: Vec::new(),
                            answer_present: false,
                        },
                        Some(_) => {
                            return Err(format!(
                                "existing grading.json is not a valid manual grading record \
                             (expected method '{method}', status pass/fail and a non-empty reason)"
                            ));
                        }
                        None => match method {
                            "humanRubric" => Grading::not_graded(
                                method,
                                "human rubric review pending: the operator must write \
                             trials/<id>/grading.json (method humanRubric, status pass/fail, \
                             reason) before the trial can be graded",
                            ),
                            "modelJudge" => Grading::not_graded(
                                method,
                                "model judge declared but not run: this build has no model-judge \
                             runtime, and grading must stay independent of the tested model \
                             (see the AP-05 plan); the result must be written as a manual \
                             grading record",
                            ),
                            _ => unreachable!("validated method"),
                        },
                    }
                }
            }
            "deterministic" => {
                if trial_status != "completed" {
                    let reason = match trial_reason {
                        Some(reason) => format!("{trial_status} ({reason})"),
                        None => trial_status.to_string(),
                    };
                    Grading::not_graded(
                        "deterministic",
                        &format!(
                            "trial status '{reason}' is censored: only completed trials \
                             carry a completed answer; timeouts, cancellations and errors \
                             are not graded as wrong answers"
                        ),
                    )
                } else {
                    let answer = extract_final_answer(&adapter_dir, &evaluator.answer_field)?;
                    let (checks, all_passed) =
                        evaluate_checks(&evaluator.checks, answer.as_deref(), &adapter_dir);
                    Grading {
                        status: if all_passed { "pass" } else { "fail" }.into(),
                        method: "deterministic".into(),
                        reason: if all_passed {
                            None
                        } else {
                            Some("one or more deterministic checks failed".into())
                        },
                        checks,
                        answer_present: answer.is_some(),
                    }
                }
            }
            other => {
                return Err(format!(
                    "evaluator method '{other}' is not a validated method"
                ));
            }
        },
    };
    artifacts::write_json(
        &trial_dir.join("grading.json"),
        &record(
            trial_id,
            task.id.as_str(),
            condition_id,
            repetition,
            &grading,
        ),
    )
    .map_err(|err| err.to_string())?;
    Ok(grading)
}

/// Reads the final event of the adapter event log and returns the declared
/// answer field when it is a string.
fn extract_final_answer(adapter_dir: &Path, answer_field: &str) -> Result<Option<String>, String> {
    let events_path = adapter_dir.join("events.ndjson");
    let raw = std::fs::read_to_string(&events_path)
        .map_err(|err| format!("adapter event log unreadable: {err}"))?;
    let mut final_event: Option<Value> = None;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(event) if event.get("t").and_then(Value::as_str) == Some("final") => {
                final_event = Some(event);
            }
            Ok(_) => {}
            Err(err) => {
                return Err(format!("adapter event log line is not valid JSON: {err}"));
            }
        }
    }
    let final_event = final_event.ok_or("no final event in the adapter event log")?;
    Ok(final_event
        .pointer(&format!("/data/{answer_field}"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// Runs the deterministic checks. Returns the per-check records and whether
/// every check passed.
fn evaluate_checks(
    checks: &[super::suite::Check],
    answer: Option<&str>,
    adapter_dir: &Path,
) -> (Vec<Value>, bool) {
    let mut records: Vec<Value> = Vec::new();
    let mut all_passed = true;
    for check in checks {
        let haystack: Option<String> = match check.scope.as_str() {
            "answer" => answer.map(str::to_string),
            "artifact" => {
                let name = check.artifact.as_deref().expect("validated artifact");
                std::fs::read_to_string(adapter_dir.join(name)).ok()
            }
            other => unreachable!("validated scope: {other}"),
        };
        let passed = match check.kind.as_str() {
            "containsAny" => haystack
                .as_deref()
                .is_some_and(|text| check.values.iter().any(|value| text.contains(value))),
            "containsAll" => haystack
                .as_deref()
                .is_some_and(|text| check.values.iter().all(|value| text.contains(value))),
            "notContains" => haystack.as_deref().is_none_or(|text| {
                !check
                    .values
                    .iter()
                    .any(|value| text.contains(value.as_str()))
            }),
            other => unreachable!("validated check type: {other}"),
        };
        all_passed = all_passed && passed;
        let detail = match check.kind.as_str() {
            "containsAny" if passed => format!(
                "matched one of: {}",
                check
                    .values
                    .iter()
                    .filter(|value| {
                        haystack
                            .as_deref()
                            .is_some_and(|text| text.contains(value.as_str()))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "containsAny" => evidence_detail(&check.scope, &check.artifact),
            "containsAll" if passed => "all required substrings present".into(),
            "containsAll" => {
                let missing: Vec<String> = check
                    .values
                    .iter()
                    .filter(|value| {
                        haystack
                            .as_deref()
                            .is_none_or(|text| !text.contains(value.as_str()))
                    })
                    .cloned()
                    .collect();
                if missing.is_empty() {
                    evidence_detail(&check.scope, &check.artifact)
                } else {
                    format!("missing: {}", missing.join(", "))
                }
            }
            "notContains" if passed => "no forbidden substring present".into(),
            "notContains" => {
                let found: Vec<String> = check
                    .values
                    .iter()
                    .filter(|value| {
                        haystack
                            .as_deref()
                            .is_some_and(|text| text.contains(value.as_str()))
                    })
                    .cloned()
                    .collect();
                format!("found forbidden: {}", found.join(", "))
            }
            other => unreachable!("validated check type: {other}"),
        };
        records.push(serde_json::json!({
            "id": check.id,
            "type": check.kind,
            "scope": check.scope,
            "artifact": check.artifact,
            "result": if passed { "pass" } else { "fail" },
            "detail": detail,
            "note": check.note
        }));
    }
    (records, all_passed)
}

fn evidence_detail(scope: &str, artifact: &Option<String>) -> String {
    match (scope, artifact) {
        ("answer", _) => {
            "no final answer recorded (the declared answer field was absent or not a string)".into()
        }
        ("artifact", Some(artifact)) => {
            format!("artifact '{artifact}' was not produced in the adapter directory")
        }
        _ => "no evidence available".into(),
    }
}

/// The path of a trial's grading file.
pub fn grading_path(trial_dir: &Path) -> PathBuf {
    trial_dir.join("grading.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::suite::Evaluator;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "repoise-bench-grading-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn task_with(evaluator: Option<Evaluator>) -> Task {
        Task {
            id: "t1".into(),
            prompt: Some("what is the cause?".into()),
            prompt_file: None,
            version: "1".into(),
            category: None,
            held_out: false,
            allowed_paths: Vec::new(),
            expected_artifacts: Vec::new(),
            evaluator,
        }
    }

    fn write_events(trial_dir: &Path, final_data: &Value) {
        let adapter = trial_dir.join("adapter");
        std::fs::create_dir_all(&adapter).unwrap();
        let hello = serde_json::json!({"v": 1, "t": "hello", "ts": 0});
        let final_event = serde_json::json!({"v": 1, "t": "final", "ts": 1, "status": "completed", "data": final_data});
        let raw = format!(
            "{}\n{}\n",
            serde_json::to_string(&hello).unwrap(),
            serde_json::to_string(&final_event).unwrap()
        );
        std::fs::write(adapter.join("events.ndjson"), raw).unwrap();
    }

    fn deterministic_evaluator(checks: Vec<serde_json::Value>) -> Evaluator {
        let mut value = serde_json::json!({
            "method": "deterministic",
            "answerField": "answer",
            "checks": checks
        });
        let _ = &mut value;
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn deterministic_checks_pass_and_fail_on_substring_evidence() {
        let dir = temp_dir("checks");
        let trial = dir.join("trial");
        write_events(
            &trial,
            &serde_json::json!({ "answer": "the crash comes from the missing null check in parse_header" }),
        );
        let task = task_with(Some(deterministic_evaluator(vec![
            serde_json::json!({"id": "cause", "type": "containsAny", "values": ["null check", "missing pointer"]}),
            serde_json::json!({"id": "location", "type": "containsAll", "values": ["parse_header"]}),
            serde_json::json!({"id": "no-hallucination", "type": "notContains", "values": ["gpu driver"]}),
        ])));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None)
            .expect("grading succeeds");
        assert_eq!(grading.status, "pass");
        assert!(grading.answer_present);
        let stored = load(&trial).expect("grading.json written");
        assert_eq!(stored["status"], "pass");
        assert_eq!(stored["schemaVersion"], GRADING_SCHEMA_VERSION);

        // The same checks fail when the answer names the wrong cause.
        let trial = dir.join("trial-wrong");
        write_events(
            &trial,
            &serde_json::json!({ "answer": "the crash is a gpu driver fault" }),
        );
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None)
            .expect("grading succeeds");
        assert_eq!(grading.status, "fail");
        let failed: Vec<&str> = grading
            .checks
            .iter()
            .filter(|check| check["result"] == "fail")
            .map(|check| check["id"].as_str().unwrap())
            .collect();
        assert_eq!(failed, vec!["cause", "location", "no-hallucination"]);
    }

    #[test]
    fn contains_any_accepts_alternative_correct_evidence() {
        let dir = temp_dir("alternatives");
        let trial = dir.join("trial");
        write_events(
            &trial,
            &serde_json::json!({ "answer": "the bug is the missing pointer" }),
        );
        let task = task_with(Some(deterministic_evaluator(vec![
            serde_json::json!({"id": "cause", "type": "containsAny", "values": ["null check", "missing pointer"]}),
        ])));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "pass");
        assert!(
            grading.checks[0]["detail"]
                .as_str()
                .unwrap()
                .contains("missing pointer")
        );
    }

    #[test]
    fn artifact_checks_read_declared_artifact_files_only() {
        let dir = temp_dir("artifacts");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({ "answer": "patch attached" }));
        let mut task = task_with(Some(deterministic_evaluator(vec![
            serde_json::json!({"id": "patch-present", "scope": "artifact", "artifact": "fix.patch", "type": "containsAny", "values": ["parse_header"]}),
        ])));
        task.expected_artifacts = vec!["fix.patch".into()];

        // No artifact yet: the check fails (the agent did not produce it).
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert!(
            grading.checks[0]["detail"]
                .as_str()
                .unwrap()
                .contains("not produced")
        );

        // Artifact produced: the same check passes.
        std::fs::create_dir_all(trial.join("adapter")).unwrap();
        std::fs::write(
            trial.join("adapter").join("fix.patch"),
            "diff --git a/x b/x\nparse_header\n",
        )
        .unwrap();
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "pass");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn censored_and_undeclared_trials_are_not_graded() {
        let dir = temp_dir("censored");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({ "answer": "partial" }));
        let task = task_with(Some(deterministic_evaluator(vec![
            serde_json::json!({"id": "cause", "type": "containsAny", "values": ["x"]}),
        ])));

        // A timed-out trial is censored, never graded as a wrong answer.
        let grading = grade_and_write(
            &task,
            &trial,
            "t1",
            "cond",
            0,
            "error",
            Some("wall_clock_exceeded"),
        )
        .unwrap();
        assert_eq!(grading.status, "not_graded");
        assert!(grading.reason.unwrap().contains("censored"));

        // A task without an evaluator is never graded.
        let task = task_with(None);
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "not_graded");
        assert_eq!(grading.method, "none");

        // Declared review methods wait for a manual grading record.
        let human = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "method": "humanRubric",
                "rubric": "name the failing function"
            }))
            .unwrap(),
        ));
        let grading = grade_and_write(&human, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "not_graded");
        assert_eq!(grading.method, "humanRubric");

        let judged = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "method": "modelJudge"
            }))
            .unwrap(),
        ));
        let grading = grade_and_write(&judged, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "not_graded");
        assert!(grading.reason.unwrap().contains("independent"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_manual_grading_is_kept_and_invalid_records_fail() {
        let dir = temp_dir("manual");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({ "answer": "x" }));
        let task = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "method": "humanRubric",
                "rubric": "name the failing function"
            }))
            .unwrap(),
        ));

        // No manual record yet: pending.
        grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();

        // Operator writes a valid manual result: it is honored and kept.
        std::fs::write(
            trial.join("grading.json"),
            r#"{"schemaVersion":1,"status":"pass","method":"humanRubric","reason":"names parse_header; blinded review by op-x"}"#,
        )
        .unwrap();
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "pass");
        assert_eq!(grading.method, "humanRubric");
        let stored = load(&trial).unwrap();
        assert!(stored["reason"].as_str().unwrap().contains("blinded"));

        // A record whose method does not match the declared evaluator is invalid.
        std::fs::write(
            trial.join("grading.json"),
            r#"{"schemaVersion":1,"status":"fail","method":"deterministic","reason":"nope"}"#,
        )
        .unwrap();
        let err = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap_err();
        assert!(err.contains("not a valid manual grading record"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_answer_or_event_log_is_not_silently_graded() {
        let dir = temp_dir("evidence");
        let trial = dir.join("trial");
        let task = task_with(Some(deterministic_evaluator(vec![
            serde_json::json!({"id": "cause", "type": "containsAny", "values": ["x"]}),
        ])));

        // Completed trial but the adapter never recorded the answer field.
        write_events(&trial, &serde_json::json!({ "other": "data" }));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert!(
            grading.checks[0]["detail"]
                .as_str()
                .unwrap()
                .contains("no final answer")
        );

        // No event log at all: the evaluator engine failed, not the agent.
        let trial = dir.join("trial-broken");
        std::fs::create_dir_all(&trial).unwrap();
        let err = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap_err();
        assert!(err.contains("event log unreadable"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn untrusted_answer_text_is_data_only() {
        let dir = temp_dir("untrusted");
        let trial = dir.join("trial");
        let marker = dir.join("pwned.txt");
        // The answer contains shell-shaped text: it must be matched as
        // data, never executed.
        write_events(
            &trial,
            &serde_json::json!({ "answer": "echo pwned > pwned.txt" }),
        );
        let task = task_with(Some(deterministic_evaluator(vec![
            serde_json::json!({"id": "no-shell", "type": "notContains", "values": ["echo pwned"]}),
        ])));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert!(!marker.exists(), "answer text must never be executed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
