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

use std::io::Read;
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

/// The state of the stored grading record. Only operator-written files can
/// be unparseable: engine records are JSON the engine itself wrote.
enum ExistingRecord {
    Absent,
    Record(Value),
    Unparseable { parse_error: String },
}

/// Reads the stored grading record, distinguishing an absent file from a
/// present-but-unparseable one (an operator file the engine must not
/// clobber silently).
fn read_existing_record(trial_dir: &Path) -> ExistingRecord {
    let path = trial_dir.join("grading.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return ExistingRecord::Absent;
    };
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => ExistingRecord::Record(value),
        Err(err) => ExistingRecord::Unparseable {
            parse_error: err.to_string(),
        },
    }
}

/// Reads the stored grading record when present and parseable.
pub fn load(trial_dir: &Path) -> Option<Value> {
    match read_existing_record(trial_dir) {
        ExistingRecord::Record(value) => Some(value),
        ExistingRecord::Absent | ExistingRecord::Unparseable { .. } => None,
    }
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

/// Preserves an invalid/unparseable operator `grading.json` at
/// `grading.json.rejected` (original bytes, never overwritten in place) and
/// writes the `evaluator_error` record in its place. The returned grading is
/// the recorded error itself, so callers do not write over it again.
fn record_rejected_manual_record(
    trial_dir: &Path,
    trial_id: &str,
    task_id: &str,
    condition_id: &str,
    repetition: u32,
    method: &str,
    reason: &str,
) -> Result<Grading, String> {
    let grading_path = trial_dir.join("grading.json");
    let rejected_path = trial_dir.join("grading.json.rejected");
    if rejected_path.exists() {
        std::fs::remove_file(&rejected_path)
            .map_err(|err| format!("could not replace existing grading.json.rejected: {err}"))?;
    }
    std::fs::rename(&grading_path, &rejected_path)
        .map_err(|err| format!("could not preserve the operator's grading.json: {err}"))?;
    let record =
        evaluator_error_record(trial_id, task_id, condition_id, repetition, method, reason);
    artifacts::write_json(&grading_path, &record).map_err(|err| err.to_string())?;
    Ok(Grading {
        status: "evaluator_error".into(),
        method: method.into(),
        reason: Some(reason.to_string()),
        checks: Vec::new(),
        answer_present: false,
    })
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
/// valid and kept verbatim (never rewritten as engine records). An invalid
/// or unparseable operator record is preserved at `grading.json.rejected`
/// and an `evaluator_error` is recorded in its place (returned as
/// `Ok`); a recorded `evaluator_error` is re-emitted verbatim. Everything
/// else is (re)computed and written as an engine record. A `Result::Err`
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
                    match read_existing_record(trial_dir) {
                        // Operator-written record (no engine provenance marker).
                        ExistingRecord::Record(raw)
                            if raw.get("source").and_then(Value::as_str) != Some("engine") =>
                        {
                            if manual_grading_is_valid(&raw, method) {
                                // Kept verbatim: an engine rewrite would
                                // relabel the operator's file as engine-written
                                // and the next rebuild would drop it.
                                return Ok(Grading {
                                    status: raw["status"].as_str().unwrap_or("not_graded").into(),
                                    method: method.into(),
                                    reason: raw["reason"].as_str().map(str::to_string),
                                    checks: Vec::new(),
                                    answer_present: false,
                                });
                            }
                            // An invalid operator record: preserve the
                            // operator's bytes at grading.json.rejected and
                            // record the evaluator_error in grading.json
                            // instead of overwriting the operator's file.
                            let reason = format!(
                                "existing manual grading record is invalid (expected method \
                                 '{method}', status pass/fail and a non-empty reason); the \
                                 operator's record is preserved at grading.json.rejected"
                            );
                            return record_rejected_manual_record(
                                trial_dir,
                                trial_id,
                                task.id.as_str(),
                                condition_id,
                                repetition,
                                method,
                                &reason,
                            );
                        }
                        // An operator file that is not parseable JSON: same
                        // treatment - the operator's bytes are preserved
                        // verbatim and the evaluator_error is recorded in
                        // their place.
                        ExistingRecord::Unparseable { parse_error, .. } => {
                            let reason = format!(
                                "existing grading.json is not parseable JSON ({parse_error}); the \
                                 operator's record is preserved at grading.json.rejected"
                            );
                            record_rejected_manual_record(
                                trial_dir,
                                trial_id,
                                task.id.as_str(),
                                condition_id,
                                repetition,
                                method,
                                &reason,
                            )?
                        }
                        // A recorded evaluator error is sticky: re-emit the
                        // stored reason verbatim (no rewrite, no re-wrapping,
                        // so repeated rebuilds are byte-stable). A valid
                        // manual record replaces it.
                        ExistingRecord::Record(raw)
                            if raw.get("status").and_then(Value::as_str)
                                == Some("evaluator_error")
                                && raw.get("method").and_then(Value::as_str) == Some(method) =>
                        {
                            return Ok(Grading {
                                status: "evaluator_error".into(),
                                method: method.into(),
                                reason: raw
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                checks: Vec::new(),
                                answer_present: false,
                            });
                        }
                        // Engine-written pending/censored state, or no record
                        // yet: recompute.
                        _ => match method {
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

/// Upper bound for a declared artifact file read by deterministic checks.
const MAX_ARTIFACT_BYTES: u64 = 1_048_576;
/// Upper bound for the adapter event log read for grading.
const MAX_EVENT_LOG_BYTES: u64 = 8_388_608;

/// Reads an agent-writable artifact file with untrusted-content guards: the
/// path must be a regular file (symlinks, FIFOs and devices are treated as
/// absent, so the grader can neither hang nor read outside the adapter
/// directory) and the read is capped. Any guard failure means the artifact
/// was "not produced".
fn read_guarded_artifact(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    std::io::Read::take(file, MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_ARTIFACT_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Reads the final event of the adapter event log and returns the declared
/// answer field when it is a string.
fn extract_final_answer(adapter_dir: &Path, answer_field: &str) -> Result<Option<String>, String> {
    let events_path = adapter_dir.join("events.ndjson");
    let raw = match std::fs::symlink_metadata(&events_path) {
        Ok(meta) if meta.file_type().is_file() => {
            let file = std::fs::File::open(&events_path)
                .map_err(|err| format!("adapter event log unreadable: {err}"))?;
            let mut buf = Vec::new();
            std::io::Read::take(file, MAX_EVENT_LOG_BYTES + 1)
                .read_to_end(&mut buf)
                .map_err(|err| format!("adapter event log unreadable: {err}"))?;
            if buf.len() as u64 > MAX_EVENT_LOG_BYTES {
                return Err(format!(
                    "adapter event log unreadable: exceeds the {MAX_EVENT_LOG_BYTES}-byte grading limit"
                ));
            }
            String::from_utf8_lossy(&buf).into_owned()
        }
        Ok(_) => {
            return Err(
                "adapter event log unreadable: not a regular file (symlinks and devices are refused)"
                    .to_string(),
            );
        }
        Err(err) => return Err(format!("adapter event log unreadable: {err}")),
    };
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
                read_guarded_artifact(&adapter_dir.join(name))
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
            "notContains" => haystack.as_deref().is_some_and(|text| {
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
            "notContains" => match haystack.as_deref() {
                None => evidence_detail(&check.scope, &check.artifact),
                Some(text) => {
                    let found: Vec<String> = check
                        .values
                        .iter()
                        .filter(|value| text.contains(value.as_str()))
                        .cloned()
                        .collect();
                    format!("found forbidden: {}", found.join(", "))
                }
            },
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

        // A record whose method does not match the declared evaluator is
        // invalid: the operator's bytes are preserved at
        // grading.json.rejected and an evaluator_error is recorded in their
        // place (never clobbered in place).
        std::fs::write(
            trial.join("grading.json"),
            r#"{"schemaVersion":1,"status":"fail","method":"deterministic","reason":"nope"}"#,
        )
        .unwrap();
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "evaluator_error");
        assert!(grading.reason.unwrap().contains("invalid"));
        let stored = load(&trial).unwrap();
        assert_eq!(stored["status"], "evaluator_error");
        assert_eq!(stored["source"], "engine");
        assert_eq!(
            std::fs::read_to_string(trial.join("grading.json.rejected")).unwrap(),
            r#"{"schemaVersion":1,"status":"fail","method":"deterministic","reason":"nope"}"#
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kept_manual_record_survives_repeat_grading_passes() {
        let dir = temp_dir("manual-repeat");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({ "answer": "x" }));
        let task = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "method": "humanRubric",
                "rubric": "name the failing function"
            }))
            .unwrap(),
        ));

        // First pass: pending, engine record written.
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "not_graded");

        // Operator writes a valid manual record.
        std::fs::write(
            trial.join("grading.json"),
            r#"{"schemaVersion":1,"status":"pass","method":"humanRubric","reason":"names parse_header; blinded review by op-x"}"#,
        )
        .unwrap();

        // Consecutive grading passes (e.g. two `bench report` rebuilds) must
        // keep the operator's record verbatim: still a pass, still
        // operator-owned (no engine relabel), same reason.
        for pass in 0..2 {
            let grading =
                grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
            assert_eq!(grading.status, "pass", "pass {pass} must stay a pass");
            assert_eq!(grading.method, "humanRubric");
            let stored = load(&trial).unwrap();
            assert_eq!(stored["status"], "pass", "pass {pass} stored status");
            assert_eq!(
                stored.get("source"),
                None,
                "pass {pass}: kept manual record must not be relabeled as engine-written"
            );
            assert!(
                stored["reason"].as_str().unwrap().contains("blinded"),
                "pass {pass}: operator reason must survive"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorded_evaluator_error_stays_stable_across_rebuilds() {
        let dir = temp_dir("sticky-error");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({ "answer": "x" }));
        let task = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "method": "humanRubric",
                "rubric": "name the failing function"
            }))
            .unwrap(),
        ));

        // Operator writes an invalid record; the first pass preserves it at
        // grading.json.rejected and records an evaluator_error in its place.
        std::fs::write(
            trial.join("grading.json"),
            r#"{"schemaVersion":1,"status":"fail","method":"deterministic","reason":"nope"}"#,
        )
        .unwrap();
        let first = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(first.status, "evaluator_error");

        // Subsequent passes must re-emit the recorded evaluator error
        // verbatim: no re-wrapping, and the stored record stays byte-stable
        // across passes, so repeated `bench report` rebuilds never nest the
        // reason or repeat the operator's text.
        let stored_after_first = std::fs::read_to_string(trial.join("grading.json")).unwrap();
        for pass in 1..3 {
            let grading =
                grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
            assert_eq!(
                grading.status, "evaluator_error",
                "pass {pass} must stay an evaluator_error"
            );
            assert_eq!(
                grading.reason, first.reason,
                "pass {pass}: reason must be re-emitted verbatim"
            );
            assert_eq!(
                std::fs::read_to_string(trial.join("grading.json")).unwrap(),
                stored_after_first,
                "pass {pass}: stored record must be byte-stable"
            );
        }
        // And a corrected manual record replaces it.
        std::fs::write(
            trial.join("grading.json"),
            r#"{"schemaVersion":1,"status":"fail","method":"humanRubric","reason":"missed the failing function"}"#,
        )
        .unwrap();
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert_eq!(grading.method, "humanRubric");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unparseable_manual_record_is_evaluator_error_and_not_clobbered() {
        let dir = temp_dir("unparseable");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({ "answer": "x" }));
        let task = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "method": "humanRubric",
                "rubric": "name the failing function"
            }))
            .unwrap(),
        ));

        // Operator writes a record with a trailing comma: present but not
        // parseable JSON.
        let broken = r#"{"schemaVersion":1,"status":"pass","method":"humanRubric","reason":"trailing comma","#;
        std::fs::write(trial.join("grading.json"), broken).unwrap();

        // The engine must not clobber the operator's bytes in place: they
        // are preserved at grading.json.rejected and an evaluator_error is
        // recorded in their place.
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "evaluator_error");
        assert!(grading.reason.unwrap().contains("not parseable JSON"));
        let stored = load(&trial).unwrap();
        assert_eq!(stored["status"], "evaluator_error");
        assert_eq!(stored["source"], "engine");
        assert_eq!(
            std::fs::read_to_string(trial.join("grading.json.rejected")).unwrap(),
            broken
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_answer_fails_every_check_type_including_not_contains() {
        // A trial that produced no answer cannot pass a notContains-only
        // task: an absent haystack fails every check type, so the trial is
        // graded fail (never "vacuously clean").
        let dir = temp_dir("no-answer-notcontains");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({}));
        let task = task_with(Some(
            serde_json::from_value(serde_json::json!({
                "checks": [{
                    "id": "no-forbidden-claim",
                    "type": "notContains",
                    "values": ["definitely present"]
                }]
            }))
            .unwrap(),
        ));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert!(!grading.answer_present);
        assert_eq!(grading.checks[0]["result"], "fail");
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

    #[test]
    fn oversized_artifact_is_treated_as_absent() {
        // A declared artifact beyond the read cap is treated as "not
        // produced" rather than read (bounded grader work).
        let dir = temp_dir("oversized-artifact");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({}));
        let adapter = trial.join("adapter");
        std::fs::create_dir_all(&adapter).unwrap();
        std::fs::write(
            adapter.join("out.txt"),
            vec![b'x'; MAX_ARTIFACT_BYTES as usize + 1],
        )
        .unwrap();
        let task = task_with(Some(deterministic_evaluator(vec![serde_json::json!({
            "id": "has-evidence",
            "type": "containsAny",
            "scope": "artifact",
            "artifact": "out.txt",
            "values": ["x"]
        })])));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert_eq!(grading.checks[0]["result"], "fail");
        assert!(
            grading.checks[0]["detail"]
                .as_str()
                .unwrap()
                .contains("was not produced"),
            "oversized artifact must be treated as absent: {:?}",
            grading.checks[0]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_artifact_is_treated_as_absent() {
        // The agent can create symlinks in adapter/: following one would let
        // it forge check results or probe files outside the trial. A
        // symlinked declared artifact is treated as "not produced".
        let dir = temp_dir("symlink-artifact");
        let trial = dir.join("trial");
        write_events(&trial, &serde_json::json!({}));
        let adapter = trial.join("adapter");
        std::fs::create_dir_all(&adapter).unwrap();
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "declared-evidence-token").unwrap();
        std::os::unix::fs::symlink(&outside, adapter.join("out.txt")).unwrap();
        let task = task_with(Some(deterministic_evaluator(vec![serde_json::json!({
            "id": "has-evidence",
            "type": "containsAny",
            "scope": "artifact",
            "artifact": "out.txt",
            "values": ["declared-evidence-token"]
        })])));
        let grading = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap();
        assert_eq!(grading.status, "fail");
        assert_eq!(grading.checks[0]["result"], "fail");
        assert!(
            grading.checks[0]["detail"]
                .as_str()
                .unwrap()
                .contains("was not produced"),
            "symlink must not be followed: {:?}",
            grading.checks[0]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_event_log_is_refused() {
        // A symlinked events.ndjson must not be read: it could point at any
        // readable file, forging the "final" answer.
        let dir = temp_dir("symlink-events");
        let trial = dir.join("trial");
        let adapter = trial.join("adapter");
        std::fs::create_dir_all(&adapter).unwrap();
        let outside = dir.join("events.txt");
        std::fs::write(
            &outside,
            "{\"v\":1,\"t\":\"final\",\"ts\":1,\"status\":\"completed\",\"data\":{\"answer\":\"forged\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, adapter.join("events.ndjson")).unwrap();
        let task = task_with(Some(deterministic_evaluator(vec![serde_json::json!({
            "id": "answer",
            "type": "containsAll",
            "values": ["forged"]
        })])));
        let err = grade_and_write(&task, &trial, "t1", "cond", 0, "completed", None).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
