//! Preflight checks before any trial is launched.
//!
//! Preflight freezes the experiment inputs and refuses to approximate:
//! runner/node/bridge must exist, the snapshot must be reproducible (clean
//! commit or explicitly recorded patch), hybrid conditions require verified
//! vector coverage on the root index, suite/prompt files must live outside
//! the repository (a checkout would otherwise expose them to the agent),
//! and credential references must resolve (names only ever reach artifacts).
//! Preflight failures are exit 3 — distinct from operational and usage
//! failures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::adapter::git::GitAdapter;
use crate::adapter::{SnapshotMode, SourceAdapter};
use crate::cache::CachePaths;
use crate::config::{CliOverrides, EffectiveConfig};
use crate::error::Error;
use crate::status::{self, StatusView};
use crate::store::Store;
use crate::{CACHE_ENV_VAR, CONFIG_FILENAME};

use super::artifacts::{is_within, write_json};
use super::suite::Suite;

/// Environment variable that pins the bench host bridge (AP-03 companion).
pub const BRIDGE_ENV_VAR: &str = "REPOISE_BENCH_HOST_BRIDGE";
/// Relative location of the checked-in bridge in a source checkout.
pub const BRIDGE_RELATIVE_PATH: &str = "packages/agent-adapter-protocol/bin/adapter-host-run.mjs";

/// Guidance state of the root repository.
#[derive(Clone, Debug, Serialize)]
pub struct Guidance {
    /// Whether a managed overlay is present at the root.
    pub overlay_present: bool,
    /// Overlay template version when present.
    pub template_version: Option<String>,
    /// SHA-256 of `AGENTS.md` when present.
    pub agents_md_hash: Option<String>,
}

/// The frozen preflight outcome.
#[derive(Clone, Debug)]
pub struct Preflight {
    pub root: PathBuf,
    pub remote_identity: Option<String>,
    pub snapshot_kind: String,
    pub revision_sha: String,
    pub patch_file: Option<PathBuf>,
    pub patch_hash: Option<String>,
    pub node: PathBuf,
    pub node_version: String,
    pub bridge: PathBuf,
    pub runner: PathBuf,
    /// Adapter environment variable names forwarded to the adapter (values
    /// never recorded; `env:` references are resolved here).
    pub adapter_env_names: Vec<String>,
    /// Resolved credential variable names (names only, never values).
    pub credential_names: Vec<String>,
    pub status: StatusView,
    /// SHA-256 of the root `repoise.config.json` when present.
    pub config_hash: Option<String>,
    pub guidance: Guidance,
    pub warnings: Vec<String>,
}

/// Runs all preflight checks for one suite.
pub fn run(suite: &Suite, suite_path: &Path) -> Result<Preflight, Error> {
    let root_raw = suite.root.trim();
    let root = super::suite::resolve_path(root_raw)?;
    let root = std::fs::canonicalize(&root)
        .map_err(|err| Error::Config(format!("root {} does not exist: {err}", root.display())))?;
    let adapter = GitAdapter::new(&root).map_err(Error::Adapter)?;

    // The resolved config/cache decide which tree entries are Repoise's own
    // (the cache directory), which the snapshot dirtiness check excludes.
    let effective = EffectiveConfig::resolve(
        &root,
        &CliOverrides {
            preset: None,
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: None,
        },
    )
    .map_err(|err| Error::Config(format!("root config does not resolve: {err}")))?;
    let cache = CachePaths::resolve(
        &root,
        &effective.cache_dir,
        std::env::var(CACHE_ENV_VAR).ok().as_deref(),
    )?;

    // Snapshot: resolve the revision; reject dirty inputs unless a recorded
    // patch snapshot is explicitly selected.
    let revision_sha = adapter
        .run_git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{}^{{commit}}", suite.snapshot.revision),
        ])
        .map(|value| value.trim().to_string())
        .map_err(|err| Error::Config(format!("cannot resolve snapshot revision: {err}")))?;
    if revision_sha.is_empty() {
        return Err(Error::Config(format!(
            "cannot resolve snapshot revision '{}'",
            suite.snapshot.revision
        )));
    }
    let (patch_file, patch_hash) = match suite.snapshot.kind.as_str() {
        "commit" => {
            // The cache directory (e.g. the default `.repoise`) is Repoise's
            // own state, not source input: it never makes the tree dirty.
            let porcelain = adapter
                .run_git(&["status", "--porcelain"])
                .map_err(Error::Adapter)?;
            let dirty = porcelain
                .lines()
                .filter(|line| {
                    if line.trim().is_empty() {
                        return false;
                    }
                    // Porcelain format: two status chars, a space, then the path
                    // (renames carry ` -> ` inside the path segment).
                    let path = line
                        .get(3..)
                        .unwrap_or("")
                        .rsplit(" -> ")
                        .next()
                        .unwrap_or("");
                    let entry = Path::new(path);
                    let absolute = if entry.is_absolute() {
                        entry.to_path_buf()
                    } else {
                        root.join(entry)
                    };
                    !absolute.starts_with(&cache.root)
                })
                .count();
            if dirty > 0 {
                return Err(Error::Config(format!(
                    "working tree is dirty ({dirty} entries); commit the changes or select a \
                     recorded patch snapshot (snapshot.kind = \"patch\")"
                )));
            }
            (None, None)
        }
        "patch" => {
            let patch =
                suite.snapshot.patch_file.clone().ok_or_else(|| {
                    Error::Config("snapshot.kind 'patch' requires patchFile".into())
                })?;
            let patch = super::suite::resolve_path(&patch.to_string_lossy())?;
            let patch = std::fs::canonicalize(&patch)
                .map_err(|err| Error::Config(format!("patch file does not exist: {err}")))?;
            let bytes = std::fs::read(&patch).map_err(Error::Io)?;
            (Some(patch.clone()), Some(crate::hash::sha256_hex(bytes)))
        }
        other => {
            return Err(Error::Config(format!(
                "snapshot.kind must be 'commit' or 'patch', got '{other}'"
            )));
        }
    };

    // Execution prerequisites: Node, the checked-in host bridge, the runner.
    let node = find_executable("node").ok_or_else(|| {
        Error::Config("node executable not found on PATH (required to drive agent adapters)".into())
    })?;
    let node_version = Command::new(&node)
        .arg("-v")
        .output()
        .map_err(Error::Io)?
        .stdout;
    let node_version = String::from_utf8_lossy(&node_version).trim().to_string();

    let bridge = find_bridge()?;
    let runner = find_executable(&suite.adapter.executable).ok_or_else(|| {
        Error::Config(format!(
            "adapter executable '{}' not found (absolute path or bare name on PATH)",
            suite.adapter.executable
        ))
    })?;

    // Suite and prompt files must stay outside the repository: a trial
    // checkout would otherwise expose them to the agent.
    let suite_canonical = std::fs::canonicalize(suite_path).map_err(Error::Io)?;
    if is_within(&root, &suite_canonical) {
        return Err(Error::Config(format!(
            "suite file {} is inside the benchmarked repository root {}; gold \
             answers and evaluators must live outside the searchable workspace",
            suite_canonical.display(),
            root.display()
        )));
    }
    for task in &suite.tasks {
        if let Some(file) = &task.prompt_file {
            let canonical =
                std::fs::canonicalize(super::suite::resolve_path(&file.to_string_lossy())?)
                    .map_err(|err| {
                        Error::Config(format!("task '{}' promptFile missing: {err}", task.id))
                    })?;
            if is_within(&root, &canonical) {
                return Err(Error::Config(format!(
                    "task '{}' promptFile {} is inside the benchmarked repository root {}",
                    task.id,
                    canonical.display(),
                    root.display()
                )));
            }
        }
    }

    // Credential references resolve now; only names reach artifacts.
    let mut adapter_env_names: Vec<String> = Vec::new();
    for (name, value) in &suite.adapter.env {
        if let Some(ref_name) = value.strip_prefix("env:")
            && std::env::var(ref_name).is_err()
        {
            return Err(Error::Config(format!(
                "adapter env '{name}' references env:{ref_name}, which is not set"
            )));
        }
        adapter_env_names.push(name.clone());
    }
    let mut credential_names: Vec<String> = Vec::new();
    let credential_ref = suite.profile.provider.as_ref().and_then(|provider| {
        provider
            .credential_ref
            .as_deref()
            .and_then(|value| value.strip_prefix("env:"))
    });
    if let Some(ref_name) = credential_ref
        && std::env::var(ref_name).is_err()
    {
        return Err(Error::Config(format!(
            "provider credentialRef env:{ref_name} is not set"
        )));
    }
    if let Some(ref_name) = credential_ref {
        credential_names.push(ref_name.to_string());
    }

    // Root index status: coverage gates hybrid conditions; an active writer
    // (watch/index) is recorded as contention, never stopped.
    let (repo_id, worktree_id, _) =
        crate::search::scope_for_search(&adapter, SnapshotMode::WorkingTree)
            .map_err(|err| Error::Config(format!("cannot scope the root repository: {err}")))?;
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    let config_file = root.join(CONFIG_FILENAME);
    let config_ref = if config_file.exists() {
        Some(config_file.as_path())
    } else {
        None
    };
    let status = status::status(
        &adapter,
        SnapshotMode::WorkingTree,
        &effective,
        config_ref,
        &store,
        &cache,
    )
    .map_err(|err| Error::Config(format!("root status failed: {err}")))?;
    for condition in &suite.conditions {
        if condition.retrieval_mode == "repoise"
            && condition.search_mode.as_deref() == Some("hybrid")
        {
            let index = status.index.as_ref();
            let covered = index.is_some_and(|index| {
                index.vectors > 0 && index.vectors_pending == 0 && index.vector_profile.is_some()
            });
            if !covered {
                return Err(Error::Config(format!(
                    "condition '{}' requires hybrid mode with verified vector coverage; the root \
                     index has no complete embedding profile (a degraded lexical fallback would be \
                     counted as hybrid). Index the root with an embedding provider, or remove the \
                     condition.",
                    condition.id
                )));
            }
        }
    }
    let mut warnings: Vec<String> = Vec::new();
    if status.owner.is_some() {
        warnings.push(
            "an index/watch writer marker is present for the root scope; the run will not stop \
             it, but watch/provider contention is recorded"
                .to_string(),
        );
    }

    // Root guidance state (baseline trials remove only the managed overlay).
    let overlay = crate::overlay::load_manifest(&root);
    let guidance = Guidance {
        overlay_present: overlay.is_ok(),
        template_version: overlay
            .as_ref()
            .ok()
            .map(|manifest| manifest.template_version.clone()),
        agents_md_hash: hash_optional_file(&root.join("AGENTS.md")),
    };
    if guidance.overlay_present {
        warnings.push(
            "the root repository carries a managed Repoise overlay; baseline trials remove it \
             from their disposable worktree only (the user's checkout is untouched)"
                .into(),
        );
    }

    Ok(Preflight {
        root: root.clone(),
        remote_identity: adapter.remote_identity().map_err(Error::Adapter)?,
        snapshot_kind: suite.snapshot.kind.clone(),
        revision_sha,
        patch_file,
        patch_hash,
        node,
        node_version,
        bridge,
        runner,
        adapter_env_names,
        credential_names,
        status,
        config_hash: hash_optional_file(&config_file),
        guidance,
        warnings,
    })
}

/// Finds an executable: absolute path first, then `PATH` (with the Windows
/// executable extensions).
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_absolute() {
        return direct.is_file().then(|| direct.clone());
    }
    let path_var = std::env::var_os("PATH")?;
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for extension in extensions {
            let candidate = std::path::Path::new(&dir).join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolves the AP-03 host bridge: the explicit env override wins, then a
/// source checkout walked up from the current directory.
pub fn find_bridge() -> Result<PathBuf, Error> {
    if let Some(raw) = std::env::var(BRIDGE_ENV_VAR)
        .ok()
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(raw.as_str());
        if path.is_file() {
            return Ok(path);
        }
        return Err(Error::Config(format!(
            "{BRIDGE_ENV_VAR} is set but {raw} is not a file"
        )));
    }
    let mut directory = std::env::current_dir().map_err(Error::Io)?;
    loop {
        let candidate = directory.join(BRIDGE_RELATIVE_PATH);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !directory.pop() {
            break;
        }
    }
    Err(Error::Config(format!(
        "agent adapter protocol host bridge not found: set {BRIDGE_ENV_VAR} to the \
         adapter-host-run.mjs path, or run from a source checkout containing {BRIDGE_RELATIVE_PATH}"
    )))
}

fn hash_optional_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(crate::hash::sha256_hex(bytes))
}

/// The dry-run document: inspectable trials, prerequisites, expected scope,
/// budgets and an estimated maximum work, with assumptions identified.
pub fn dry_run(
    suite: &Suite,
    suite_hash: &str,
    preflight: &Preflight,
    trials: &[super::plan::Trial],
) -> serde_json::Value {
    let per_trial_wall = suite.limits.wall_clock_ms;
    let estimated = trials
        .len()
        .saturating_mul(per_trial_wall.unwrap_or(0) as usize);
    serde_json::json!({
        "schemaVersion": 1,
        "kind": "dry-run",
        "experimentId": suite.id,
        "suiteHash": suite_hash,
        "root": preflight.root.to_string_lossy().into_owned(),
        "snapshot": {
            "kind": preflight.snapshot_kind,
            "revision": format!("git:{}", preflight.revision_sha),
            "patchHash": preflight.patch_hash,
            "dirty": false
        },
        "prerequisites": {
            "runner": { "executable": preflight.runner.to_string_lossy().into_owned() },
            "node": { "executable": preflight.node.to_string_lossy().into_owned(), "version": preflight.node_version },
            "bridge": preflight.bridge.to_string_lossy().into_owned(),
            "gitRepository": true,
            "index": {
                "present": preflight.status.index.is_some(),
                "vectors": preflight.status.index.as_ref().map(|i| i.vectors).unwrap_or(0),
                "vectorsPending": preflight.status.index.as_ref().map(|i| i.vectors_pending).unwrap_or(0),
                "vectorProfile": preflight.status.index.as_ref().and_then(|i| i.vector_profile.clone())
            },
            "guidance": &preflight.guidance,
            "warnings": &preflight.warnings
        },
        "conditions": suite
            .conditions
            .iter()
            .map(|condition| serde_json::json!({
                "id": condition.id,
                "retrievalMode": condition.retrieval_mode,
                "searchMode": condition.search_mode,
                "requiredCapabilities": condition.required_capabilities
            }))
            .collect::<Vec<serde_json::Value>>(),
        "trials": trials
            .iter()
            .map(|trial| serde_json::json!({
                "trialId": trial.trial_id,
                "taskId": trial.task_id,
                "conditionId": trial.condition_id,
                "repetition": trial.repetition,
                "orderedIndex": trial.ordered_index
            }))
            .collect::<Vec<serde_json::Value>>(),
        "budgets": {
            "perTrial": &suite.limits,
            "experimentWallClockMs": suite.budgets.experiment_wall_clock_ms
        },
        "estimatedMaxAgentWallMs": estimated,
        "assumptions": [
            "estimatedMaxAgentWallMs = trials * per-trial wallClockMs; it is an upper bound on timed agent work and excludes setup (worktree/index), setup is not estimated",
            "per-trial wallClockMs defaults are applied by the adapter host when the suite omits limits",
            "no completion time is promised; actual time depends on the model, provider and hardware"
        ]
    })
}

/// Everything frozen before the immutable experiment manifest is written.
pub struct ManifestInput<'a> {
    pub suite: &'a Suite,
    pub suite_path: &'a Path,
    pub suite_hash: &'a str,
    pub preflight: &'a Preflight,
    pub plan: &'a super::plan::TrialPlan,
    pub trials: &'a [super::plan::Trial],
    pub overrides: &'a BTreeMap<String, serde_json::Value>,
}

/// Writes the immutable frozen experiment manifest and returns its value.
pub fn write_manifest(
    experiment_dir: &Path,
    input: &ManifestInput<'_>,
) -> crate::Result<serde_json::Value> {
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "experimentId": input.suite.id,
        "suiteFile": input.suite_path.to_string_lossy().into_owned(),
        "suiteHash": input.suite_hash,
        "overrides": input.overrides,
        "startedAtUtc": crate::bench::runner::now_utc(),
        "root": {
            "path": input.preflight.root.to_string_lossy().into_owned(),
            "remoteIdentity": input.preflight.remote_identity,
            "configHash": input.preflight.config_hash
        },
        "snapshot": {
            "kind": input.preflight.snapshot_kind,
            "revision": format!("git:{}", input.preflight.revision_sha),
            "patchFile": input.preflight.patch_file.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "patchHash": input.preflight.patch_hash,
            "dirty": false,
            "materialization": "tracked content checked out by git; symlinks preserved; submodules not initialized; LFS pointers not fetched"
        },
        "conditions": input.suite
            .conditions
            .iter()
            .map(|condition| serde_json::json!({
                "id": condition.id,
                "retrievalMode": condition.retrieval_mode,
                "searchMode": condition.search_mode,
                "requiredCapabilities": condition.required_capabilities
            }))
            .collect::<Vec<serde_json::Value>>(),
        "guidance": {
            "templateVersion": input.preflight.guidance.template_version,
            "rootOverlayPresent": input.preflight.guidance.overlay_present,
            "rootAgentsMdHash": input.preflight.guidance.agents_md_hash
        },
        "repoise": {
            "version": env!("CARGO_PKG_VERSION"),
            "embedding": {
                "profile": input.preflight.status.index.as_ref().and_then(|i| i.vector_profile.clone()),
                "cachePolicy": "content-addressed immutable cache shared across trials under the experiment cache directory (declared)"
            },
            "sharing": serde_json::to_value(&input.preflight.status.sharing).unwrap_or_default()
        },
        "adapter": {
            "executable": input.preflight.runner.to_string_lossy().into_owned(),
            "args": input.suite.adapter.args,
            "envNames": &input.preflight.adapter_env_names,
            "credentialEnvNames": &input.preflight.credential_names,
            "bridge": input.preflight.bridge.to_string_lossy().into_owned(),
            "node": input.preflight.node_version
        },
        "profile": {
            "harness": input.suite.profile.harness,
            "provider": input.suite.profile.provider,
            "model": input.suite.profile.model,
            "reasoning": input.suite.profile.reasoning,
            "compaction": input.suite.profile.compaction,
            "sampling": input.suite.profile.sampling
        },
        "limits": &input.suite.limits,
        "budgets": &input.suite.budgets,
        "scope": input.suite.scope,
        "seed": input.suite.seed,
        "plan": input.plan
            .blocks
            .iter()
            .map(|block| serde_json::json!({
                "task": block.task_id,
                "repetition": block.repetition,
                "order": block
                    .order
                    .iter()
                    .map(|index| input.suite.conditions[*index].id.clone())
                    .collect::<Vec<_>>()
            }))
            .collect::<Vec<serde_json::Value>>(),
        "trials": input.trials
            .iter()
            .map(|trial| serde_json::json!({
                "trialId": trial.trial_id,
                "taskId": trial.task_id,
                "conditionId": trial.condition_id,
                "repetition": trial.repetition,
                "orderedIndex": trial.ordered_index
            }))
            .collect::<Vec<serde_json::Value>>(),
        "unknowns": unknown_settings(input.suite),
        "isolation": {
            "mechanism": "one disposable linked git worktree per trial, checked out detached at the frozen snapshot",
            "securitySandbox": false,
            "notes": [
                "separate worktrees are not security sandboxes; hidden-test integrity for coding tasks is a declared limitation until a host-enforced isolation mechanism is defined (AP-05)",
                "trial artifacts, gold answers and evaluators live outside every trial workspace root",
                "user-managed watchers/model servers are never stopped"
            ]
        }
    });
    write_json(&experiment_dir.join("manifest.json"), &manifest)?;
    Ok(manifest)
}

/// Settings that are unknown by construction, recorded explicitly.
fn unknown_settings(suite: &Suite) -> Vec<String> {
    let mut unknowns: Vec<String> = Vec::new();
    if suite
        .profile
        .model
        .as_ref()
        .is_some_and(|model| model.revision.is_none())
    {
        unknowns.push(
            "model weights revision (a model id alone does not pin served weights)".to_string(),
        );
    }
    unknowns.push("hardware model and driver (not probed)".to_string());
    unknowns.push("provider prompt-cache state (cold versus warm is not measured)".to_string());
    unknowns
}
