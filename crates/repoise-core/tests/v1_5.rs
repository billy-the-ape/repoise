//! K5: bounded local history lane, lane separation from current search and
//! explicit horizon gaps. Enrichment session semantics are unit-tested in
//! `history.rs` with a fake provider; these integration tests drive the real
//! Git adapter, store migration, publication and history search lane.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use repoise_core::adapter::SnapshotMode;
use repoise_core::adapter::git::GitAdapter;
use repoise_core::cache::CachePaths;
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::history::{self, GapKind, HistorySearchRequest};
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::search::{self, SearchRequest};
use repoise_core::store::Store;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-k5-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes `content` to `rel`, stages and commits it with `message`.
fn commit_file(root: &Path, rel: &str, content: &str, message: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    run_git(root, &["add", rel]);
    run_git(
        root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            message,
        ],
    );
}

/// A scratch Git repository; `history_config` is committed when provided.
fn git_repo(tag: &str, history_config: &str) -> PathBuf {
    let root = temp_dir(tag);
    run_git(&root, &["init", "-b", "main"]);
    let extra = if history_config.is_empty() {
        String::new()
    } else {
        format!(", {history_config}")
    };
    let config = format!("{{\"schemaVersion\": 1, \"history\": {{ \"enabled\": true{extra} }}}}");
    commit_file(&root, "repoise.config.json", &config, "Add repoise config");
    root
}

struct Harness {
    root: PathBuf,
    effective: EffectiveConfig,
    cache: CachePaths,
    store: Store,
}

fn harness(root: &Path) -> Harness {
    let effective = EffectiveConfig::resolve(root, &CliOverrides::default()).unwrap();
    let cache = CachePaths::resolve(root, ".repoise", None).unwrap();
    let adapter = GitAdapter::new(root).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::WorkingTree).unwrap();
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    Harness {
        root: root.to_path_buf(),
        effective,
        cache,
        store,
    }
}

fn build(h: &Harness) -> indexing::IndexOutcome {
    let adapter = GitAdapter::new(&h.root).unwrap();
    indexing::index(
        &adapter,
        SnapshotMode::WorkingTree,
        &h.effective,
        &h.store,
        &h.cache,
        &IndexRequest::default(),
        None,
        None,
    )
    .unwrap()
}

fn current_history(h: &Harness) -> Vec<repoise_core::store::HistoryRow> {
    let adapter = GitAdapter::new(&h.root).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::WorkingTree).unwrap();
    let conn = h.store.open().unwrap();
    repoise_core::store::load_generation(&conn, &repo_id, &worktree_id)
        .unwrap()
        .expect("generation published")
        .history
}

fn history_search(h: &Harness, query: &str) -> history::HistorySearchResponse {
    let adapter = GitAdapter::new(&h.root).unwrap();
    history::search_history(
        &adapter,
        SnapshotMode::WorkingTree,
        &h.store,
        &HistorySearchRequest {
            query: query.to_string(),
            path_filter: None,
            max_results: Some(5),
            cursor: None,
        },
    )
    .unwrap()
}

#[test]
fn history_lane_publishes_and_searches_offline() {
    let root = git_repo("lane", "");
    commit_file(
        &root,
        "docs/guide.md",
        "# Guide\n\nZephyr module guide.\n",
        "Add guide (#12)",
    );
    commit_file(
        &root,
        "src/widget.ts",
        "export const widget = 1;\n",
        "Repair the zephyr module",
    );
    let h = harness(&root);
    assert!(h.effective.history.enabled);

    let outcome = build(&h);
    let summary = outcome.history.as_ref().expect("history lane published");
    // Config commit plus two content commits.
    assert_eq!(summary.count, 3);
    assert!(summary.gaps.is_empty());
    assert_eq!(current_history(&h).len(), 3);

    // The PR hint is recorded but unverified (no enrichment session).
    let rows = current_history(&h);
    let hinted = rows
        .iter()
        .find(|row| row.message == "Add guide (#12)")
        .expect("hinted commit recorded");
    assert_eq!(hinted.pr_hint.as_deref(), Some([12].as_slice()));
    assert!(hinted.association.is_none());

    // The history lane finds commits by message and path.
    let response = history_search(&h, "zephyr");
    assert_eq!(response.retrieval_mode, "history");
    assert_eq!(response.results.len(), 1);
    let hit = &response.results[0];
    assert_eq!(hit.message, "Repair the zephyr module");
    assert!(hit.revision_id.starts_with("git:"));
    assert!(
        hit.affected_paths
            .iter()
            .any(|path| path.contains("widget.ts"))
    );
    assert!(hit.association.is_none());
    assert!(hit.url.is_none(), "no remote: no permalink");

    let guide = history_search(&h, "guide");
    assert_eq!(guide.results.len(), 1);
    assert_eq!(guide.results[0].message, "Add guide (#12)");
    assert!(
        guide.results[0]
            .affected_paths
            .iter()
            .any(|path| path.contains("guide.md"))
    );

    // History items never leak into the current docs/code lane.
    let adapter = GitAdapter::new(&h.root).unwrap();
    let lexical = search::search(
        &adapter,
        SnapshotMode::WorkingTree,
        &h.store,
        &SearchRequest {
            query: "zephyr".to_string(),
            path_filter: None,
            role_filter: None,
            max_results: Some(5),
            max_output_tokens: None,
            cursor: None,
            mode: repoise_core::search::SearchMode::Lexical,
            rrf_k: None,
        },
        None,
    )
    .unwrap();
    assert!(
        lexical
            .results
            .iter()
            .all(|hit| hit.source_id.starts_with("chunk-")),
        "lexical lane contains only current chunks"
    );
}

#[test]
fn horizon_gap_is_reported_explicitly() {
    let root = git_repo("horizon", "\"horizon\": 5");
    for n in 0..6 {
        commit_file(
            &root,
            &format!("docs/p{n}.md"),
            &format!("# P{n}\n\nBody {n}.\n"),
            &format!("Document page {n}"),
        );
    }
    let h = harness(&root);
    assert_eq!(h.effective.history.horizon, 5);
    let outcome = build(&h);
    let summary = outcome.history.as_ref().expect("history lane published");
    // Config commit plus the five newest content commits.
    assert_eq!(summary.count, 5);
    let horizon_gap = summary
        .gaps
        .iter()
        .find(|gap| gap.contains("mainline"))
        .expect("horizon gap reported");
    assert!(
        horizon_gap.contains("7"),
        "gap names the full mainline: {horizon_gap}"
    );
    assert!(current_history(&h).len() == 5);
}

#[test]
fn history_off_by_default_keeps_lane_empty() {
    let root = temp_dir("off");
    run_git(&root, &["init", "-b", "main"]);
    commit_file(&root, "docs/a.md", "# A\n\nBody.\n", "First commit");
    let h = harness(&root);
    assert!(!h.effective.history.enabled);
    let outcome = build(&h);
    assert!(outcome.history.is_none(), "lane off: no history summary");
    assert!(current_history(&h).is_empty());
    let response = history_search(&h, "commit");
    assert_eq!(response.retrieval_mode, "history");
    assert!(response.results.is_empty(), "empty lane returns no results");
}

#[test]
fn gap_kinds_are_distinguishable() {
    assert_ne!(
        std::mem::discriminant(&GapKind::ShallowClone),
        std::mem::discriminant(&GapKind::Horizon)
    );
}
