//! K6: related-knowledge service, offline check categories, incremental
//! watch, and the non-destructive overlay lifecycle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use repoise_core::adapter::{SnapshotMode, SourceAdapter};
use repoise_core::cache::CachePaths;
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::search::{self, SearchRequest};
use repoise_core::store::Store;

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-k6-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A real, plain-directory repository the watcher can mutate on disk.
struct DirRepo {
    root: PathBuf,
}

impl DirRepo {
    fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        let root = temp_dir(tag);
        for (path, content) in files {
            let target = root.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&target, content).unwrap();
        }
        Self { root }
    }

    fn context(
        &self,
    ) -> (
        Box<dyn SourceAdapter>,
        SnapshotMode,
        EffectiveConfig,
        CachePaths,
    ) {
        let adapter =
            repoise_core::adapter::filesystem::FilesystemAdapter::new(&self.root).unwrap();
        let effective = EffectiveConfig::resolve(&self.root, &CliOverrides::default()).unwrap();
        let cache = CachePaths::resolve(&self.root, ".repoise", None).unwrap();
        (
            Box::new(adapter),
            SnapshotMode::PlainDirectory,
            effective,
            cache,
        )
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.root.join(path), content).unwrap();
    }

    fn remove(&self, path: &str) {
        std::fs::remove_file(self.root.join(path)).unwrap();
    }

    fn store(&self, cache: &CachePaths) -> Store {
        let adapter =
            repoise_core::adapter::filesystem::FilesystemAdapter::new(&self.root).unwrap();
        let (repo_id, worktree_id, _) =
            search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
        Store::new(cache.db_path(&repo_id, &worktree_id))
    }
}

const DOC_MD: &str = "# Guide\n\n## Setup\n\nInstall the toolchain.\n\n## Usage\n\nRun the CLI.\n";
const MATH_TS: &str =
    "export function multiply(a: number, b: number): number {\n  return a * b;\n}\n";
const FORMAT_TS: &str = "import { multiply } from \"./math\";\nexport function render(value: number): string {\n  return String(multiply(value, 2));\n}\n";

fn build_index(repo: &DirRepo) -> indexing::IndexOutcome {
    let (adapter, mode, effective, cache) = repo.context();
    let store = repo.store(&cache);
    indexing::index(
        adapter.as_ref(),
        mode,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        None,
        None,
    )
    .unwrap()
}

/// Finds a chunk source id in a given path by lexical search.
fn source_id_for(repo: &DirRepo, path: &str) -> String {
    let (adapter, mode, effective, cache) = repo.context();
    let store = repo.store(&cache);
    let request = SearchRequest {
        query: path.to_string(),
        path_filter: Some(path.to_string()),
        role_filter: None,
        max_results: Some(1),
        max_output_tokens: None,
        cursor: None,
        mode: repoise_core::search::SearchMode::Lexical,
        rrf_k: Some(effective.rrf_k),
    };
    let response =
        repoise_core::search::search(adapter.as_ref(), mode, &store, &request, None).unwrap();
    response.results[0].source_id.clone()
}

fn check_of(repo: &DirRepo) -> repoise_core::check::CheckResult {
    let (adapter, mode, effective, cache) = repo.context();
    let store = repo.store(&cache);
    repoise_core::check::check(
        adapter.as_ref(),
        mode,
        &effective,
        None,
        &store,
        &cache,
        &repoise_core::check::CheckOptions::default(),
    )
    .unwrap()
}

#[test]
fn related_follows_reference_edges_and_heading_structure() {
    let repo = DirRepo::new(
        "related",
        &[
            ("README.md", DOC_MD),
            ("src/math.ts", MATH_TS),
            ("src/format.ts", FORMAT_TS),
        ],
    );
    build_index(&repo);
    let (adapter, mode, _effective, cache) = repo.context();
    let store = repo.store(&cache);

    // A format.ts chunk references multiply in math.ts.
    let source_id = source_id_for(&repo, "src/format.ts");
    let request = repoise_core::related::RelatedRequest {
        source_id,
        kinds: vec![repoise_core::related::RelationKind::References],
        limit: Some(20),
        max_output_tokens: None,
    };
    let response =
        repoise_core::related::related(adapter.as_ref(), mode, &store, &request).unwrap();
    assert_eq!(response.schema_version, 1);
    let referenced: Vec<&repoise_core::related::RelatedHit> = response
        .results
        .iter()
        .filter(|hit| hit.path == "src/math.ts" && hit.name.as_deref() == Some("multiply"))
        .collect();
    assert!(
        !referenced.is_empty(),
        "reference edge to math.ts expected, got {:?}",
        response.results
    );
    assert!(referenced[0].edge_kind.is_some());

    // Referenced-by is the inverse edge: math.ts is referenced by format.ts.
    let source_id = source_id_for(&repo, "src/math.ts");
    let request = repoise_core::related::RelatedRequest {
        source_id,
        kinds: vec![repoise_core::related::RelationKind::ReferencedBy],
        limit: None,
        max_output_tokens: None,
    };
    let response =
        repoise_core::related::related(adapter.as_ref(), mode, &store, &request).unwrap();
    assert!(
        response.results.iter().any(|hit| {
            hit.path == "src/format.ts"
                && hit.name.as_deref() == Some("multiply")
                && hit.edge_kind.is_some()
        }),
        "referenced-by format.ts expected, got {:?}",
        response.results
    );

    // Unknown source ids are a stable index-state error.
    let missing = repoise_core::related::related(
        adapter.as_ref(),
        mode,
        &store,
        &repoise_core::related::RelatedRequest {
            source_id: "does-not-exist".to_string(),
            kinds: Vec::new(),
            limit: None,
            max_output_tokens: None,
        },
    );
    assert!(matches!(
        missing,
        Err(repoise_core::error::Error::IndexState(_))
    ));
}

#[test]
fn related_limit_bounds_results() {
    let repo = DirRepo::new("related-limit", &[("README.md", DOC_MD)]);
    build_index(&repo);
    let (adapter, mode, _effective, cache) = repo.context();
    let store = repo.store(&cache);
    let source_id = source_id_for(&repo, "README.md");
    let response = repoise_core::related::related(
        adapter.as_ref(),
        mode,
        &store,
        &repoise_core::related::RelatedRequest {
            source_id,
            kinds: Vec::new(),
            limit: Some(1),
            max_output_tokens: None,
        },
    )
    .unwrap();
    assert!(response.results.len() <= 1);
}
#[test]
fn check_categories_missing_ok_stale() {
    let repo = DirRepo::new("check", &[("README.md", DOC_MD)]);

    // No published index: missing.
    let result = check_of(&repo);
    assert_eq!(result.category, repoise_core::check::CheckCategory::Missing);
    assert!(!result.fresh);

    // Index built and unchanged: ok.
    build_index(&repo);
    let result = check_of(&repo);
    assert_eq!(result.category, repoise_core::check::CheckCategory::Ok);
    assert!(result.fresh);
    assert!(result.coverage_met);

    // A live change: stale, with reasons.
    repo.write(
        "README.md",
        "# Guide\n\n## Setup\n\nBootstrapped now.\n\n## Usage\n\nRun the CLI.\n",
    );
    let result = check_of(&repo);
    assert_eq!(result.category, repoise_core::check::CheckCategory::Stale);
    assert!(!result.fresh);
    assert!(!result.reasons.is_empty());
}

#[test]
fn check_reports_unmet_vector_coverage_without_provider_calls() {
    let repo = DirRepo::new("check-coverage", &[("README.md", DOC_MD)]);
    std::fs::write(
        repo.root.join(repoise_core::CONFIG_FILENAME),
        r#"{"schemaVersion":1,"preset":"hybrid","embedding":{"provider":"openai"}}"#,
    )
    .unwrap();
    build_index(&repo);
    let result = check_of(&repo);
    assert_eq!(
        result.category,
        repoise_core::check::CheckCategory::CoverageNotMet
    );
    assert!(result.fresh);
    assert!(!result.coverage_met);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.contains("hybrid preset requires vectors")),
        "expected the vector coverage reason, got {:?}",
        result.reasons
    );
}

fn quick_watcher(
    repo: &DirRepo,
    max_pending: usize,
) -> (
    repoise_core::watch::Watcher,
    repoise_core::watch::WatchOptions,
) {
    let (adapter, mode, effective, cache) = repo.context();
    let store = repo.store(&cache);
    let options = repoise_core::watch::WatchOptions {
        interval: Duration::from_millis(10),
        debounce: Duration::from_millis(10),
        max_pending_paths: max_pending,
    };
    (
        repoise_core::watch::Watcher::new(adapter, mode, effective, store, cache, options).unwrap(),
        options,
    )
}

#[test]
fn watch_publishes_incremental_changes_and_then_idles() {
    let repo = DirRepo::new("watch", &[("README.md", DOC_MD), ("src/math.ts", MATH_TS)]);
    build_index(&repo);
    let (mut watcher, _options) =
        quick_watcher(&repo, repoise_core::watch::DEFAULT_MAX_PENDING_PATHS);
    let cancel = AtomicBool::new(false);

    // Nothing changed: idle, no new generation published.
    assert!(matches!(
        watcher.step(&cancel).unwrap(),
        repoise_core::watch::WatchStep::Idle
    ));

    // Add a file: one incremental publication naming exactly that path.
    repo.write("src/format.ts", FORMAT_TS);
    match watcher.step(&cancel).unwrap() {
        repoise_core::watch::WatchStep::Published(outcome) => {
            assert_eq!(outcome.changed_paths, vec!["src/format.ts".to_string()]);
            assert!(!outcome.force_full);
            assert!(!outcome.branch_switch);
        }
        other => panic!("expected publication, got {other:?}"),
    }

    // Settled state: idle again (no duplicate publication).
    assert!(matches!(
        watcher.step(&cancel).unwrap(),
        repoise_core::watch::WatchStep::Idle
    ));

    // Modify the new file: it reparses and publishes again.
    repo.write("src/format.ts", "import { multiply } from \"./math\";\nexport function render(value: number): string {\n  return String(multiply(value, 4));\n}\n");
    match watcher.step(&cancel).unwrap() {
        repoise_core::watch::WatchStep::Published(outcome) => {
            assert_eq!(outcome.changed_paths, vec!["src/format.ts".to_string()]);
            assert!(outcome.files_reparsed >= 1);
        }
        other => panic!("expected publication, got {other:?}"),
    }
}

#[test]
fn watch_detects_deletions() {
    let repo = DirRepo::new(
        "watch-delete",
        &[("README.md", DOC_MD), ("src/math.ts", MATH_TS)],
    );
    build_index(&repo);
    let (mut watcher, _options) =
        quick_watcher(&repo, repoise_core::watch::DEFAULT_MAX_PENDING_PATHS);
    let cancel = AtomicBool::new(false);
    repo.remove("src/math.ts");
    match watcher.step(&cancel).unwrap() {
        repoise_core::watch::WatchStep::Published(outcome) => {
            assert_eq!(outcome.changed_paths, vec!["src/math.ts".to_string()]);
        }
        other => panic!("expected publication, got {other:?}"),
    }
}

#[test]
fn watch_honors_cancellation_during_debounce() {
    let repo = DirRepo::new("watch-cancel", &[("README.md", DOC_MD)]);
    build_index(&repo);
    let (mut watcher, _options) =
        quick_watcher(&repo, repoise_core::watch::DEFAULT_MAX_PENDING_PATHS);
    let cancel = AtomicBool::new(true);
    repo.write("extra.md", "# Extra\n\nBody.\n");
    assert!(matches!(
        watcher.step(&cancel).unwrap(),
        repoise_core::watch::WatchStep::Cancelled
    ));
}

#[test]
fn watch_bounds_pending_paths_with_full_reparse() {
    let repo = DirRepo::new(
        "watch-bounded",
        &[
            ("README.md", DOC_MD),
            ("a.md", "# A\n"),
            ("b.md", "# B\n"),
            ("c.md", "# C\n"),
        ],
    );
    build_index(&repo);
    let (mut watcher, _options) = quick_watcher(&repo, 1);
    let cancel = AtomicBool::new(false);
    repo.write("a.md", "# A2\n");
    repo.write("b.md", "# B2\n");
    match watcher.step(&cancel).unwrap() {
        repoise_core::watch::WatchStep::Published(outcome) => {
            assert!(
                outcome.force_full,
                "more than the bounded pending set should force a full reparse"
            );
        }
        other => panic!("expected publication, got {other:?}"),
    }
}

fn init_overlay(
    repo: &DirRepo,
    agents: bool,
) -> (
    repoise_core::init::InitPlan,
    repoise_core::init::InitOutcome,
) {
    let options = repoise_core::init::InitOptions {
        preset: repoise_core::config::Preset::DocsOnly,
        dry_run: false,
        yes: true,
        provider: None,
        adopt_managed_block: Some(PathBuf::from("README.md")),
        agents_snippet: agents,
    };
    let plan = repoise_core::init::plan(&repo.root, &options).unwrap();
    let outcome = repoise_core::init::apply(&plan).unwrap();
    (plan, outcome)
}

#[test]
fn overlay_init_is_idempotent_and_tracks_managed_bytes() {
    let repo = DirRepo::new("overlay-init", &[("README.md", "# Repo\n\nHello world.\n")]);
    let (plan, outcome) = init_overlay(&repo, true);
    assert!(outcome.conflicts.is_empty());
    assert!(repo.root.join(repoise_core::CONFIG_FILENAME).exists());
    assert!(repo.root.join(repoise_core::OVERLAY_FILENAME).exists());

    // The managed block landed in README and the snippet in AGENTS.md.
    let readme = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    assert!(readme.contains(&repoise_core::init::managed_block()));
    assert!(readme.starts_with("# Repo\n\nHello world.\n"));
    let agents = std::fs::read_to_string(repo.root.join("AGENTS.md")).unwrap();
    assert!(agents.contains(&repoise_core::init::agents_snippet_block()));

    // The manifest records roles and installed bytes for every entry.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.root.join(repoise_core::OVERLAY_FILENAME)).unwrap(),
    )
    .unwrap();
    let files = manifest["files"].as_array().unwrap();
    let roles: Vec<&str> = files
        .iter()
        .filter_map(|entry| entry["role"].as_str())
        .collect();
    assert!(roles.contains(&"config"));
    assert!(roles.contains(&"managed-block"));
    assert!(roles.contains(&"agents-snippet"));
    assert!(files.iter().all(|entry| entry["installed"].is_string()));

    // Re-planning the same overlay is idempotent.
    let options = repoise_core::init::InitOptions {
        preset: repoise_core::config::Preset::DocsOnly,
        dry_run: true,
        yes: true,
        provider: None,
        adopt_managed_block: Some(PathBuf::from("README.md")),
        agents_snippet: true,
    };
    let plan2 = repoise_core::init::plan(&repo.root, &options).unwrap();
    for file in &plan2.files {
        assert_eq!(
            file.action,
            repoise_core::init::FileAction::Unchanged,
            "{} should be unchanged",
            file.relative.display()
        );
    }
    let _ = plan;
}

#[test]
fn overlay_uninstall_removes_only_unchanged_managed_bytes() {
    let repo = DirRepo::new(
        "overlay-uninstall",
        &[("README.md", "# Repo\n\nHello world.\n")],
    );
    init_overlay(&repo, true);
    let readme_before = std::fs::read_to_string(repo.root.join("README.md")).unwrap();

    // Dry run: nothing is written.
    let dry = repoise_core::overlay::uninstall(&repo.root, true).unwrap();
    assert!(dry.dry_run);
    assert!(repo.root.join(repoise_core::OVERLAY_FILENAME).exists());
    assert_eq!(
        std::fs::read_to_string(repo.root.join("README.md")).unwrap(),
        readme_before
    );

    // Real uninstall: config, manifest and blocks go; README body survives.
    let done = repoise_core::overlay::uninstall(&repo.root, false).unwrap();
    assert!(done.conflicts.is_empty());
    assert!(done.manifest_removed);
    assert!(!repo.root.join(repoise_core::CONFIG_FILENAME).exists());
    assert!(!repo.root.join(repoise_core::OVERLAY_FILENAME).exists());
    let readme_after = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    assert!(!readme_after.contains("repoise:managed"));
    assert!(readme_after.contains("# Repo\n\nHello world.\n"));
    // AGENTS.md contained only the snippet block: it is removed entirely.
    assert!(!repo.root.join("AGENTS.md").exists());
}

#[test]
fn overlay_uninstall_keeps_owner_modified_bytes_and_manifest() {
    let repo = DirRepo::new(
        "overlay-conflict",
        &[("README.md", "# Repo\n\nHello world.\n")],
    );
    init_overlay(&repo, false);
    let config = repo.root.join(repoise_core::CONFIG_FILENAME);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    value["preset"] = serde_json::json!("docs-code-lexical");
    std::fs::write(&config, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let outcome = repoise_core::overlay::uninstall(&repo.root, false).unwrap();
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(outcome.conflicts[0].path, repoise_core::CONFIG_FILENAME);
    assert!(!outcome.manifest_removed);
    assert!(config.exists());
    // The untouched README block is still removed.
    let readme = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    assert!(!readme.contains("repoise:managed"));
}

/// Simulates a template upgrade: rewrite the installed baselines in the
/// manifest (and the matching live bytes) so the fresh render diverges.
fn simulate_template_upgrade(repo: &DirRepo) {
    let manifest_path = repo.root.join(repoise_core::OVERLAY_FILENAME);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let old_block = repoise_core::init::managed_block().replace("1.0.0", "0.9.0");
    for entry in manifest["files"].as_array_mut().unwrap().iter_mut() {
        match entry["path"].as_str().unwrap() {
            repoise_core::CONFIG_FILENAME => {
                let baseline = "OLD-CONFIG-BASELINE";
                entry["installed"] = serde_json::json!(baseline);
                std::fs::write(repo.root.join(repoise_core::CONFIG_FILENAME), baseline).unwrap();
            }
            "README.md" => {
                entry["installed"] = serde_json::json!(&old_block);
                std::fs::write(
                    repo.root.join("README.md"),
                    format!("# Repo\n\nHello world.\n{old_block}\n"),
                )
                .unwrap();
            }
            _ => {}
        }
    }
    std::fs::write(
        manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn overlay_update_advances_clean_entries_and_conflicts_divergent_ones() {
    let repo = DirRepo::new(
        "overlay-update",
        &[("README.md", "# Repo\n\nHello world.\n")],
    );
    init_overlay(&repo, false);
    simulate_template_upgrade(&repo);

    // Dry run: reports advancement without writing.
    let dry = repoise_core::overlay::update(&repo.root, true).unwrap();
    assert!(dry.dry_run);
    assert!(
        dry.updated
            .iter()
            .any(|path| path == repoise_core::CONFIG_FILENAME)
    );
    let readme = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    assert!(readme.contains("0.9.0"), "dry run must not write");

    // Real update: config re-rendered, block advanced, manifest re-pinned.
    let done = repoise_core::overlay::update(&repo.root, false).unwrap();
    assert!(done.conflicts.is_empty());
    assert!(done.manifest_updated);
    let config = std::fs::read_to_string(repo.root.join(repoise_core::CONFIG_FILENAME)).unwrap();
    assert!(config.contains("\"preset\": \"docs-only\""));
    let readme = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    assert!(readme.contains(&repoise_core::init::managed_block()));
    assert!(readme.starts_with("# Repo\n\nHello world.\n"));
    assert!(!readme.contains("0.9.0"));

    // Settled: a second update changes nothing.
    let again = repoise_core::overlay::update(&repo.root, false).unwrap();
    assert!(again.updated.is_empty());
    assert!(!again.manifest_updated);
}

#[test]
fn overlay_update_conflicts_on_owner_modified_managed_bytes() {
    let repo = DirRepo::new(
        "overlay-update-conflict",
        &[("README.md", "# Repo\n\nHello world.\n")],
    );
    init_overlay(&repo, false);
    // The owner edited the managed block in place.
    let readme = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    let edited = readme.replace("run `repoise doctor`", "run `repoise doctor --strict`");
    std::fs::write(repo.root.join("README.md"), &edited).unwrap();

    let outcome = repoise_core::overlay::update(&repo.root, false).unwrap();
    assert!(
        outcome
            .conflicts
            .iter()
            .any(|conflict| conflict.path == "README.md"),
        "modified block must conflict, got {:?}",
        outcome.conflicts
    );
    assert!(!outcome.manifest_updated);
    // Owner bytes are preserved exactly.
    assert_eq!(
        std::fs::read_to_string(repo.root.join("README.md")).unwrap(),
        edited
    );
}

#[test]
fn overlay_update_rejects_pre_v2_manifest_without_baseline() {
    let repo = DirRepo::new("overlay-pre-v2", &[("README.md", "# Repo\n")]);
    std::fs::write(
        repo.root.join(repoise_core::CONFIG_FILENAME),
        r#"{"schemaVersion":1,"preset":"docs-only"}"#,
    )
    .unwrap();
    std::fs::write(
        repo.root.join(repoise_core::OVERLAY_FILENAME),
        r#"{"tool_version":"0.0.0","template_version":"0.0.0","files":["repoise.config.json"]}"#,
    )
    .unwrap();
    let outcome = repoise_core::overlay::update(&repo.root, false).unwrap();
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(outcome.conflicts[0].path, repoise_core::CONFIG_FILENAME);
    assert!(
        outcome.conflicts[0]
            .reason
            .contains("re-run `repoise init`")
    );
}
#[test]
fn overlay_update_advances_baselines_despite_sibling_conflicts() {
    let repo = DirRepo::new(
        "overlay-partial",
        &[("README.md", "# Repo\n\nHello world.\n")],
    );
    init_overlay(&repo, false);
    simulate_template_upgrade(&repo);
    // The owner edited the managed block in place: it will conflict, while
    // the config entry stays clean (live bytes == old baseline).
    let readme = std::fs::read_to_string(repo.root.join("README.md")).unwrap();
    let edited = readme.replace("run `repoise doctor`", "run `repoise doctor --strict`");
    std::fs::write(repo.root.join("README.md"), &edited).unwrap();

    // Partial update: config advances and its baseline is re-pinned even
    // though the sibling entry conflicts.
    let done = repoise_core::overlay::update(&repo.root, false).unwrap();
    assert_eq!(
        done.updated,
        vec![repoise_core::CONFIG_FILENAME.to_string()]
    );
    assert_eq!(done.conflicts.len(), 1);
    assert_eq!(done.conflicts[0].path, "README.md");
    assert!(done.manifest_updated);

    // Settled for the clean entry: the second update is a no-op for it and
    // does not re-pin the manifest.
    let again = repoise_core::overlay::update(&repo.root, false).unwrap();
    assert!(again.updated.is_empty());
    assert!(
        again
            .unchanged
            .contains(&repoise_core::CONFIG_FILENAME.to_string())
    );
    assert_eq!(again.conflicts.len(), 1);
    assert!(!again.manifest_updated);

    // Uninstall: only the genuine owner edit conflicts; the advanced config
    // is removed without a false "modified since init" conflict.
    let removed = repoise_core::overlay::uninstall(&repo.root, false).unwrap();
    assert_eq!(removed.conflicts.len(), 1);
    assert_eq!(removed.conflicts[0].path, "README.md");
    assert!(!removed.manifest_removed);
    assert!(!repo.root.join(repoise_core::CONFIG_FILENAME).exists());
}
