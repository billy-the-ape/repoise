//! Per-trial execution.
//!
//! The coordinator never implements the agent loop. It prepares the trial
//! workspace (worktree, guidance, index — all setup time, outside the timed
//! agent interval), builds the versioned AP-03 adapter request, and
//! delegates the agent process to the protocol host through the checked-in
//! Node bridge: protocol enforcement, usage accounting and child-tree
//! cleanup have one source of truth (the host). The coordinator adds
//! external monotonic timing, cancellation, an outer watchdog and the
//! per-trial record.
//!
//! Trial statuses: host statuses (`completed`, `cancelled`, `agent_error`,
//! `adapter_error`, `budget_stop`, `timeout`, `protocol_error`) pass through
//! unchanged; the coordinator adds `infra_error` (its own setup/spawn
//! failures, including a setup wall-clock timeout — reason `setup_timeout`
//! — or an interrupt landing during setup — reason `setup_cancelled`) and
//! the scheduler adds `not_started` (budget cap, interruption, drift).
//! Setup work is never part of the timed agent interval. Every attempt is
//! recorded; nothing is silently retried.

use std::fs;
use std::io;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::error::Error;

use super::artifacts::{self, Layout};
use super::plan::Trial;
use super::preflight::Preflight;
use super::suite::{Condition, Suite, Task};
use super::workspace::Workspace;

/// Grace between a cooperative cancel and the forced tree kill.
const CANCEL_GRACE: Duration = Duration::from_secs(15);
/// Outer watchdog margin over the host's own wall-clock enforcement.
const OUTER_WATCHDOG_MARGIN: Duration = Duration::from_secs(120);
/// How often the coordinator polls the bridge for cancellation/status.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// File name of a trial's MCP server audit sink (under the trial
/// directory, outside the agent-visible trial workspace).
pub const AUDIT_SINK_FILENAME: &str = "mcp-audit.ndjson";
/// Bound on how much of one trial's audit sink the coordinator reads
/// (per-record bytes are already bounded at the server).
const MAX_AUDIT_READ_BYTES: u64 = 16 * 1024 * 1024;
/// The `schemaVersion` value of summarized audit documents and mode records.
const AUDIT_SCHEMA_VERSION: u64 = 1;
/// The `source` label of summarized audit documents and mode records.
const AUDIT_SOURCE: &str = "server-side observation";

/// Bound on how much of one setup child's stdout/stderr the coordinator
/// captures for diagnostics: the readers keep draining (so a full pipe can
/// never deadlock the child) but store at most this many bytes.
const MAX_SETUP_PIPE_BYTES: usize = 64 * 1024;
/// Bound on how much captured setup diagnostic survives into a recorded
/// trial error (the tail is kept and marked truncated).
const MAX_SETUP_ERROR_BYTES: usize = 8 * 1024;

/// Process-group separation for one owned setup child.
///
/// The workspace denies `unsafe_code`; this is the single documented FFI
/// boundary that locally allows it, isolated to this module. The setup
/// child makes itself the leader of a new process group between `fork` and
/// `exec`, so every descendant inherits the group and the coordinator's
/// bounded kill (a group signal on Unix) reaches the whole setup tree — a
/// descendant of the index child must never outlive the experiment's
/// cancellation. On Windows the equivalent comes from `taskkill /T`.
#[cfg(unix)]
mod setup_process_group {
    #![allow(unsafe_code)]

    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    use libc::setpgid;

    /// Spawns `command` with the child leading its own process group.
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Child> {
        // Safety: `pre_exec` hooks the forked child before `exec`; the hook
        // only calls `setpgid(0, 0)` (acting on the calling process itself)
        // and never touches Rust runtime state.
        unsafe {
            command.pre_exec(|| {
                // Runs only in the freshly forked child before `exec`, after
                // which its address space is replaced; `setpgid(0, 0)` acts
                // on the calling process itself.
                if setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            })
        };
        command.spawn()
    }
}

/// Spawns one owned setup child so its whole process tree can be
/// terminated (Unix: own process group; Windows: tree kill).
#[cfg(unix)]
fn spawn_setup_child(command: &mut Command) -> io::Result<Child> {
    setup_process_group::spawn(command)
}

#[cfg(not(unix))]
fn spawn_setup_child(command: &mut Command) -> io::Result<Child> {
    command.spawn()
}

/// Terminates one setup child and its descendant processes (best effort,
/// mirroring the host bridge kill path): on Unix the child leads its own
/// process group, so the group signal takes the whole tree; on Windows
/// `taskkill /T` walks the tree. Signals only; the caller reaps the direct
/// child.
fn kill_setup_tree(pid: u32) {
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    } else {
        // Direct kill first, then the group: after the leader is gone the
        // group signal still reaches any surviving descendants.
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .output();
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .output();
    }
}

/// Why a bounded setup child did not complete successfully.
#[derive(Debug)]
enum SetupFailureKind {
    /// The setup wall-clock bound fired before the child completed.
    Timeout,
    /// The experiment interrupt landed before or during setup.
    Interrupted,
    /// The child could not be spawned / waited on (coordinator-side I/O).
    Coordinator(String),
}

/// A failed setup wait, with the bounded captured diagnostic (raw;
/// sanitized and capped where it is recorded).
#[derive(Debug)]
struct SetupFailure {
    kind: SetupFailureKind,
    captured: String,
}

/// Drains one setup pipe until EOF: reads are capped at the store size (the
/// rest is discarded), so a chatty child can neither deadlock a full pipe
/// nor grow memory without bound.
fn drain_setup_pipe<T: Read>(pipe: T, buffer: &Mutex<Vec<u8>>, cap: usize) {
    let mut reader = BufReader::new(pipe);
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let mut guard = buffer.lock().unwrap();
                let room = cap.saturating_sub(guard.len());
                if room > 0 {
                    guard.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            Err(_) => break,
        }
    }
}

/// Runs one owned setup child under a hard wall-clock bound and the
/// experiment interrupt. Both pipes are drained in bounded reader threads;
/// when the bound fires or the interrupt lands, the child's whole process
/// tree is terminated and reaped and the captured (bounded) output comes
/// back with the failure kind. A pre-set interrupt never spawns the child.
fn run_bounded_setup_command(
    mut command: Command,
    bound: Duration,
    interrupted: &AtomicBool,
) -> Result<Output, SetupFailure> {
    if interrupted.load(Ordering::SeqCst) {
        return Err(SetupFailure {
            kind: SetupFailureKind::Interrupted,
            captured: String::new(),
        });
    }
    let deadline = Instant::now() + bound;
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_setup_child(child).map_err(|err| SetupFailure {
        kind: SetupFailureKind::Coordinator(format!("spawn: {err}")),
        captured: String::new(),
    })?;
    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    let stdout_buf_reader = Arc::clone(&stdout_buf);
    let stderr_buf_reader = Arc::clone(&stderr_buf);
    let stdout_reader = std::thread::spawn(move || {
        drain_setup_pipe(stdout_pipe, &stdout_buf_reader, MAX_SETUP_PIPE_BYTES);
    });
    let stderr_reader = std::thread::spawn(move || {
        drain_setup_pipe(stderr_pipe, &stderr_buf_reader, MAX_SETUP_PIPE_BYTES);
    });

    let mut failure: Option<SetupFailureKind> = None;
    let mut finished: Option<std::process::ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                finished = Some(status);
                break;
            }
            Ok(None) => {
                if interrupted.load(Ordering::SeqCst) {
                    failure = Some(SetupFailureKind::Interrupted);
                    break;
                }
                if Instant::now() >= deadline {
                    failure = Some(SetupFailureKind::Timeout);
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                kill_setup_tree(child.id());
                let _ = child.wait();
                return Err(SetupFailure {
                    kind: SetupFailureKind::Coordinator(format!("wait: {err}")),
                    captured: String::new(),
                });
            }
        }
    }
    // Terminated on failure; on completion this is the no-op sweep of any
    // lingering descendants (a completed setup must not leave children
    // holding the pipes).
    kill_setup_tree(child.id());
    let status = match finished {
        Some(status) => status,
        None => child.wait().map_err(|err| SetupFailure {
            kind: SetupFailureKind::Coordinator(format!("reap: {err}")),
            captured: String::new(),
        })?,
    };
    // The tree is gone, so both pipes are closed and the readers are done.
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    let stdout = String::from_utf8_lossy(&stdout_buf.lock().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_buf.lock().unwrap()).into_owned();
    match failure {
        Some(kind) => Err(SetupFailure {
            kind,
            captured: format!("{stdout}{stderr}"),
        }),
        None => Ok(Output {
            status,
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
        }),
    }
}
/// Everything the runner needs, shared by every trial of one experiment.
pub struct RunContext {
    pub self_exe: PathBuf,
    pub layout: Layout,
    pub preflight: Preflight,
    pub suite: Suite,
    pub keep_workspaces: bool,
    /// Resolved adapter environment (may include credentials — process
    /// environment only, never serialized into an artifact).
    pub adapter_env: Vec<(String, String)>,
    pub json: bool,
}

/// One recorded trial; `status` is the coordinator-level status.
pub struct TrialOutcome {
    pub status: String,
    pub reason: Option<String>,
    pub summary: String,
}

/// Setup state of a trial that failed before the agent produced anything.
struct InfraSetup {
    workspace: Option<Workspace>,
    setup: Value,
    guidance: Value,
}

/// Timings captured while a trial is being set up.
struct TrialTimings {
    queue_ms: u128,
    setup_started: Instant,
    started: Instant,
}

impl RunContext {
    /// Runs one trial end to end: workspace setup, adapter run, record,
    /// cleanup. Setup/infrastructure failures are recorded as
    /// `infra_error` trials (the run continues); only unwritable records are
    /// fatal.
    pub fn run_trial(
        &self,
        trial: &Trial,
        queue_ms: u128,
        interrupted: &AtomicBool,
    ) -> Result<TrialOutcome, Error> {
        let started = Instant::now();
        let condition = self
            .suite
            .condition(&trial.condition_id)
            .expect("validated condition id");
        let task = self.suite.task(&trial.task_id).expect("validated task id");
        let trial_dir = self.layout.trial_dir(&trial.trial_id);
        let total =
            self.suite.tasks.len() * self.suite.conditions.len() * self.suite.repeat as usize;

        // --- Setup (outside the timed agent interval) ----------------------
        let setup_started = Instant::now();
        let workspace_path = self.layout.workspace_path(&trial.trial_id);
        let workspace = match Workspace::create(
            &self.preflight.root,
            &workspace_path,
            &self.preflight.revision_sha,
            self.preflight.patch_file.as_deref(),
        ) {
            Ok(workspace) => workspace,
            Err(err) => {
                return self.record_infra(
                    trial,
                    TrialTimings {
                        queue_ms,
                        setup_started,
                        started,
                    },
                    InfraSetup {
                        workspace: None,
                        setup: Value::Null,
                        guidance: Value::Null,
                    },
                    "worktree_create_failed",
                    &err.to_string(),
                );
            }
        };
        let worktree_ms = setup_started.elapsed().as_millis();

        let guidance = match condition.retrieval_mode.as_str() {
            "baseline" => match baseline_guidance(&workspace.path) {
                Ok(value) => value,
                Err(err) => {
                    return self.record_infra(
                        trial,
                        TrialTimings {
                            queue_ms,
                            setup_started,
                            started,
                        },
                        InfraSetup {
                            workspace: Some(workspace),
                            setup: Value::Null,
                            guidance: Value::Null,
                        },
                        "baseline_guidance_conflict",
                        &err.to_string(),
                    );
                }
            },
            _ => repoise_guidance(&workspace.path),
        };

        let mut setup = serde_json::Map::new();
        setup.insert("worktreeMs".into(), Value::from(worktree_ms as u64));
        setup.insert("indexMs".into(), Value::Null);
        setup.insert("indexGeneration".into(), Value::Null);
        setup.insert("indexManifestHash".into(), Value::Null);
        setup.insert("vectors".into(), Value::Null);
        if condition.retrieval_mode == "repoise" {
            // The trial index is coordinator-owned setup work: bounded by
            // the per-trial setup wall clock (separate from the agent wall
            // clock) and cancellable through the experiment interrupt.
            let setup_limit_ms = self
                .suite
                .budgets
                .setup_wall_clock_ms
                .unwrap_or(super::suite::DEFAULT_SETUP_WALL_CLOCK_MS);
            match build_trial_index(self, &workspace.path, setup_limit_ms, interrupted) {
                Ok(index) => {
                    setup.insert("indexMs".into(), Value::from(index.elapsed_ms as u64));
                    setup.insert("indexGeneration".into(), Value::from(index.generation_id));
                    setup.insert(
                        "indexManifestHash".into(),
                        Value::from(index.manifest_hash.clone()),
                    );
                    setup.insert("vectors".into(), Value::from(index.vectors_total));
                    // A hybrid/vectors-only condition must actually have a
                    // fully published vector set: `repoise index` exits 0 even
                    // when embeddings fail or stay pending, and the pinned
                    // MCP server degrades to lexical for a vectorless index.
                    // Fail setup instead of counting a lexical run as hybrid.
                    if matches!(
                        condition.search_mode.as_deref(),
                        Some("hybrid") | Some("vectors-only")
                    ) && !index_viable_for_vectors(&index)
                    {
                        return self.record_infra(
                            trial,
                            TrialTimings {
                                queue_ms,
                                setup_started,
                                started,
                            },
                            InfraSetup {
                                workspace: Some(workspace),
                                setup: Value::Object(setup),
                                guidance,
                            },
                            "index_missing_vectors",
                            &format!(
                                "condition '{}' requires published vectors, but the trial \
                                 index published {} vector(s) with {} pending and {} \
                                 failed; the pinned MCP server would silently degrade \
                                 to lexical",
                                condition.search_mode.as_deref().unwrap_or(""),
                                index.vectors_total,
                                index.vectors_pending,
                                index.vectors_failed
                            ),
                        );
                    }
                }
                Err(setup_error) => {
                    return self.record_infra(
                        trial,
                        TrialTimings {
                            queue_ms,
                            setup_started,
                            started,
                        },
                        InfraSetup {
                            workspace: Some(workspace),
                            setup: Value::Object(setup),
                            guidance,
                        },
                        setup_error.reason,
                        &setup_error.detail,
                    );
                }
            }
        }
        let setup_ms = setup_started.elapsed().as_millis();

        // --- The timed agent interval ---------------------------------------
        let request = match build_request(self, trial, condition, task, &workspace) {
            Ok(value) => value,
            Err(err) => {
                return self.record_infra(
                    trial,
                    TrialTimings {
                        queue_ms,
                        setup_started,
                        started,
                    },
                    InfraSetup {
                        workspace: Some(workspace),
                        setup: Value::Object(setup),
                        guidance,
                    },
                    "request_build_failed",
                    &err.to_string(),
                );
            }
        };
        let redacted = redact_request(&request);
        if let Err(err) = artifacts::write_json(&trial_dir.join("request.json"), &redacted) {
            return self.record_infra(
                trial,
                TrialTimings {
                    queue_ms,
                    setup_started,
                    started,
                },
                InfraSetup {
                    workspace: Some(workspace),
                    setup: Value::Object(setup),
                    guidance,
                },
                "request_write_failed",
                &err.to_string(),
            );
        }
        let request_hash = crate::hash::sha256_hex(
            serde_json::to_vec(&redacted).map_err(|err| Error::Json(err.to_string()))?,
        );
        // Re-check the interrupt before starting agent work: an interrupt
        // that landed during setup must not start a new agent process.
        if interrupted.load(Ordering::SeqCst) {
            return self.record_infra(
                trial,
                TrialTimings {
                    queue_ms,
                    setup_started,
                    started,
                },
                InfraSetup {
                    workspace: Some(workspace),
                    setup: Value::Object(setup),
                    guidance,
                },
                "interrupted_before_agent",
                "the experiment was interrupted between setup and agent work",
            );
        }
        let agent_started = Instant::now();
        // A host failure (spawn, request write, wait) still records the trial
        // as infra_error and removes the workspace, so the run continues and
        // the report is always published.
        let host = match run_host(self, trial, &request, &trial_dir, interrupted) {
            Ok(host) => host,
            Err(err) => {
                return self.record_infra(
                    trial,
                    TrialTimings {
                        queue_ms,
                        setup_started,
                        started,
                    },
                    InfraSetup {
                        workspace: Some(workspace),
                        setup: Value::Object(setup),
                        guidance,
                    },
                    "host_run_failed",
                    &err.to_string(),
                );
            }
        };
        let agent_ms =
            host_elapsed_ms(&host.result).unwrap_or_else(|| agent_started.elapsed().as_millis());

        // --- Server-side MCP observation (opt-in audit sink) ------------------
        // For repoise conditions the coordinator designated a per-trial
        // audit sink in this trial's pinned server argv (see mcp_argv);
        // summarize whatever records the server actually wrote. A missing,
        // truncated or absent sink is recorded as such — never as zero tool
        // use or a pass (see summarize_mcp_audit).
        let server_audit = if condition.retrieval_mode == "repoise" {
            Some(summarize_mcp_audit(&trial_dir.join(AUDIT_SINK_FILENAME)))
        } else {
            None
        };

        // --- Record -----------------------------------------------------------
        let total_ms = started.elapsed().as_millis();
        let record = serde_json::json!({
            "schemaVersion": 1,
            "trialId": trial.trial_id,
            "taskId": trial.task_id,
            "conditionId": trial.condition_id,
            "repetition": trial.repetition,
            "orderedIndex": trial.ordered_index,
            "workspaceRoot": workspace.path.to_string_lossy().into_owned(),
            "snapshotRevision": format!("git:{}", self.preflight.revision_sha),
            "setup": Value::Object(setup),
            "guidance": guidance,
            "requestHash": request_hash,
            "hello": host.result.get("hello").cloned(),
            "adapter": host.result.get("adapter").cloned(),
            "harness": host.result.get("harness").cloned(),
            // The per-trial effective harness version (from the adapter's
            // `harness_version` note) and the suite's expected pin: together
            // they make drift across trials visible in every recorded trial,
            // so pooled statistics can never silently mix versions.
            "harnessPin": self.suite.profile.harness.revision.clone(),
            "harnessVersion": observed_harness_version(&host.result),
            "mode": mode_record(condition, &host.result, server_audit.as_ref()),
            "timings": {
                "queueMs": queue_ms,
                "setupMs": setup_ms,
                "agentMs": agent_ms,
                "totalMs": total_ms
            },
            "status": host.status,
            "reason": host.reason,
            "usage": host.result.get("usage").cloned(),
            // Server-side observation of this trial's Repoise MCP usage
            // (null for baseline conditions; see summarize_mcp_audit).
            "audit": server_audit,
            "adapterDurationMs": host.result.get("adapterDurationMs").cloned(),
            "warnings": host.result.get("warnings").cloned().unwrap_or(Value::Array(vec![])),
            "errors": host.result.get("errors").cloned().unwrap_or(Value::Array(vec![])),
            "artifacts": {
                "requestFile": "request.json",
                "eventsFile": "adapter/events.ndjson",
                "resultFile": "adapter/result.json",
                "stderrLog": "adapter/stderr.log",
                "hostLog": "host.log",
                "mcpAuditFile": AUDIT_SINK_FILENAME,
                "trialFile": "trial.json"
            },
            "completeness": if host.result.is_null() { "partial" } else { "complete" }
        });
        if let Err(err) = artifacts::write_json(&trial_dir.join("trial.json"), &record) {
            // setup/guidance were moved into the record above; the record is
            // only the final artifact missing here.
            return self.record_infra(
                trial,
                TrialTimings {
                    queue_ms,
                    setup_started,
                    started,
                },
                InfraSetup {
                    workspace: Some(workspace),
                    setup: Value::Null,
                    guidance: Value::Null,
                },
                "trial_record_write_failed",
                &err.to_string(),
            );
        }

        // --- Grading (AP-05): independent, data-only, after the agent interval ---
        let grading = match super::grading::grade_and_write(
            task,
            &trial_dir,
            &trial.trial_id,
            &trial.condition_id,
            trial.repetition,
            &host.status,
            host.reason.as_deref(),
        ) {
            Ok(grading) => grading,
            Err(err) => {
                let record = super::grading::evaluator_error_record(
                    &trial.trial_id,
                    &trial.task_id,
                    &trial.condition_id,
                    trial.repetition,
                    task.evaluator
                        .as_ref()
                        .map(|evaluator| evaluator.method.as_str())
                        .unwrap_or("none"),
                    &format!("evaluator failure: {err}"),
                );
                if let Err(write_err) =
                    artifacts::write_json(&trial_dir.join("grading.json"), &record)
                {
                    progress(
                        self.json,
                        &format!(
                            "note: could not write grading record for {}: {write_err}",
                            trial_dir.display()
                        ),
                    );
                }
                super::grading::Grading {
                    status: "evaluator_error".into(),
                    method: "none".into(),
                    reason: Some(err),
                    checks: Vec::new(),
                    answer_present: false,
                }
            }
        };

        // --- Cleanup ------------------------------------------------------------
        if !self.keep_workspaces
            && let Err(err) = workspace.remove()
        {
            progress(
                self.json,
                &format!(
                    "note: could not remove workspace {}: {err}",
                    workspace.path.display()
                ),
            );
        }
        Ok(TrialOutcome {
            status: host.status.clone(),
            reason: host.reason.clone(),
            summary: format!(
                "trial {}/{} {id} ({condition}): {status} {reason} (setup {setup_ms} ms, agent {agent_ms} ms, grading {grading})",
                trial.ordered_index + 1,
                total,
                id = trial.trial_id,
                condition = trial.condition_id,
                status = host.status,
                reason = host.reason.as_deref().unwrap_or(""),
                grading = grading.status
            ),
        })
    }

    /// Records one `infra_error` trial (and cleans its workspace) so the
    /// run can continue; the report still lists the failed attempt.
    fn record_infra(
        &self,
        trial: &Trial,
        timings: TrialTimings,
        state: InfraSetup,
        reason: &str,
        detail: &str,
    ) -> Result<TrialOutcome, Error> {
        let record = serde_json::json!({
            "schemaVersion": 1,
            "trialId": trial.trial_id,
            "taskId": trial.task_id,
            "conditionId": trial.condition_id,
            "repetition": trial.repetition,
            "orderedIndex": trial.ordered_index,
            "workspaceRoot": state
                .workspace
                .as_ref()
                .map(|workspace| workspace.path.to_string_lossy().into_owned()),
            "setup": state.setup,
            "guidance": state.guidance,
            "timings": {
                "queueMs": timings.queue_ms,
                "setupMs": timings.setup_started.elapsed().as_millis(),
                "agentMs": null_value(),
                "totalMs": timings.started.elapsed().as_millis()
            },
            "status": "infra_error",
            "reason": reason,
            "errors": [ detail ],
            "completeness": "partial"
        });
        let trial_dir = self.layout.trial_dir(&trial.trial_id);
        artifacts::write_json(&trial_dir.join("trial.json"), &record)?;
        // An infra_error trial was never a completed answer: its grading
        // record is censored (not graded as a wrong answer), so the report's
        // grading counts stay honest.
        let task = self.suite.task(&trial.task_id).expect("validated task id");
        if let Err(err) = super::grading::grade_and_write(
            task,
            &trial_dir,
            &trial.trial_id,
            &trial.condition_id,
            trial.repetition,
            "infra_error",
            Some(reason),
        ) {
            let record = super::grading::evaluator_error_record(
                &trial.trial_id,
                &trial.task_id,
                &trial.condition_id,
                trial.repetition,
                task.evaluator
                    .as_ref()
                    .map(|evaluator| evaluator.method.as_str())
                    .unwrap_or("none"),
                &format!("evaluator failure: {err}"),
            );
            artifacts::write_json(&trial_dir.join("grading.json"), &record)?;
        }
        if let Some(workspace) = state.workspace
            && !self.keep_workspaces
        {
            let _ = workspace.remove();
        }
        progress(
            self.json,
            &format!(
                "trial {}/{} {}: infra_error {reason}",
                trial.ordered_index + 1,
                self.suite.tasks.len() * self.suite.conditions.len() * self.suite.repeat as usize,
                trial.trial_id
            ),
        );
        Ok(TrialOutcome {
            status: "infra_error".into(),
            reason: Some(reason.to_string()),
            summary: format!("{reason}: {detail}"),
        })
    }
}

fn null_value() -> Value {
    Value::Null
}

/// Increments a u64 counter stored as a JSON value.
fn bump(map: &mut serde_json::Map<String, Value>, key: &str) {
    let count = map.get(key).and_then(Value::as_u64).unwrap_or(0);
    map.insert(key.to_string(), Value::from(count + 1));
}

/// Builds the exact argv for one trial's read-only MCP server. The shape
/// (`mcp [ROOT] --search-mode <MODE> [--committed] [--audit-file <PATH>]`)
/// is accepted by the real CLI parser; the CLI integration test starts a
/// server with this argv. `audit_file`, when present, designates this
/// trial's unique machine audit sink: server-side observation of which
/// tools the trial actually invoked and with what retrieval outcome
/// (never the query or result content).
pub fn mcp_argv(
    exe: &Path,
    root: &Path,
    search_mode: &str,
    committed: bool,
    audit_file: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![
        exe.to_string_lossy().into_owned(),
        "mcp".into(),
        root.to_string_lossy().into_owned(),
        "--search-mode".into(),
        search_mode.to_string(),
    ];
    if committed {
        argv.push("--committed".into());
    }
    if let Some(path) = audit_file {
        argv.push("--audit-file".into());
        argv.push(path.to_string_lossy().into_owned());
    }
    argv
}

/// Builds the AP-03 adapter request for one trial. The MCP server argv
/// pins the enforced retrieval mode (`--search-mode`) and, for committed
/// snapshots, the immutable snapshot mode; the server is bound to the exact
/// trial worktree root.
fn build_request(
    ctx: &RunContext,
    trial: &Trial,
    condition: &Condition,
    task: &Task,
    workspace: &Workspace,
) -> Result<Value, Error> {
    let (prompt, task_hash) = match &task.prompt {
        Some(prompt) => (prompt.clone(), crate::hash::sha256_hex(prompt.as_bytes())),
        None => {
            let contents = task.resolved_prompt()?;
            let task_hash = crate::hash::sha256_hex(contents.as_bytes());
            (contents, task_hash)
        }
    };

    let (retrieval_mode, mcp) = match condition.retrieval_mode.as_str() {
        "baseline" => ("baseline", Value::Null),
        _ => {
            let mode = condition
                .search_mode
                .as_deref()
                .expect("validated searchMode for repoise conditions");
            // The per-trial audit sink lives in the trial directory
            // (coordinator-owned artifacts), outside the agent-visible
            // trial workspace and the indexed corpus.
            let audit_file = ctx
                .layout
                .trial_dir(&trial.trial_id)
                .join(AUDIT_SINK_FILENAME);
            let argv = mcp_argv(
                &ctx.self_exe,
                &workspace.path,
                mode,
                ctx.preflight.snapshot_kind == "commit",
                Some(&audit_file),
            );
            (
                "repoise",
                serde_json::json!({
                    "serverName": "repoise",
                    "argv": argv,
                    "env": mcp_server_env(
                        &ctx.layout.cache_dir,
                        ctx.preflight.embedding_endpoint_env.as_deref(),
                        ctx.preflight.embedding_api_key_env.as_deref(),
                    )
                }),
            )
        }
    };

    let required_capabilities: Vec<String> = if condition.required_capabilities.is_empty() {
        if retrieval_mode == "repoise" {
            vec![
                "fresh_sessions".to_string(),
                "isolated_state".to_string(),
                "mcp_config".to_string(),
                "usage_semantics".to_string(),
                "cancellation".to_string(),
            ]
        } else {
            vec![
                "fresh_sessions".to_string(),
                "isolated_state".to_string(),
                "usage_semantics".to_string(),
                "cancellation".to_string(),
            ]
        }
    } else {
        condition.required_capabilities.clone()
    };

    let limits = &ctx.suite.limits;
    let profile = &ctx.suite.profile;
    Ok(serde_json::json!({
        "protocolVersion": 1,
        "identity": {
            "experimentId": ctx.suite.id,
            "trialId": trial.trial_id,
            "taskId": trial.task_id,
            "replication": trial.repetition
        },
        "workspace": {
            "root": workspace.path.to_string_lossy().into_owned(),
            "revision": format!("git:{}", ctx.preflight.revision_sha),
            "scope": ctx.suite.scope
        },
        "task": {
            "prompt": prompt,
            "promptFile": null_value(),
            "taskVersion": task.version,
            "taskHash": task_hash
        },
        "condition": {
            "retrievalMode": retrieval_mode,
            "requiredCapabilities": required_capabilities,
            "mcp": mcp,
            "guidanceOverlay": {
                "enabled": retrieval_mode == "repoise",
                "description": if retrieval_mode == "repoise" {
                    "managed Repoise retrieval guidance present in the trial workspace (when installed)"
                } else {
                    "managed Repoise guidance removed from the trial workspace (baseline)"
                }
            }
        },
        "profile": {
            "harness": profile.harness,
            "provider": profile.provider,
            "model": profile.model,
            "reasoning": profile.reasoning,
            "compaction": profile.compaction,
            "sampling": profile.sampling
        },
        "limits": {
            "wallClockMs": limits
                .wall_clock_ms
                .unwrap_or(super::suite::DEFAULT_TRIAL_WALL_CLOCK_MS),
            "maxIterations": limits.max_iterations,
            "maxToolCalls": limits.max_tool_calls,
            "tokenBudget": limits.token_budget,
            "costBudgetUsd": limits.cost_budget_usd,
            "maxEventBytes": limits.max_event_bytes,
            "maxEvents": limits.max_events
        },
        "outputs": {
            "artifactDir": ctx.layout.adapter_dir(&trial.trial_id).to_string_lossy().into_owned(),
            "eventsFile": ctx
                .layout
                .adapter_dir(&trial.trial_id)
                .join("events.ndjson")
                .to_string_lossy()
                .into_owned(),
            "resultFile": ctx
                .layout
                .adapter_dir(&trial.trial_id)
                .join("result.json")
                .to_string_lossy()
                .into_owned()
        }
    }))
}

/// Artifact-safe copy of the trial request: the live request carries the
/// resolved `condition.mcp.env` values (e.g. the embedding endpoint/API-key),
/// but the names-only artifact contract means `request.json` stores them as
/// a placeholder. The unredacted request goes only to the host bridge over
/// stdin, and the recorded `requestHash` hashes this redacted form so it
/// matches the artifact on disk.
fn redact_request(request: &Value) -> Value {
    let mut redacted = request.clone();
    if let Some(env) = redacted
        .get_mut("condition")
        .and_then(|condition| condition.get_mut("mcp"))
        .and_then(|mcp| mcp.get_mut("env"))
        .and_then(Value::as_object_mut)
    {
        for value in env.values_mut() {
            *value = Value::from("[REDACTED]");
        }
    }
    redacted
}

/// Trial MCP server environment: the trial cache directory plus the
/// embedding endpoint/API-key variables declared in the root config. The
/// MCP server resolves those from its own process env for hybrid search,
/// exactly like the coordinator's index child does; without them a hybrid
/// query would silently degrade to lexical. Unset variables are omitted:
/// lexical conditions need no embedding, and a hybrid trial whose index has
/// no published vectors already fails setup (vector gate).
fn mcp_server_env(
    cache_dir: &Path,
    embedding_endpoint_env: Option<&str>,
    embedding_api_key_env: Option<&str>,
) -> Value {
    mcp_server_env_with(
        cache_dir,
        embedding_endpoint_env,
        embedding_api_key_env,
        &|name| std::env::var(name).ok(),
    )
}

/// `mcp_server_env` with an injectable environment lookup (tests).
fn mcp_server_env_with(
    cache_dir: &Path,
    embedding_endpoint_env: Option<&str>,
    embedding_api_key_env: Option<&str>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Value {
    let mut env = serde_json::Map::new();
    env.insert(
        crate::CACHE_ENV_VAR.to_string(),
        Value::from(cache_dir.to_string_lossy().into_owned()),
    );
    for name in [embedding_endpoint_env, embedding_api_key_env]
        .into_iter()
        .flatten()
    {
        if let Some(value) = lookup(name) {
            env.insert(name.to_string(), Value::from(value));
        }
    }
    Value::Object(env)
}

/// Removes only the managed Repoise guidance from a baseline trial
/// workspace (the user's checkout is never touched). A conflict means the
/// managed guidance cannot be verified absent, so the baseline check fails.
fn baseline_guidance(workspace: &Path) -> Result<Value, Error> {
    match crate::overlay::uninstall(workspace, false) {
        Ok(outcome) if outcome.conflicts.is_empty() => Ok(serde_json::json!({
            "retrieval": "baseline",
            "overlayRemoved": outcome.manifest_removed,
            "removedFiles": outcome.removed_files,
            "removedBlocks": outcome.removed_blocks,
            "conflicts": outcome.conflicts
        })),
        Ok(outcome) => Err(Error::Init(format!(
            "baseline guidance verification failed: {} managed overlay conflict(s); the \
             baseline workspace still carries Repoise guidance",
            outcome.conflicts.len()
        ))),
        Err(err) if err.to_string().contains("no overlay manifest") => Ok(serde_json::json!({
            "retrieval": "baseline",
            "overlayRemoved": false,
            "removedFiles": [],
            "removedBlocks": [],
            "conflicts": []
        })),
        Err(err) => Err(Error::Init(format!(
            "baseline overlay uninstall failed: {err}"
        ))),
    }
}

/// Records the Repoise guidance state of a repoise trial workspace.
fn repoise_guidance(workspace: &Path) -> Value {
    let overlay = crate::overlay::load_manifest(workspace);
    serde_json::json!({
        "retrieval": "repoise",
        "overlayPresent": overlay.is_ok(),
        "templateVersion": overlay.as_ref().ok().map(|m| m.template_version.clone()),
        "agentsMdHash": hash_optional(workspace.join("AGENTS.md"))
    })
}

fn hash_optional(path: PathBuf) -> Option<String> {
    std::fs::read(&path).ok().map(crate::hash::sha256_hex)
}

/// Mode enforcement record: the requested mode is always logged; the
/// effective mode is only verifiable through adapter tool telemetry, and is
/// declared unverified otherwise (a degraded lexical call is never counted
/// as hybrid execution).
fn host_elapsed_ms(host_result: &Value) -> Option<u128> {
    host_result
        .get("elapsedMs")
        .and_then(Value::as_f64)
        .and_then(|value| (value >= 0.0).then_some(value.round() as u128))
}

/// The adapter's observed harness version for this trial: the Cline adapter
/// emits a `harness_version` note event carrying `data.revision` when the
/// version probe succeeded. Null when the adapter could not resolve it (or
/// the adapter does not report it), so drift detection never invents a
/// version.
fn observed_harness_version(host_result: &Value) -> Value {
    host_result
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| {
            events.iter().find(|event| {
                event.get("t").and_then(Value::as_str) == Some("note")
                    && event.get("code").and_then(Value::as_str) == Some("harness_version")
            })
        })
        .and_then(|event| event.pointer("/data/revision"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Summarizes one trial's MCP server audit sink (the coordinator's
/// server-side observation of the trial's pinned `repoise mcp` process).
/// The raw NDJSON stays in the trial directory; this summary is what the
/// report consumes.
///
/// `status` semantics — missing evidence is never zero use or a pass:
/// - `unavailable`: the sink file is absent or unreadable (the server may
///   never have started, or could not open the sink);
/// - `observed`: at least one search tool call was recorded (a clean
///   `server_end` is recorded separately as `cleanShutdown`);
/// - `incomplete`: no search tool call was recorded and the audit stream
///   never reached a clean `server_end` (cancelled/killed server,
///   truncated write, or a broken stdio transport);
/// - `not_observed`: a clean, complete audit stream with no
///   `search_project_knowledge` tool calls and no malformed records (a
///   lost/truncated line could have been a search, so any malformed record —
///   including an `audit_error` marker replacing a dropped record — keeps
///   the stream `incomplete`);
///
/// A stream is complete only when every recorded server is matched: the
/// final `server_end` has reason `stdin_closed` and no `server_start` is
/// left open (the sink is append-only across re-spawns, so a later
/// cancelled/killed server without a final end leaves its tail unknown);
/// a `stdin_error`/`stdout_error` end, a missing `server_end`, or more
/// ends than starts all leave the stream incomplete.
///
/// Only measurements the server itself made are aggregated: per-tool call
/// counts, search success/failure, requested/effective retrieval modes and
/// degradation counts. Malformed records are counted, never propagated.
pub fn summarize_mcp_audit(path: &Path) -> Value {
    let mut notes: Vec<String> = Vec::new();
    let text: Option<String> = match fs::File::open(path) {
        Ok(file) => match file.metadata() {
            Ok(meta) if meta.len() > MAX_AUDIT_READ_BYTES => {
                notes.push(format!(
                    "audit sink larger than {MAX_AUDIT_READ_BYTES} bytes; not read"
                ));
                None
            }
            Ok(_) => match fs::read_to_string(path) {
                Ok(text) => Some(text),
                Err(err) => {
                    notes.push(format!("audit sink unreadable: {err}"));
                    None
                }
            },
            Err(err) => {
                notes.push(format!("audit sink unreadable: {err}"));
                None
            }
        },
        Err(err) => {
            notes.push(format!(
                "audit sink not found: {err} (the server may never have started, \
                 or it could not open the sink)"
            ));
            None
        }
    };
    let Some(text) = text else {
        return serde_json::json!({
            "schemaVersion": AUDIT_SCHEMA_VERSION,
            "source": AUDIT_SOURCE,
            "status": "unavailable",
            "notes": notes,
            "searches": null_value()
        });
    };

    let mut tool_calls = serde_json::Map::new();
    let mut searches_succeeded = 0u64;
    let mut searches_failed = 0u64;
    let mut searches_degraded = 0u64;
    let mut effective_modes: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut malformed = 0u64;
    let mut server_end: Option<Value> = None;
    let mut started = false;
    let mut open_servers = 0i64;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            malformed += 1;
            continue;
        };
        match record.get("kind").and_then(Value::as_str) {
            Some("server_start") => {
                started = true;
                open_servers += 1;
            }
            Some("server_end") => {
                open_servers -= 1;
                server_end = Some(record.get("reason").cloned().unwrap_or(Value::Null))
            }
            Some("tool_call") => {
                let tool = record
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                bump(&mut tool_calls, &tool);
                if tool == "search_project_knowledge" {
                    if record.get("success").and_then(Value::as_bool) == Some(true) {
                        searches_succeeded += 1;
                        if record.get("degraded").and_then(Value::as_bool) == Some(true) {
                            searches_degraded += 1;
                        }
                        if let Some(effective) = record
                            .get("effectiveMode")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                        {
                            bump(&mut effective_modes, &effective);
                        }
                    } else {
                        searches_failed += 1;
                    }
                }
            }
            // A dropped record (the `audit_error` marker) could have been a
            // search, so it counts as malformed exactly like an unreadable
            // line.
            Some("audit_error") => malformed += 1,
            Some(_) | None => malformed += 1,
        }
    }
    // Completeness follows the function doc: a clean, fully matched stream.
    let clean =
        server_end.as_ref().and_then(Value::as_str) == Some("stdin_closed") && open_servers == 0;
    let succeeded = searches_succeeded;
    if malformed > 0 {
        notes.push(format!("{malformed} malformed audit record(s) skipped"));
    }
    if started && !clean {
        if open_servers > 0 {
            notes.push(format!(
                "audit stream is incomplete: {open_servers} server_start record(s) have no \
                 matching server_end (a later re-spawned server was cancelled, killed or \
                 its final write was truncated); the stream after its last recorded \
                 event is unknown"
            ));
        } else if open_servers < 0 {
            notes.push(
                "audit stream is incomplete: more server_end records than server_start \
                 (the sink was tampered with or predates this server)"
                    .to_string(),
            );
        } else {
            match server_end.as_ref() {
                Some(reason) => notes.push(format!(
                    "audit stream is incomplete: server_end reason {reason} is not a clean \
                     shutdown (the stdio transport broke); the stream after the last \
                     recorded event is unknown"
                )),
                None => notes.push(
                    "audit stream is incomplete: no clean server_end record (the server was \
                     cancelled, killed or its final write was truncated)"
                        .to_string(),
                ),
            }
        }
    }
    if !started {
        notes.push(
            "no server_start record: the sink predates this server or was tampered with"
                .to_string(),
        );
    }
    let status = if succeeded > 0 || searches_failed > 0 {
        "observed"
    } else if clean && malformed == 0 {
        // A lost (malformed) line could have been a search: never report
        // "no searches happened" when part of the stream is unreadable.
        "not_observed"
    } else {
        "incomplete"
    };
    let mut searches = serde_json::Map::new();
    searches.insert("succeeded".to_string(), Value::from(succeeded));
    searches.insert("failed".to_string(), Value::from(searches_failed));
    searches.insert("degraded".to_string(), Value::from(searches_degraded));
    searches.insert("effective".to_string(), Value::Object(effective_modes));
    serde_json::json!({
        "schemaVersion": AUDIT_SCHEMA_VERSION,
        "source": AUDIT_SOURCE,
        "status": status,
        "cleanShutdown": clean,
        "toolCalls": Value::Object(tool_calls),
        "searches": Value::Object(searches),
        "malformedRecords": malformed,
        "notes": notes
    })
}

fn mode_record(condition: &Condition, host_result: &Value, server_audit: Option<&Value>) -> Value {
    let requested = if condition.retrieval_mode == "repoise" {
        condition.search_mode.clone()
    } else {
        None
    };
    if requested.is_none() {
        return serde_json::json!({
            "requested": null_value(),
            "effective": null_value(),
            "source": null_value(),
            "note": "baseline: no Repoise retrieval"
        });
    }
    // Server-side observation first: the trial's pinned MCP server wrote an
    // audit sink naming each tool call and the retrieval mode each completed
    // search actually reported. A pinned `--search-mode` argv or a passed
    // vector preflight is configuration, not observation — only a completed
    // hybrid search counts as observed hybrid.
    let Some(audit) = server_audit else {
        // Baseline or an older trial record without server-side audit.
        let tool_telemetry = host_result
            .pointer("/hello/capabilities/tool_telemetry")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if tool_telemetry {
            let events = host_result.get("events").and_then(Value::as_array);
            let used = events.is_some_and(|events| {
                events.iter().any(|event| {
                    matches!(
                        event.get("t").and_then(Value::as_str),
                        Some("tool_start") | Some("tool_end")
                    ) && event
                        .get("tool")
                        .and_then(Value::as_str)
                        .is_some_and(|tool| tool.contains("repoise"))
                })
            });
            return serde_json::json!({
                "requested": requested,
                "effective": if used { "observed" } else { "not_observed" },
                "source": "adapter telemetry",
                "note": "tool-call presence from the adapter tool telemetry event stream; the executed search mode is enforced by the pinned MCP server (a vectorless trial index fails setup)"
            });
        }
        return serde_json::json!({
            "requested": requested,
            "effective": null_value(),
            "source": "configuration only",
            "note": "unverified: no server-side audit sink and the adapter declares no tool telemetry; the pinned MCP server mode is the enforcement boundary, runtime degradation is not observable"
        });
    };
    let audit_status = audit
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let effective = if audit_status == "observed" {
        audit
            .pointer("/searches/effective")
            .and_then(Value::as_object)
            .and_then(|modes| {
                let first = |name: &str| {
                    modes.iter().find(|(mode, count)| {
                        mode.as_str() == name && count.as_u64().unwrap_or(0) > 0
                    })
                };
                first("hybrid")
                    .or_else(|| first("lexical"))
                    .or_else(|| first("vectors-only"))
            })
            .map(|(mode, _)| mode.clone())
    } else {
        None
    };
    let note = match audit_status {
        "observed" => format!(
            "server-side observation: the trial's MCP server recorded tool calls; observed retrieval modes: {:?}",
            effective_modes_debug(audit)
        ),
        "not_observed" => {
            "server-side observation: the audit stream is complete (clean server end) but recorded no \
             search_project_knowledge calls; the configured mode was not exercised"
                .to_string()
        }
        "incomplete" => {
            "server-side observation incomplete: the audit stream never reached a clean server end \
             (cancellation, kill or truncation); observed calls so far are reported, but absence of \
             evidence is not evidence of no use"
                .to_string()
        }
        _ => {
            "server-side observation unavailable: the trial's audit sink is absent or unreadable; \
             the absence of audit records is not evidence of no tool use"
                .to_string()
        }
    };
    serde_json::json!({
        "requested": requested,
        "effective": effective,
        "source": AUDIT_SOURCE,
        "observationStatus": audit_status,
        "note": note
    })
}

/// The per-mode counts of completed searches in a summarized audit sink
/// (debug display for the mode record note).
fn effective_modes_debug(audit: &Value) -> Vec<String> {
    audit
        .pointer("/searches/effective")
        .and_then(Value::as_object)
        .map(|modes| {
            modes
                .iter()
                .map(|(mode, count)| format!("{mode}={}", count.as_u64().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug)]
struct IndexInfo {
    generation_id: i64,
    manifest_hash: String,
    vectors_total: u64,
    vectors_pending: u64,
    vectors_failed: u64,
    elapsed_ms: u128,
}

/// A hybrid/vectors-only condition needs a fully published vector set: at
/// least one vector, nothing left pending (budget/cancel) and nothing
/// failed (batch failures after retries). A partially embedded index must
/// not be recorded as hybrid. The pending/failed counts come from the
/// embedding session of `repoise index`, which only ever handles chunks
/// eligible under the configured embedding scope (docs, or docs + code), so
/// this per-trial gate is scope-aware by construction: config and other
/// lexical-only chunks never block a hybrid trial (the same denominator
/// fix as the root preflight's scope-aware coverage).
fn index_viable_for_vectors(index: &IndexInfo) -> bool {
    index.vectors_total > 0 && index.vectors_pending == 0 && index.vectors_failed == 0
}

/// A failed trial setup: the stable recorded reason plus the sanitized,
/// bounded diagnostic detail for `trial.json` and the run summary.
#[derive(Debug)]
struct SetupError {
    reason: &'static str,
    detail: String,
}

/// Maps a bounded setup-child failure to its recorded reason and detail.
/// Setup timeouts and setup cancellations are distinct infra outcomes,
/// never agent outcomes: they are separate from the agent wall-clock
/// timeout and never charged to `agentMs`.
fn setup_error(ctx: &RunContext, failure: SetupFailure, limit_ms: u64) -> SetupError {
    match failure.kind {
        SetupFailureKind::Timeout => SetupError {
            reason: "setup_timeout",
            detail: bounded_detail(&redact_setup_secrets(
                &format!(
                    "index setup exceeded the {limit_ms} ms setup bound; the index child \
                     and its process tree were terminated before completion\n{}",
                    failure.captured
                ),
                ctx,
            )),
        },
        SetupFailureKind::Interrupted => SetupError {
            reason: "setup_cancelled",
            detail: bounded_detail(&redact_setup_secrets(
                &format!(
                    "index setup was terminated by an experiment interrupt before completion\n{}",
                    failure.captured
                ),
                ctx,
            )),
        },
        SetupFailureKind::Coordinator(message) => SetupError {
            reason: "index_build_failed",
            detail: bounded_detail(&redact_setup_secrets(&message, ctx)),
        },
    }
}

/// Replaces live secret values in captured setup diagnostics: the setup
/// child inherits the coordinator's environment (including the embedding
/// endpoint/API-key values), and a provider error can echo them — recorded
/// errors are shareable artifacts.
fn redact_setup_secrets(text: &str, ctx: &RunContext) -> String {
    let mut values = Vec::new();
    for name in [
        ctx.preflight.embedding_endpoint_env.as_deref(),
        ctx.preflight.embedding_api_key_env.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            values.push(value);
        }
    }
    redact_values(text, &values)
}

/// Replaces live secret values with a placeholder (split out so it is
/// testable without mutating the process environment).
fn redact_values(text: &str, values: &[String]) -> String {
    let mut out = text.to_string();
    for value in values {
        out = out.replace(value, "[REDACTED]");
    }
    out
}

/// Keeps only the bounded tail of a diagnostic for a recorded error.
fn bounded_detail(text: &str) -> String {
    if text.len() <= MAX_SETUP_ERROR_BYTES {
        return text.to_string();
    }
    let start = text.ceil_char_boundary(MAX_SETUP_ERROR_BYTES);
    format!("…(truncated) {}", &text[start..])
}

/// Builds the trial worktree index through the same `repoise index`
/// operation. The index child is coordinator-owned subprocess work: it is
/// bounded by the per-trial setup wall clock (separate from, and never
/// charged to, the timed agent interval), cancellable through the
/// experiment interrupt, and terminated as a process tree when its bound
/// fires or the interrupt lands. The experiment cache directory is
/// declared in the manifest.
fn build_trial_index(
    ctx: &RunContext,
    workspace: &Path,
    limit_ms: u64,
    interrupted: &AtomicBool,
) -> Result<IndexInfo, SetupError> {
    let started = Instant::now();
    let mut command = Command::new(&ctx.self_exe);
    command
        .arg("index")
        .arg(workspace)
        .arg("--json")
        .env(crate::CACHE_ENV_VAR, &ctx.layout.cache_dir);
    if ctx.preflight.snapshot_kind == "commit" {
        command.arg("--committed");
    }
    let output = run_bounded_setup_command(command, Duration::from_millis(limit_ms), interrupted)
        .map_err(|failure| setup_error(ctx, failure, limit_ms))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(setup_error(
            ctx,
            SetupFailure {
                kind: SetupFailureKind::Coordinator(format!(
                    "index exited {}: {stderr}",
                    output.status.code().unwrap_or(-1)
                )),
                captured: String::new(),
            },
            limit_ms,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim())
        .or_else(|_| {
            stdout
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .and_then(|line| serde_json::from_str(line).ok())
                .ok_or_else(|| "index produced no JSON output".to_string())
        })
        .map_err(|err| SetupError {
            reason: "index_build_failed",
            detail: err.to_string(),
        })?;
    Ok(IndexInfo {
        generation_id: value["generation_id"].as_i64().unwrap_or(0),
        manifest_hash: value["manifest_hash"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        vectors_total: value["vectors_total"].as_u64().unwrap_or(0),
        vectors_pending: value["embedding"]["pending"].as_u64().unwrap_or(0),
        vectors_failed: value["embedding"]["failed"].as_u64().unwrap_or(0),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

struct HostRun {
    result: Value,
    status: String,
    reason: Option<String>,
}

/// Spawns the host bridge for one trial, drives cancellation, and returns
/// the host result (or `Value::Null` with a coordinator status when the
/// bridge produced none).
fn run_host(
    ctx: &RunContext,
    trial: &Trial,
    request: &Value,
    trial_dir: &Path,
    interrupted: &AtomicBool,
) -> Result<HostRun, Error> {
    let request_line =
        serde_json::to_string(request).map_err(|err| Error::Json(err.to_string()))?;
    let adapter_dir = ctx.layout.adapter_dir(&trial.trial_id);

    let mut command = Command::new(&ctx.preflight.node);
    command
        .arg(&ctx.preflight.bridge)
        .arg(&ctx.preflight.runner);
    for arg in &ctx.suite.adapter.args {
        command.arg(arg);
    }
    let mut env = std::collections::HashMap::new();
    for name in ["PATH", "HOME", "USERPROFILE", "SystemRoot", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            env.insert(name.to_string(), value);
        }
    }
    let names: Vec<String> = ctx
        .adapter_env
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    env.insert("ADAPTER_ENV_VARS".into(), names.join(",").into());
    for (name, value) in &ctx.adapter_env {
        env.insert(name.clone(), value.clone().into());
    }
    let mut child = command
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            Error::Init(format!(
                "{}: host bridge spawn failed: {err}",
                trial.trial_id
            ))
        })?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    if let Err(err) = stdin
        .write_all(format!("{request_line}\n").as_bytes())
        .map_err(Error::Io)
    {
        // The bridge is running; never leave it behind on a request failure.
        force_kill(&mut child, &adapter_dir);
        return Err(err);
    }
    // Stdin stays open: a later `cancel` control line is the graceful
    // cross-platform cancellation channel (no signal needed on Windows).

    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_handle = std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = BufReader::new(stdout_pipe).read_to_end(&mut all);
        all
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = BufReader::new(stderr_pipe).read_to_end(&mut all);
        all
    });

    // The per-trial wall bound is always in effect: the suite value, or the
    // coordinator default when omitted (the host arms no timeout on its own).
    let wall_clock_ms = ctx
        .suite
        .limits
        .wall_clock_ms
        .unwrap_or(super::suite::DEFAULT_TRIAL_WALL_CLOCK_MS);
    let watchdog_deadline =
        Instant::now() + Duration::from_millis(wall_clock_ms) + OUTER_WATCHDOG_MARGIN;
    let mut cancel_started: Option<Instant> = None;
    let mut watchdog_fired = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if cancel_started.is_none() && interrupted.load(Ordering::SeqCst) {
                    cancel_started = Some(Instant::now());
                    let _ = stdin.write_all(b"cancel\n");
                    let _ = stdin.flush();
                }
                if let Some(started_at) = cancel_started
                    && started_at.elapsed() >= CANCEL_GRACE
                {
                    force_kill(&mut child, &adapter_dir);
                    break;
                } else if Instant::now() >= watchdog_deadline {
                    watchdog_fired = true;
                    force_kill(&mut child, &adapter_dir);
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                force_kill(&mut child, &adapter_dir);
                return Err(Error::Init(format!("{}: {err}", trial.trial_id)));
            }
        }
    }
    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    let host_log = trial_dir.join("host.log");
    for line in String::from_utf8_lossy(&stderr_bytes).lines() {
        let _ = artifacts::append_line(&host_log, line);
    }
    if watchdog_fired {
        let _ = artifacts::append_line(
            &host_log,
            "coordinator watchdog: forced the host bridge and its tree after the outer deadline",
        );
    }

    let result = artifacts::read_to_string(&adapter_dir.join("result.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .or_else(|| {
            String::from_utf8_lossy(&stdout_bytes)
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .and_then(|line| serde_json::from_str(line).ok())
        });
    let result = result.unwrap_or(Value::Null);
    let cancelled = cancel_started.is_some();
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if cancelled {
                "cancelled".to_string()
            } else {
                "infra_error".to_string()
            }
        });
    // Adapter host statuses collapse onto the bench trial vocabulary:
    // protocol/adapter failures and wall-clock timeouts are trial errors.
    let status = if matches!(status.as_str(), "completed" | "cancelled" | "infra_error") {
        status
    } else {
        "error".to_string()
    };
    let reason = result
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            if watchdog_fired {
                Some("host_watchdog".into())
            } else if cancelled && result.is_null() {
                Some("coordinator_interrupt".into())
            } else if result.is_null() {
                Some("no_host_result".into())
            } else {
                None
            }
        });
    Ok(HostRun {
        result,
        status,
        reason,
    })
}

/// Force-kills the bridge and the recorded adapter child tree (best effort).
fn force_kill(child: &mut Child, adapter_dir: &Path) {
    let pid = child.id();
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    } else {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .output();
        // Best-effort sweep of the adapter's own recorded process group.
        if let Ok(raw) = std::fs::read_to_string(adapter_dir.join("child-pid.txt"))
            && let Ok(child_pid) = raw.trim().parse::<i32>()
            && child_pid > 1
        {
            let _ = Command::new("kill")
                .args(["-KILL", &format!("-{child_pid}")])
                .output();
        }
    }
    let _ = child.wait();
}

/// Progress line: human text on stdout, JSON-ish on stderr under --json.
pub fn progress(json: bool, message: &str) {
    if json {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

/// The UTC RFC-3339 timestamp used across manifests and reports.
pub fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(secs / 86_400);
    let hour = secs % 86400 / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Gregorian calendar date from days since 1970-01-01 (Howard Hinnant's
/// `civil_from_days` algorithm).
fn civil_from_days(secs: u64) -> (u64, u32, u32) {
    let z = secs as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if month <= 2 { y + 1 } else { y }) as u64, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_info(total: u64, pending: u64, failed: u64) -> IndexInfo {
        IndexInfo {
            generation_id: 1,
            manifest_hash: "hash".into(),
            vectors_total: total,
            vectors_pending: pending,
            vectors_failed: failed,
            elapsed_ms: 0,
        }
    }

    #[test]
    fn hybrid_gate_requires_a_fully_published_vector_set() {
        assert!(index_viable_for_vectors(&index_info(42, 0, 0)));
        // No vectors at all.
        assert!(!index_viable_for_vectors(&index_info(0, 0, 0)));
        // Budget/cancel left inputs pending.
        assert!(!index_viable_for_vectors(&index_info(42, 3, 0)));
        // A partly failed embedding run (some batches failed after retries)
        // must not be recorded as hybrid.
        assert!(!index_viable_for_vectors(&index_info(42, 0, 2)));
    }

    #[test]
    fn redact_request_replaces_mcp_env_values_only() {
        let request = serde_json::json!({
            "schemaVersion": 1,
            "task": { "id": "task-1", "version": "1" },
            "scope": "read",
            "condition": {
                "retrieval": "repoise",
                "mcp": {
                    "argv": ["repoise", "mcp"],
                    "env": {
                        "REPOISE_CACHE_DIR": "/tmp/bench-cache",
                        "OPENAI_API_KEY": "sk-secret"
                    }
                }
            },
            "limits": { "wallClockMs": 900000 }
        });
        let redacted = redact_request(&request);
        // Names are kept, every env value is replaced.
        assert_eq!(
            redacted["condition"]["mcp"]["env"]["REPOISE_CACHE_DIR"],
            "[REDACTED]"
        );
        assert_eq!(
            redacted["condition"]["mcp"]["env"]["OPENAI_API_KEY"],
            "[REDACTED]"
        );
        // The live request stays untouched, everything else is preserved.
        assert_eq!(
            request["condition"]["mcp"]["env"]["OPENAI_API_KEY"],
            "sk-secret"
        );
        assert_eq!(redacted["limits"]["wallClockMs"], 900000);
        assert_eq!(redacted["task"]["id"], "task-1");

        // A baseline request has no mcp section: unchanged.
        let baseline = serde_json::json!({ "condition": { "retrieval": "baseline" } });
        assert_eq!(redact_request(&baseline), baseline);
    }

    /// The bridge reports `elapsedMs` as a fractional millisecond value
    /// (`hrtime`-derived), so it must be read as a float and rounded —
    /// `as_u64` rejects every non-integer and would force the coordinator
    /// fallback for every trial.
    #[test]
    fn host_elapsed_ms_reads_fractional_values() {
        assert_eq!(
            host_elapsed_ms(&serde_json::json!({ "elapsedMs": 1234.567 })),
            Some(1235)
        );
        assert_eq!(
            host_elapsed_ms(&serde_json::json!({ "elapsedMs": 0.4 })),
            Some(0)
        );
        assert_eq!(
            host_elapsed_ms(&serde_json::json!({ "elapsedMs": 1000 })),
            Some(1000)
        );
        assert_eq!(
            host_elapsed_ms(&serde_json::json!({ "elapsedMs": -5.2 })),
            None
        );
        assert_eq!(
            host_elapsed_ms(&serde_json::json!({ "elapsedMs": "fast" })),
            None
        );
        assert_eq!(host_elapsed_ms(&serde_json::json!({})), None);
    }

    #[test]
    fn observed_harness_version_reads_the_probe_note_only() {
        // The adapter's resolved version arrives as a `harness_version` note.
        let host = serde_json::json!({
            "events": [
                { "t": "note", "code": "harness_version", "data": { "surface": "cli", "revision": "3.0.74" } },
                { "t": "note", "code": "something_else", "data": { "revision": "9.9.9" } }
            ]
        });
        assert_eq!(observed_harness_version(&host), serde_json::json!("3.0.74"));

        // No probe note (probe failed, or adapter silent) is null, never a guess.
        assert_eq!(
            observed_harness_version(&serde_json::json!({ "events": [] })),
            Value::Null
        );
        assert_eq!(
            observed_harness_version(&serde_json::json!({})),
            Value::Null
        );
    }
    #[test]
    fn mcp_env_carries_cache_and_embedding_variables() {
        // Simulated environment: the declared embedding variables are set.
        let lookup = |name: &str| match name {
            "REPOISE_BENCH_MCP_ENV_TEST_ENDPOINT_7F3A" => Some("https://embed.test/v1".to_string()),
            "REPOISE_BENCH_MCP_ENV_TEST_KEY_7F3A" => Some("sk-test".to_string()),
            _ => None,
        };
        let env = mcp_server_env_with(
            Path::new("/tmp/bench-cache"),
            Some("REPOISE_BENCH_MCP_ENV_TEST_ENDPOINT_7F3A"),
            Some("REPOISE_BENCH_MCP_ENV_TEST_KEY_7F3A"),
            &lookup,
        );
        assert_eq!(env[crate::CACHE_ENV_VAR], "/tmp/bench-cache");
        // The trial's MCP server must see the same embedding credentials the
        // coordinator's index child saw, or a hybrid query degrades to
        // lexical.
        assert_eq!(
            env["REPOISE_BENCH_MCP_ENV_TEST_ENDPOINT_7F3A"],
            "https://embed.test/v1"
        );
        assert_eq!(env["REPOISE_BENCH_MCP_ENV_TEST_KEY_7F3A"], "sk-test");

        // Unset variables are omitted (lexical conditions need none, and a
        // hybrid trial with no vectors fails setup instead).
        let env = mcp_server_env_with(
            Path::new("/tmp/bench-cache"),
            Some("REPOISE_BENCH_MCP_ENV_TEST_UNSET_7F3A"),
            None,
            &lookup,
        );
        assert!(env.get("REPOISE_BENCH_MCP_ENV_TEST_UNSET_7F3A").is_none());
    }

    #[test]
    fn mcp_argv_pins_search_mode_and_snapshot_mode() {
        let argv = mcp_argv(
            Path::new("/opt/repoise"),
            Path::new("/tmp/trial-worktree"),
            "lexical",
            true,
            None,
        );
        assert_eq!(
            argv,
            vec![
                "/opt/repoise".to_string(),
                "mcp".to_string(),
                "/tmp/trial-worktree".to_string(),
                "--search-mode".to_string(),
                "lexical".to_string(),
                "--committed".to_string()
            ]
        );
        let argv = mcp_argv(Path::new("repoise"), Path::new("wt"), "hybrid", false, None);
        assert_eq!(
            argv,
            vec![
                "repoise".to_string(),
                "mcp".to_string(),
                "wt".to_string(),
                "--search-mode".to_string(),
                "hybrid".to_string()
            ]
        );
        // The per-trial audit sink is appended only when designated.
        let argv = mcp_argv(
            Path::new("repoise"),
            Path::new("wt"),
            "hybrid",
            false,
            Some(Path::new("/tmp/trial/mcp-audit.ndjson")),
        );
        assert_eq!(
            argv,
            vec![
                "repoise".to_string(),
                "mcp".to_string(),
                "wt".to_string(),
                "--search-mode".to_string(),
                "hybrid".to_string(),
                "--audit-file".to_string(),
                "/tmp/trial/mcp-audit.ndjson".to_string()
            ]
        );
    }

    fn audit_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("repoise-bench-audit-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write_audit(path: &Path, lines: &[&str]) {
        std::fs::write(path, lines.join("\n") + "\n").expect("write audit fixture");
    }

    #[test]
    fn summarize_mcp_audit_counts_observed_searches_and_modes() {
        let dir = audit_dir("observed");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start","server":{"repoId":"repo","worktreeId":"wt"}}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_knowledge","requestedMode":"hybrid","effectiveMode":"hybrid","degraded":false,"servedGeneration":7,"success":true,"durationMs":12}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"read_project_knowledge","requestedMode":null,"effectiveMode":null,"degraded":null,"servedGeneration":null,"success":true,"durationMs":3}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"project_knowledge_status","requestedMode":null,"effectiveMode":null,"degraded":null,"servedGeneration":null,"success":true,"durationMs":2}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_knowledge","requestedMode":"hybrid","effectiveMode":"lexical","degraded":true,"degradationReason":"degraded_to_lexical","servedGeneration":7,"success":true,"durationMs":5}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "observed");
        assert_eq!(summary["source"], "server-side observation");
        assert_eq!(summary["cleanShutdown"], true);
        assert_eq!(summary["toolCalls"]["search_project_knowledge"], 2);
        assert_eq!(summary["toolCalls"]["read_project_knowledge"], 1);
        assert_eq!(summary["toolCalls"]["project_knowledge_status"], 1);
        assert_eq!(summary["searches"]["succeeded"], 2);
        assert_eq!(summary["searches"]["failed"], 0);
        assert_eq!(summary["searches"]["degraded"], 1);
        assert_eq!(summary["searches"]["effective"]["hybrid"], 1);
        assert_eq!(summary["searches"]["effective"]["lexical"], 1);
        assert_eq!(summary["malformedRecords"], 0);
        assert!(summary["notes"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_missing_sink_is_unavailable() {
        let dir = audit_dir("missing");
        let path = dir.join(AUDIT_SINK_FILENAME);
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "unavailable");
        assert_eq!(summary["searches"], Value::Null);
        // The absence of records must be explained, not treated as zero use.
        let notes = summary["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().is_some_and(|s| s.contains("not found")))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_incomplete_without_clean_end() {
        let dir = audit_dir("incomplete");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_knowledge","requestedMode":"lexical","effectiveMode":"lexical","degraded":false,"success":true}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        // A recorded search exists, but the stream never ended cleanly
        // (cancellation/kill/truncation): the use is observed, the missing
        // clean end is reported as such, and absence of further evidence is
        // not evidence of no use.
        assert_eq!(summary["status"], "observed");
        assert_eq!(summary["cleanShutdown"], false);
        assert_eq!(summary["searches"]["succeeded"], 1);
        let notes = summary["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().is_some_and(|s| s.contains("incomplete")))
        );
        // No search records and no clean end: incomplete, unknown — not
        // zero use.
        let partial = dir.join("partial.ndjson");
        write_audit(&partial, &[r#"{"schemaVersion":1,"kind":"server_start"}"#]);
        let partial_summary = summarize_mcp_audit(&partial);
        assert_eq!(partial_summary["status"], "incomplete");
        assert_eq!(partial_summary["searches"]["succeeded"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_not_observed_when_complete_but_no_search() {
        let dir = audit_dir("not_observed");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"read_project_knowledge","success":true}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "not_observed");
        assert_eq!(summary["cleanShutdown"], true);
        assert_eq!(summary["searches"]["succeeded"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_error_marker_keeps_stream_incomplete() {
        // An `audit_error` marker replaces a dropped record that could have
        // been a search: it counts as malformed, so even a complete, clean
        // stream is never `not_observed`.
        let dir = audit_dir("audit_error");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"audit_error"}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "incomplete");
        assert_eq!(summary["cleanShutdown"], true);
        assert_eq!(summary["malformedRecords"], 1);
        assert_eq!(summary["searches"]["succeeded"], 0);
        let notes = summary["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().is_some_and(|s| s.contains("malformed")))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_respawned_stream_keeps_counts() {
        // The host may re-spawn the pinned server inside one trial; the
        // append-only sink then holds two `server_start` records and one
        // final `server_end`. Counts must survive, and the earlier
        // records must not be lost.
        let dir = audit_dir("respawn");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_knowledge","requestedMode":"hybrid","effectiveMode":"hybrid","degraded":false,"success":true}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_error"}"#,
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_knowledge","requestedMode":"hybrid","effectiveMode":"lexical","degraded":true,"success":true}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "observed");
        assert_eq!(summary["cleanShutdown"], true);
        assert_eq!(summary["searches"]["succeeded"], 2);
        assert_eq!(summary["searches"]["degraded"], 1);
        assert_eq!(summary["searches"]["effective"]["hybrid"], 1);
        assert_eq!(summary["searches"]["effective"]["lexical"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_broken_transport_is_not_not_observed() {
        // A `server_end` with a non-`stdin_closed` reason means the stdio
        // transport broke: with no recorded searches the stream is
        // incomplete (unknown), never "no searches happened".
        let dir = audit_dir("transport");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"read_project_knowledge","success":true}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdout_error"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "incomplete");
        assert_eq!(summary["cleanShutdown"], false);
        assert_eq!(summary["searches"]["succeeded"], 0);
        let notes = summary["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().is_some_and(|s| s.contains("stdout_error")))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_malformed_record_blocks_not_observed() {
        // A garbled/truncated line between a clean start and end could have
        // been a search: the stream is incomplete, never not_observed.
        let dir = audit_dir("malformed-clean");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_know"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "incomplete");
        assert_eq!(summary["cleanShutdown"], true);
        assert_eq!(summary["searches"]["succeeded"], 0);
        assert_eq!(summary["malformedRecords"], 1);
        let notes = summary["notes"].as_array().unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.as_str().is_some_and(|s| s.contains("malformed")))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_respawn_without_final_end_is_incomplete() {
        // A re-spawned server that is cancelled/killed without a final
        // clean end leaves its tail unknown, even though an earlier server
        // ended cleanly: incomplete, never not_observed (zero use).
        let dir = audit_dir("respawn-open");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#,
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        assert_eq!(summary["status"], "incomplete");
        assert_eq!(summary["cleanShutdown"], false);
        assert_eq!(summary["searches"]["succeeded"], 0);
        let notes = summary["notes"].as_array().unwrap();
        assert!(notes.iter().any(|note| {
            note.as_str()
                .is_some_and(|s| s.contains("no matching server_end"))
        }));
        // More ends than starts (tampered or pre-existing sink) is not
        // clean either.
        let tampered = dir.join("tampered.ndjson");
        write_audit(
            &tampered,
            &[r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_closed"}"#],
        );
        let tampered_summary = summarize_mcp_audit(&tampered);
        assert_eq!(tampered_summary["status"], "incomplete");
        assert_eq!(tampered_summary["cleanShutdown"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_mcp_audit_counts_malformed_and_failed_searches() {
        let dir = audit_dir("malformed");
        let path = dir.join(AUDIT_SINK_FILENAME);
        write_audit(
            &path,
            &[
                r#"{"schemaVersion":1,"kind":"server_start"}"#,
                "not-json{",
                r#"{"schemaVersion":1,"kind":"tool_call","tool":"search_project_knowledge","requestedMode":"hybrid","effectiveMode":null,"degraded":null,"success":false}"#,
                r#"{"schemaVersion":1,"kind":"server_end","reason":"stdin_error"}"#,
            ],
        );
        let summary = summarize_mcp_audit(&path);
        // A failed search is still evidence the tool was invoked.
        assert_eq!(summary["status"], "observed");
        assert_eq!(summary["searches"]["failed"], 1);
        assert_eq!(summary["malformedRecords"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn repoise_condition() -> Condition {
        Condition {
            id: "repoise-hybrid".to_string(),
            retrieval_mode: "repoise".to_string(),
            search_mode: Some("hybrid".to_string()),
            required_capabilities: vec![],
        }
    }

    fn baseline_condition() -> Condition {
        Condition {
            id: "baseline".to_string(),
            retrieval_mode: "baseline".to_string(),
            search_mode: None,
            required_capabilities: vec![],
        }
    }

    #[test]
    fn mode_record_baseline_has_no_mode() {
        let record = mode_record(
            &baseline_condition(),
            &Value::Object(serde_json::Map::new()),
            None,
        );
        assert_eq!(record["requested"], Value::Null);
        assert_eq!(record["note"], "baseline: no Repoise retrieval");
    }

    #[test]
    fn mode_record_server_audit_observed_hybrid() {
        let audit = serde_json::json!({
            "status": "observed",
            "searches": { "succeeded": 1, "failed": 0, "degraded": 0, "effective": { "hybrid": 1 } }
        });
        let record = mode_record(
            &repoise_condition(),
            &Value::Object(serde_json::Map::new()),
            Some(&audit),
        );
        assert_eq!(record["requested"], "hybrid");
        assert_eq!(record["effective"], "hybrid");
        assert_eq!(record["source"], "server-side observation");
        assert_eq!(record["observationStatus"], "observed");
    }

    #[test]
    fn mode_record_server_audit_not_observed_is_not_a_pass() {
        let audit = serde_json::json!({ "status": "not_observed" });
        let record = mode_record(
            &repoise_condition(),
            &Value::Object(serde_json::Map::new()),
            Some(&audit),
        );
        // Configured hybrid, but the audit proves no search ran: the
        // effective mode is null, not "hybrid" and not "observed".
        assert_eq!(record["effective"], Value::Null);
        assert_eq!(record["observationStatus"], "not_observed");
        assert_eq!(record["source"], "server-side observation");
    }

    #[test]
    fn mode_record_unverified_without_audit_or_telemetry() {
        let host = serde_json::json!({
            "hello": { "capabilities": { "tool_telemetry": false } }
        });
        let record = mode_record(&repoise_condition(), &host, None);
        assert_eq!(record["effective"], Value::Null);
        assert_eq!(record["source"], "configuration only");
        assert!(record["note"].as_str().unwrap().contains("unverified"));
    }
    // ------------------------------------------------------------------
    // Bounded, cancellable setup subprocess (per-trial setup wall clock)
    // ------------------------------------------------------------------

    fn shell_command(script: &str) -> Command {
        let mut command = if cfg!(windows) {
            Command::new("cmd")
        } else {
            Command::new("sh")
        };
        if cfg!(windows) {
            command.arg("/c");
        } else {
            command.arg("-c");
        }
        command.arg(script);
        command
    }

    fn stall_body() -> &'static str {
        if cfg!(windows) {
            "timeout /t 30 /nobreak > nul"
        } else {
            "sleep 30"
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "repoise-runner-setup-test-{}-{}-{tag}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a small executable (shell script / batch) used as a stand-in
    /// for the coordinator's own `repoise` executable.
    fn fake_exec(dir: &Path, body: &str) -> PathBuf {
        let path = if cfg!(windows) {
            dir.join("fake-index.bat")
        } else {
            dir.join("fake-index")
        };
        let mut contents = String::new();
        if cfg!(windows) {
            contents.push_str("@echo off\r\n");
        } else {
            contents.push_str("#!/bin/sh\n");
        }
        contents.push_str(body);
        fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn fake_index_json_body() -> &'static str {
        if cfg!(windows) {
            "echo {\"generation_id\": 7, \"manifest_hash\": \"mh\", \"vectors_total\": 3, \"embedding\": {\"pending\": 0, \"failed\": 0}}"
        } else {
            "echo '{\"generation_id\": 7, \"manifest_hash\": \"mh\", \"vectors_total\": 3, \"embedding\": {\"pending\": 0, \"failed\": 0}}'"
        }
    }

    #[test]
    fn bounded_setup_command_captures_successful_output() {
        let interrupted = AtomicBool::new(false);
        let output = run_bounded_setup_command(
            shell_command("echo '{\"generation_id\": 7}'"),
            Duration::from_secs(10),
            &interrupted,
        )
        .expect("a successful setup child must complete");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("\"generation_id\": 7"), "{stdout}");
    }

    #[test]
    fn bounded_setup_command_drains_a_full_pipe_to_the_capture_cap() {
        let script = if cfg!(windows) {
            "powershell -NoProfile -Command \"[Console]::OpenStandardOutput().Write([byte[]]::new(300000))\""
        } else {
            "head -c 300000 /dev/zero | tr '\\0' 'A'"
        };
        let interrupted = AtomicBool::new(false);
        let started = Instant::now();
        let output =
            run_bounded_setup_command(shell_command(script), Duration::from_secs(20), &interrupted)
                .expect("draining must not deadlock or hit the bound");
        assert!(output.status.success());
        assert_eq!(
            output.stdout.len(),
            MAX_SETUP_PIPE_BYTES,
            "capture must stop at the bound while the pipe keeps draining"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a chatty child must not stall the wait"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_setup_command_times_out_and_terminates_the_descendant_tree() {
        let dir = temp_dir("stall");
        let pid_file = dir.join("heartbeat.pid");
        let marker = dir.join("heartbeat.ndjson");
        let script = format!(
            "echo ready\n( i=0; while [ \"$i\" -lt 1000 ]; do printf 'tick\\n' >> {marker}; sleep 1; i=$((i+1)); done ) & echo $! > {pid}\nsleep 30",
            marker = marker.display(),
            pid = pid_file.display(),
        );
        let interrupted = AtomicBool::new(false);
        let started = Instant::now();
        let failure = run_bounded_setup_command(
            shell_command(&script),
            Duration::from_millis(1500),
            &interrupted,
        )
        .expect_err("a stalled setup child must hit the bound");
        assert!(
            matches!(failure.kind, SetupFailureKind::Timeout),
            "{failure:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the bound must be enforced promptly"
        );
        // The readiness line proves the child started doing real work
        // before it stalled.
        assert!(failure.captured.contains("ready"), "{}", failure.captured);
        // The descendant heartbeat must be dead: the group kill took the
        // whole tree, so the marker stops growing.
        let heartbeat_pid: i32 = fs::read_to_string(&pid_file)
            .expect("heartbeat pid recorded")
            .trim()
            .parse()
            .unwrap();
        let ticks = || fs::read(&marker).map(|bytes| bytes.len()).unwrap_or(0);
        let before = ticks();
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(
            ticks(),
            before,
            "the descendant heartbeat must have stopped"
        );
        // Reaped: `kill -0` must eventually fail.
        let mut reaped = false;
        for _ in 0..50 {
            if let Ok(probe) = Command::new("kill")
                .args(["-0", &heartbeat_pid.to_string()])
                .output()
                && !probe.status.success()
            {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(reaped, "the descendant heartbeat must be reaped");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bounded_setup_command_honours_the_experiment_interrupt() {
        // Pre-set interrupt: the child must never start at all.
        let interrupted = AtomicBool::new(true);
        let started = Instant::now();
        let failure = run_bounded_setup_command(
            shell_command(stall_body()),
            Duration::from_secs(30),
            &interrupted,
        )
        .expect_err("a pre-set interrupt must not start setup work");
        assert!(
            matches!(failure.kind, SetupFailureKind::Interrupted),
            "{failure:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));

        // Interrupt lands while the child is running.
        let interrupted = Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn({
            let interrupted = Arc::clone(&interrupted);
            move || {
                std::thread::sleep(Duration::from_millis(300));
                interrupted.store(true, Ordering::SeqCst);
            }
        });
        let started = Instant::now();
        let failure = run_bounded_setup_command(
            shell_command(stall_body()),
            Duration::from_secs(30),
            &interrupted,
        )
        .expect_err("an interrupt must stop the wait");
        assert!(
            matches!(failure.kind, SetupFailureKind::Interrupted),
            "{failure:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the interrupt must be honoured promptly"
        );
        handle.join().unwrap();
    }

    #[test]
    fn redacted_setup_diagnostics_drop_live_secret_values() {
        let text = "provider error: key sk-live-abc123 refused";
        let values = vec!["sk-live-abc123".to_string()];
        assert_eq!(
            redact_values(text, &values),
            "provider error: key [REDACTED] refused"
        );
        assert_eq!(redact_values(text, &[]), text);
    }
    // ------------------------------------------------------------------
    // Trial-level: bounded setup through `run_trial`
    // ------------------------------------------------------------------

    fn make_git_repo(root: &Path) -> String {
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        fs::write(root.join("README.md"), "hello\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "-q", "-m", "initial"]);
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn test_suite(root: &Path, retrieval_mode: &str, setup_wall_ms: Option<u64>) -> Suite {
        let mut value = serde_json::json!({
            "id": "exp-test",
            "root": root.to_string_lossy().into_owned(),
            "tasks": [{ "id": "task-a", "prompt": "do it" }],
            "conditions": [{
                "id": "cond",
                "retrievalMode": retrieval_mode,
                "searchMode": if retrieval_mode == "repoise" { Some("lexical") } else { None }
            }],
            "adapter": { "executable": "node" },
            "profile": { "harness": { "name": "fake" } }
        });
        if let Some(ms) = setup_wall_ms {
            value["budgets"]["setupWallClockMs"] = Value::from(ms);
        }
        let suite: Suite = serde_json::from_value(value).expect("valid test suite");
        suite.validate().expect("valid test suite");
        suite
    }

    fn dummy_status_view(root: &Path) -> crate::status::StatusView {
        use crate::shared::SharingInfo;
        use crate::status::{
            StatusCache, StatusConfig, StatusFreshness, StatusScope, StatusSnapshot, StatusView,
            WriterStatus,
        };
        StatusView {
            version: "test".into(),
            config: StatusConfig {
                file: None,
                problems: vec![],
            },
            scope: StatusScope {
                adapter: "git".into(),
                root: root.to_path_buf(),
                mode: "committed".into(),
                repo_id: "r".into(),
                worktree_id: "w".into(),
                remote_identity: None,
            },
            snapshot: StatusSnapshot {
                snapshot_id: "s".into(),
                revision: None,
                branch: None,
                mode: "committed".into(),
                manifest_hash: "h".into(),
                dirty_digest: None,
                dirty_count: 0,
            },
            index: None,
            history_lane: None,
            freshness: StatusFreshness {
                status: "fresh".into(),
                reasons: vec![],
                snapshot_id: "s".into(),
                branch: None,
                dirty_count: 0,
            },
            cache: StatusCache {
                root: root.to_path_buf(),
                db_path: root.join("db"),
                db_bytes: 0,
                wal_bytes: 0,
            },
            owner: None,
            writer: WriterStatus {
                lock_held: false,
                state: "free".into(),
            },
            sharing: SharingInfo {
                state: "unavailable".into(),
                family_id: None,
                store: None,
                note: None,
            },
            last_error: None,
        }
    }

    fn test_run_context(
        tmp: &Path,
        repo: &Path,
        revision: &str,
        self_exe: &Path,
        suite: Suite,
    ) -> RunContext {
        use crate::bench::preflight::{BridgeSource, Guidance};
        RunContext {
            self_exe: self_exe.to_path_buf(),
            layout: Layout::create(&tmp.join("out"), "exp-test").unwrap(),
            preflight: Preflight {
                root: repo.to_path_buf(),
                remote_identity: None,
                snapshot_kind: "commit".into(),
                revision_sha: revision.to_string(),
                patch_file: None,
                patch_hash: None,
                node: tmp.join("node"),
                node_version: "test".into(),
                bridge: tmp.join("bridge"),
                bridge_source: BridgeSource::Override,
                runner: tmp.join("adapter"),
                adapter_env_names: vec![],
                credential_names: vec![],
                embedding_endpoint_env: None,
                embedding_api_key_env: None,
                status: dummy_status_view(repo),
                config_hash: None,
                guidance: Guidance {
                    overlay_present: false,
                    template_version: None,
                    agents_md_hash: None,
                },
                warnings: vec![],
            },
            suite,
            keep_workspaces: false,
            adapter_env: vec![],
            json: true,
        }
    }

    fn test_trial() -> Trial {
        Trial {
            trial_id: "t-task-a-r0-cond".into(),
            task_index: 0,
            task_id: "task-a".into(),
            condition_index: 0,
            condition_id: "cond".into(),
            repetition: 0,
            ordered_index: 0,
        }
    }

    #[test]
    fn build_trial_index_parses_successful_output() {
        let tmp = temp_dir("index-ok");
        let self_exe = fake_exec(&tmp, fake_index_json_body());
        let ctx = test_run_context(
            &tmp,
            &tmp,
            "deadbeef",
            &self_exe,
            test_suite(&tmp, "repoise", None),
        );
        let index = build_trial_index(&ctx, &tmp, 30_000, &AtomicBool::new(false))
            .expect("a successful index output must parse");
        assert_eq!(index.generation_id, 7);
        assert_eq!(index.manifest_hash, "mh");
        assert_eq!(index.vectors_total, 3);
        assert_eq!(index.vectors_pending, 0);
        assert_eq!(index.vectors_failed, 0);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_trial_index_timeout_is_bounded_and_reasoned() {
        let tmp = temp_dir("index-stall");
        let self_exe = fake_exec(&tmp, stall_body());
        let ctx = test_run_context(
            &tmp,
            &tmp,
            "deadbeef",
            &self_exe,
            test_suite(&tmp, "repoise", Some(800)),
        );
        let started = Instant::now();
        let err = build_trial_index(&ctx, &tmp, 800, &AtomicBool::new(false))
            .expect_err("a stalled index child must hit the bound");
        assert_eq!(err.reason, "setup_timeout");
        assert!(err.detail.contains("800 ms setup bound"), "{}", err.detail);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the setup bound must be enforced promptly"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_trial_index_failure_keeps_the_exit_diagnostic() {
        let tmp = temp_dir("index-fail");
        let body = if cfg!(windows) {
            "echo oops 1>&2\r\nexit 3\r\n"
        } else {
            "echo oops >&2\nexit 3\n"
        };
        let self_exe = fake_exec(&tmp, body);
        let ctx = test_run_context(
            &tmp,
            &tmp,
            "deadbeef",
            &self_exe,
            test_suite(&tmp, "repoise", None),
        );
        let err = build_trial_index(&ctx, &tmp, 30_000, &AtomicBool::new(false))
            .expect_err("a failed index child must error");
        assert_eq!(err.reason, "index_build_failed");
        assert!(err.detail.contains("index exited 3"), "{}", err.detail);
        assert!(err.detail.contains("oops"), "{}", err.detail);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_trial_setup_timeout_is_bounded_recorded_and_cleaned_up() {
        let tmp = temp_dir("trial-stall");
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let revision = make_git_repo(&repo);
        let self_exe = fake_exec(&tmp, stall_body());
        let ctx = test_run_context(
            &tmp,
            &repo,
            &revision,
            &self_exe,
            test_suite(&repo, "repoise", Some(800)),
        );
        let trial = test_trial();
        let started = Instant::now();
        let outcome = ctx.run_trial(&trial, 0, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.status, "infra_error");
        assert_eq!(outcome.reason.as_deref(), Some("setup_timeout"));
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "a stalled setup must not hold the trial"
        );

        let record: Value = serde_json::from_str(
            &fs::read_to_string(ctx.layout.trial_dir(&trial.trial_id).join("trial.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(record["status"], "infra_error");
        assert_eq!(record["reason"], "setup_timeout");
        assert_eq!(record["timings"]["agentMs"], Value::Null);
        assert!(record["timings"]["setupMs"].as_u64().unwrap() >= 800);
        assert_eq!(record["setup"]["indexMs"], Value::Null);
        assert_eq!(record["completeness"], "partial");
        // The grading record is censored, and the workspace is cleaned up.
        let grading: Value = serde_json::from_str(
            &fs::read_to_string(ctx.layout.trial_dir(&trial.trial_id).join("grading.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(grading["status"], "not_graded");
        assert!(!ctx.layout.workspace_path(&trial.trial_id).exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_trial_interrupt_cancels_setup_and_never_starts_the_agent() {
        let tmp = temp_dir("trial-interrupt");
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let revision = make_git_repo(&repo);
        let self_exe = fake_exec(&tmp, fake_index_json_body());
        let ctx = test_run_context(
            &tmp,
            &repo,
            &revision,
            &self_exe,
            test_suite(&repo, "repoise", None),
        );
        let trial = test_trial();
        let interrupted = AtomicBool::new(true);
        let outcome = ctx.run_trial(&trial, 0, &interrupted).unwrap();
        assert_eq!(outcome.status, "infra_error");
        assert_eq!(outcome.reason.as_deref(), Some("setup_cancelled"));

        let record: Value = serde_json::from_str(
            &fs::read_to_string(ctx.layout.trial_dir(&trial.trial_id).join("trial.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(record["reason"], "setup_cancelled");
        assert_eq!(record["timings"]["agentMs"], Value::Null);
        // The host bridge was never started, and the workspace is removed.
        assert!(!ctx.layout.adapter_dir(&trial.trial_id).exists());
        assert!(!ctx.layout.workspace_path(&trial.trial_id).exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_trial_interrupt_between_setup_and_agent_starts_no_agent() {
        // Baseline condition: setup does no index work, so an interrupt that
        // is set after setup (here: from the start) must be caught at the
        // pre-agent re-check — recorded, never forwarded to the bridge.
        let tmp = temp_dir("trial-interrupt-baseline");
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let revision = make_git_repo(&repo);
        let self_exe = fake_exec(&tmp, "exit 0\n");
        let ctx = test_run_context(
            &tmp,
            &repo,
            &revision,
            &self_exe,
            test_suite(&repo, "baseline", None),
        );
        let trial = test_trial();
        let interrupted = AtomicBool::new(true);
        let outcome = ctx.run_trial(&trial, 0, &interrupted).unwrap();
        assert_eq!(outcome.status, "infra_error");
        assert_eq!(outcome.reason.as_deref(), Some("interrupted_before_agent"));
        assert!(!ctx.layout.adapter_dir(&trial.trial_id).exists());
        let _ = fs::remove_dir_all(&tmp);
    }
}
