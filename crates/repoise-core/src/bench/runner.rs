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
//! failures) and the scheduler adds `not_started` (budget cap,
//! interruption, drift). Every attempt is recorded; nothing is silently
//! retried.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
            match build_trial_index(self, &workspace.path) {
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
                Err(message) => {
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
                        "index_build_failed",
                        &message,
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
            "mode": mode_record(condition, &host.result),
            "timings": {
                "queueMs": queue_ms,
                "setupMs": setup_ms,
                "agentMs": agent_ms,
                "totalMs": total_ms
            },
            "status": host.status,
            "reason": host.reason,
            "usage": host.result.get("usage").cloned(),
            "adapterDurationMs": host.result.get("adapterDurationMs").cloned(),
            "warnings": host.result.get("warnings").cloned().unwrap_or(Value::Array(vec![])),
            "errors": host.result.get("errors").cloned().unwrap_or(Value::Array(vec![])),
            "artifacts": {
                "requestFile": "request.json",
                "eventsFile": "adapter/events.ndjson",
                "resultFile": "adapter/result.json",
                "stderrLog": "adapter/stderr.log",
                "hostLog": "host.log",
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

/// Builds the exact argv for one trial's read-only MCP server. The shape
/// (`mcp [ROOT] --search-mode <MODE> [--committed]`) is accepted by the real
/// CLI parser; the CLI integration test starts a server with this argv.
pub fn mcp_argv(exe: &Path, root: &Path, search_mode: &str, committed: bool) -> Vec<String> {
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
            let argv = mcp_argv(
                &ctx.self_exe,
                &workspace.path,
                mode,
                ctx.preflight.snapshot_kind == "commit",
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

fn mode_record(condition: &Condition, host_result: &Value) -> Value {
    let requested = if condition.retrieval_mode == "repoise" {
        condition.search_mode.clone()
    } else {
        None
    };
    let tool_telemetry = host_result
        .pointer("/hello/capabilities/tool_telemetry")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if requested.is_none() {
        return serde_json::json!({
            "requested": null_value(),
            "effective": null_value(),
            "note": "baseline: no Repoise retrieval"
        });
    }
    if !tool_telemetry {
        return serde_json::json!({
            "requested": requested,
            "effective": null_value(),
            "note": "unverified: the adapter declares no tool telemetry; the pinned MCP server mode is the enforcement boundary, runtime degradation is not observable"
        });
    }
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
    serde_json::json!({
        "requested": requested,
        "effective": if used { "observed" } else { "not_observed" },
        "note": "tool-call presence from the adapter tool telemetry event stream; the executed search mode is enforced by the pinned MCP server (a vectorless trial index fails setup)"
    })
}

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

/// Builds the trial worktree index through the same `repoise index`
/// operation (setup time, outside the timed agent interval; the experiment
/// cache directory is declared in the manifest).
fn build_trial_index(ctx: &RunContext, workspace: &Path) -> Result<IndexInfo, String> {
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
    let output = command.output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "index exited {}: {stderr}",
            output.status.code().unwrap_or(-1)
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
        .map_err(|err| err.to_string())?;
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
        let argv = mcp_argv(Path::new("repoise"), Path::new("wt"), "hybrid", false);
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
    }
}
