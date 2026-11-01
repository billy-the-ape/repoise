//! Preflight checks before any trial is launched.
//!
//! Preflight freezes the experiment inputs and refuses to approximate:
//! runner/node/bridge must exist, the snapshot must be reproducible (clean
//! commit or explicitly recorded patch), hybrid conditions require verified
//! vector coverage on the root index, suite/prompt files must live outside
//! the repository (a checkout would otherwise expose them to the agent),
//! and credential references must resolve (names only ever reach artifacts).
//! The output root (artifacts and trial worktrees) is Repoise's own state:
//! like the cache directory it is excluded from the snapshot dirtiness
//! check, and an output root equal to the repository root is rejected.
//! Preflight failures are exit 3 — distinct from operational and usage
//! failures.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
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

/// The root hybrid gate: the root index must report complete scope-aware
/// vector coverage — a stored embedding profile and a vector for every
/// eligible chunk under that stored scope. Chunks outside the embedding
/// scope (config and other lexical-only corpora) never block hybrid, and a
/// lexical-only, profile-missing, empty-corpus or partially embedded
/// index never qualifies.
pub fn hybrid_gate_met(index: &crate::status::StatusIndex) -> bool {
    index.embedding_coverage == "complete"
}

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
    /// Embedding endpoint variable name from the root config (the
    /// `embedding.endpoint` `env:` reference without its prefix). The value
    /// is forwarded to each trial's MCP server env at request time.
    pub embedding_endpoint_env: Option<String>,
    /// Embedding API-key variable name from the root config
    /// (`embedding.apiKeyEnv`), forwarded like the endpoint variable.
    pub embedding_api_key_env: Option<String>,
    pub status: StatusView,
    /// SHA-256 of the root `repoise.config.json` when present.
    pub config_hash: Option<String>,
    pub guidance: Guidance,
    pub warnings: Vec<String>,
}

/// Runs all preflight checks for one suite.
pub fn run(suite: &Suite, suite_path: &Path, output_root: &Path) -> Result<Preflight, Error> {
    let root_raw = suite.root.trim();
    let root = super::suite::resolve_path(root_raw)?;
    let root = std::fs::canonicalize(&root)
        .map_err(|err| Error::Config(format!("root {} does not exist: {err}", root.display())))?;
    let adapter = GitAdapter::new(&root).map_err(Error::Adapter)?;

    // The output root holds artifacts and trial worktrees (Repose's own
    // state, like the cache directory) and is excluded from the snapshot
    // dirtiness check, so an in-repo output directory (the default when
    // running from the repository root) never fails a later invocation.
    // An output root equal to the repository root would exempt everything,
    // so it is rejected outright.
    let output_root = normalize_abs_path(output_root);
    if output_root == root {
        return Err(Error::Config(format!(
            "the output directory {} is the benchmarked repository root itself; \
             choose a different directory (e.g. the default `repoise-bench` \
             subdirectory or a path outside the repository)",
            output_root.display()
        )));
    }
    let output_within_root = is_within(&root, &output_root);

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
    // Embedding credential variable names declared in the root config: the
    // trial's MCP server must see them in its own process env to serve the
    // same embedding session the coordinator's index child built (names only
    // are recorded; values are forwarded at request time).
    let embedding_settings = effective.embedding.as_ref();
    let embedding_endpoint_env = embedding_settings
        .and_then(|settings| settings.endpoint.as_deref())
        .and_then(|ref_name| ref_name.strip_prefix("env:"))
        .map(str::to_string);
    let embedding_api_key_env =
        embedding_settings.and_then(|settings| settings.api_key_env.clone());

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
            // `--untracked-files=all` lists each untracked file individually:
            // the default `normal` mode collapses an untracked directory into
            // one ancestor entry (e.g. `?? .bench/`), and that ancestor is not
            // a descendant of the nested output root excluded below, so an
            // in-repo output root under an untracked parent would make every
            // run after the first fail as dirty.
            let porcelain = adapter
                .run_git(&[
                    "status",
                    "--porcelain",
                    // NUL-separated: without -z git C-quotes paths containing
                    // spaces, tabs, quotes, backslashes or (default
                    // `core.quotePath`) non-ASCII bytes, and the raw text no
                    // longer matches the output/cache roots below, so the
                    // runner's own artifacts would count as dirt on every
                    // run after the first.
                    "-z",
                    "--untracked-files=all",
                ])
                .map_err(Error::Adapter)?;
            let dirty = count_dirty_entries(
                &porcelain,
                &root,
                &cache.root,
                output_within_root.then_some(&output_root),
            );
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
            // The patch is experiment input, like the suite and prompt
            // files: a trial checkout would otherwise expose it to the
            // agent (the schema requires it to live outside the repo).
            if is_within(&root, &patch) {
                return Err(Error::Config(format!(
                    "snapshot patchFile {} is inside the benchmarked repository root {}; \
                     experiment patch input must live outside the searchable workspace",
                    patch.display(),
                    root.display()
                )));
            }
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

    let bridge = find_bridge(&root)?;
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
            let covered = index.is_some_and(hybrid_gate_met);
            if !covered {
                return Err(Error::Config(format!(
                    "condition '{}' requires hybrid mode with verified vector coverage over the \
                     eligible corpus (the stored embedding scope); the root index does not \
                     have a complete embedding profile (a degraded lexical fallback would be \
                     counted as hybrid). Index the root with an embedding provider, or remove \
                     the condition.",
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
        embedding_endpoint_env,
        embedding_api_key_env,
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

/// Resolves the AP-03 host bridge: the explicit env override wins, then the
/// source checkout the running executable was built from (walked up from
/// `current_exe()`). The user's current directory is never consulted, and a
/// resolved bridge inside the benchmarked repository is rejected:
/// repository content is untrusted data, so a repo that ships its own
/// `adapter-host-run.mjs` must never be executed automatically with adapter
/// credentials.
pub fn find_bridge(root: &Path) -> Result<PathBuf, Error> {
    find_bridge_with(root, &|name| std::env::var(name).ok())
}

/// `find_bridge` with an injectable environment lookup (tests).
fn find_bridge_with(
    root: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, Error> {
    if let Some(raw) = lookup(BRIDGE_ENV_VAR).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(raw.as_str());
        if path.is_file() {
            // An explicit override is an operator choice made outside the
            // benchmarked repository's control.
            return Ok(path);
        }
        return Err(Error::Config(format!(
            "{BRIDGE_ENV_VAR} is set but {raw} is not a file"
        )));
    }
    let exe = std::env::current_exe().map_err(Error::Io)?;
    let start = exe
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_default();
    match bridge_candidates_from(&start) {
        Some(candidate) if is_within(root, &candidate) => Err(Error::Config(format!(
            "the host bridge {} resolved next to the running executable is inside \
             the benchmarked repository {}; repository content is untrusted data \
             and will not be executed automatically — set {BRIDGE_ENV_VAR} to the \
             bridge file explicitly",
            candidate.display(),
            root.display()
        ))),
        Some(candidate) => Ok(candidate),
        None => Err(Error::Config(format!(
            "agent adapter protocol host bridge not found next to the running \
             executable ({}); set {BRIDGE_ENV_VAR} to the adapter-host-run.mjs path",
            BRIDGE_RELATIVE_PATH
        ))),
    }
}

/// Walks up from `start` returning the first checked-in host bridge.
fn bridge_candidates_from(start: &Path) -> Option<PathBuf> {
    let mut directory = start.to_path_buf();
    loop {
        let candidate = directory.join(BRIDGE_RELATIVE_PATH);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !directory.pop() {
            return None;
        }
    }
}

/// Counts `git status --porcelain -z --untracked-files=all` entries that are
/// not Repoise's own state: the resolved cache root, or the in-repository
/// output root (artifacts and trial worktrees). NUL-separated fields carry
/// raw, unquoted paths; each entry is two status chars, a space, then the
/// path, and a rename/copy entry is followed by its origin path as an extra
/// NUL field — only the destination side is considered.
fn count_dirty_entries(
    porcelain: &str,
    root: &Path,
    cache_root: &Path,
    output_root: Option<&Path>,
) -> usize {
    let mut dirty = 0;
    let mut skipping_rename_origin = false;
    for field in porcelain.split('\0') {
        if field.is_empty() {
            continue;
        }
        if skipping_rename_origin {
            // Origin path of a rename/copy entry: the entry was already
            // counted against its destination side.
            skipping_rename_origin = false;
            continue;
        }
        if field.len() < 3 {
            continue;
        }
        let (status, path) = field.split_at(3);
        if status.starts_with(['R', 'C']) {
            skipping_rename_origin = true;
        }
        let entry = Path::new(path);
        let absolute = if entry.is_absolute() {
            entry.to_path_buf()
        } else {
            root.join(entry)
        };
        // The resolved cache root and (when inside the repository) the
        // output root are Repoise's own state, not source input: neither
        // makes the tree dirty.
        if !absolute.starts_with(cache_root)
            && !output_root.is_some_and(|output| absolute.starts_with(output))
        {
            dirty += 1;
        }
    }
    dirty
}

/// Syntactically normalizes a path (folds `.`/`..` components) without
/// requiring the path to exist, so it can be compared against the canonical
/// repository root before the output directories are created.
fn normalize_abs_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
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
    let per_trial_wall = suite
        .limits
        .wall_clock_ms
        .unwrap_or(super::suite::DEFAULT_TRIAL_WALL_CLOCK_MS);
    let estimated = trials.len().saturating_mul(per_trial_wall as usize);
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
                "eligibleChunks": preflight.status.index.as_ref().map(|i| i.eligible_chunks).unwrap_or(0),
                "vectorsPending": preflight.status.index.as_ref().map(|i| i.vectors_pending).unwrap_or(0),
                "vectorProfile": preflight.status.index.as_ref().and_then(|i| i.vector_profile.clone()),
                "embeddingCoverage": preflight.status.index.as_ref().map(|i| i.embedding_coverage.clone())
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
        "assessment": {
            "gates": serde_json::to_value(suite.gates.as_ref()).unwrap_or_default(),
            "reportSpec": serde_json::to_value(suite.report.as_ref()).unwrap_or_default(),
            "tasks": suite
                .tasks
                .iter()
                .map(|task| serde_json::json!({
                    "id": task.id,
                    "evaluatorMethod": task
                        .evaluator
                        .as_ref()
                        .map(|evaluator| evaluator.method.as_str())
                }))
                .collect::<Vec<serde_json::Value>>()
        },
        "estimatedMaxAgentWallMs": estimated,
        "assumptions": [
            "estimatedMaxAgentWallMs = trials * per-trial wallClockMs; it is an upper bound on timed agent work and excludes setup (worktree/index), setup is not estimated",
            "per-trial wallClockMs is always in effect: the coordinator applies a default of 900000 ms (15 minutes) when the suite omits limits.wallClockMs, forwards the effective value to the adapter, and enforces it with its own outer watchdog",
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
    let tasks: Vec<serde_json::Value> = input
        .suite
        .tasks
        .iter()
        .map(|task| {
            let provenance = task.provenance()?;
            serde_json::to_value(provenance).map_err(|err| Error::Json(err.to_string()))
        })
        .collect::<Result<Vec<_>, Error>>()?;
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
        "tasks": tasks,
        "gates": serde_json::to_value(input.suite.gates.as_ref()).unwrap_or_default(),
        "report": serde_json::to_value(input.suite.report.as_ref()).unwrap_or_default(),
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
#[cfg(test)]
mod tests {
    use super::*;
    fn status_index(embedding_coverage: &str) -> crate::status::StatusIndex {
        crate::status::StatusIndex {
            generation_id: 1,
            schema_version: crate::INDEX_SCHEMA_VERSION,
            files: 10,
            chunks: 12,
            built_at_ms: 0,
            manifest_hash: "hash".into(),
            config_fingerprint: "config".into(),
            parser_fingerprint: "parser".into(),
            vectors: 4,
            vector_profile: Some("profile".into()),
            embedding_scope: "docs".into(),
            eligible_chunks: 4,
            ineligible_chunks: 8,
            vectors_pending: 0,
            embedding_coverage: embedding_coverage.into(),
        }
    }

    #[test]
    fn hybrid_gate_requires_scope_aware_complete_coverage() {
        // A complete docs-scope build with config chunks intentionally left
        // lexical still passes (no role remapping needed).
        assert!(hybrid_gate_met(&status_index("complete")));
        // A partially embedded eligible corpus fails.
        assert!(!hybrid_gate_met(&status_index("partial")));
        // No stored profile (lexical-only or config-change rebuild needed).
        assert!(!hybrid_gate_met(&status_index("profile-unavailable")));
        assert!(!hybrid_gate_met(&status_index("disabled")));
        // A stored profile over an empty eligible corpus is not proof of
        // hybrid retrieval.
        assert!(!hybrid_gate_met(&status_index("empty")));
    }

    /// The test binary is built from this source checkout, so walking up
    /// from `current_exe()` locates the checked-in host bridge.
    #[test]
    fn find_bridge_never_auto_executes_a_repository_bridge() {
        let exe = std::env::current_exe().expect("test binary path");
        let Some(start) = exe.parent() else {
            return;
        };
        let Some(candidate) = bridge_candidates_from(start) else {
            // The test binary was not built inside a checkout (e.g. a
            // packed archive); the override branches still verify.
            let no_env: &dyn Fn(&str) -> Option<String> = &|_| None;
            let err = find_bridge_with(Path::new("/tmp"), no_env).unwrap_err();
            assert!(err.to_string().contains("not found"), "{err}");
            return;
        };
        let no_env: &dyn Fn(&str) -> Option<String> = &|_| None;
        // A root that contains the candidate (the benchmarked repository
        // ships the bridge) must refuse to auto-execute it.
        let root = candidate
            .ancestors()
            .find(|ancestor| ancestor.join(BRIDGE_RELATIVE_PATH) == candidate)
            .expect("candidate root");
        let err = find_bridge_with(root, no_env).unwrap_err();
        assert!(err.to_string().contains("untrusted"), "{err}");
        // The same bridge next to the executable is accepted for an
        // unrelated root.
        let ok = find_bridge_with(Path::new("/definitely-not-a-checkout"), no_env).expect("bridge");
        assert_eq!(ok, candidate);
        // An explicit override is an operator choice and always wins, even
        // when it points inside the repository root.
        let with_env: &dyn Fn(&str) -> Option<String> =
            &|name| (name == BRIDGE_ENV_VAR).then(|| candidate.to_string_lossy().into_owned());
        assert_eq!(
            find_bridge_with(root, with_env).expect("explicit override"),
            candidate
        );
        // An override pointing at a missing file is a clear preflight error.
        let bad_env: &dyn Fn(&str) -> Option<String> =
            &|name| (name == BRIDGE_ENV_VAR).then(|| "/nonexistent-bridge-7f3a.mjs".into());
        let err = find_bridge_with(root, bad_env).unwrap_err();
        assert!(err.to_string().contains("not a file"), "{err}");
    }

    /// `-z` porcelain is NUL-separated with raw (unquoted) paths, so output
    /// roots with spaces or non-ASCII names are still recognized, each
    /// untracked file under them is excluded individually, and sibling
    /// untracked files beside them still count. Rename entries count their
    /// destination side only (the origin arrives as an extra NUL field).
    #[test]
    fn nested_untracked_output_and_cache_roots_are_not_dirty() {
        let root = Path::new("/repo");
        let cache_root = Path::new("/repo/.cache/repoise");
        let output_root = Path::new("/repo/bench results/results");
        let porcelain = [
            "?? bench results/results/keep.txt",
            "?? .cache/repoise/db.sqlite",
            "?? bench results/sibling.txt",
            " M src/main.rs",
            "R  bench results/results/renamed.txt",
            "src/old.txt",
            "R  new name.txt",
            "old name.txt",
            "?? \u{6587}\u{6869}/prompt.txt",
        ]
        .join("\u{0}");
        assert_eq!(
            count_dirty_entries(&porcelain, root, cache_root, Some(output_root)),
            4
        );
        // Without the in-repo output root, the untracked output state is
        // dirt too (keep.txt and the renamed.txt destination both count).
        assert_eq!(count_dirty_entries(&porcelain, root, cache_root, None), 6);
        // Tracked-only changes are unaffected by the untracked exclusions.
        assert_eq!(
            count_dirty_entries(" D gone.txt", root, cache_root, Some(output_root)),
            1
        );
        assert_eq!(count_dirty_entries("", root, cache_root, None), 0);
    }
}
