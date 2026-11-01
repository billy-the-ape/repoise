//! Index freshness is grounded in the declared snapshot: the published
//! content manifest and effective-config fingerprint decide freshness, and
//! Git working-tree dirtiness is reported separately — never judged. These
//! behavioral tests pin that down: a dirty tree whose saved contents are
//! indexed is fresh; an unindexed change is stale; committed/branch changes
//! follow the snapshot-mode rules; and the bench preflight's clean-snapshot
//! rule stays independent of index freshness.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use repoise_core::adapter::SnapshotMode;
use repoise_core::adapter::git::GitAdapter;
use repoise_core::cache::CachePaths;
use repoise_core::check::{self, CheckOptions};
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::search::{self, SearchRequest};
use repoise_core::store::Store;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The drive root of a Windows path (e.g. `C:\` from
/// `C:\Users\<user>\AppData\Local\Temp`), if any.
#[cfg(windows)]
fn windows_drive_root(path: &Path) -> Option<PathBuf> {
    let mut root = PathBuf::new();
    for component in path.components().take(2) {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                root.push(component.as_os_str());
            }
            _ => break,
        }
    }
    if root.as_os_str().is_empty() {
        None
    } else {
        Some(root)
    }
}

/// Resolves the base directory for one test. On Windows, the deepest
/// generated path is
/// <dir>/.repoise/repos/repo-<64 hex>/worktrees/wt-<64 hex>/index.sqlite-wal
/// (~180 chars after the directory); the default %TEMP% is deep
/// (C:\Users\<user>\AppData\Local\Temp), so anchor under the drive root
/// when it is writable to keep the full path well under the 260-char
/// MAX_PATH limit; otherwise fall back to the default temp directory.
/// (Same workaround as the K6 suite in v1_6.rs.)
fn base_dir(name: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = windows_drive_root(&std::env::temp_dir())
        .filter(|root| std::fs::create_dir_all(root.join(name)).is_ok())
        .map(|root| root.join(name))
    {
        return dir;
    }
    std::env::temp_dir().join(name)
}

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let name = format!("repoise-fresh-{tag}-{}-{n}", std::process::id());
    let dir = base_dir(&name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The deepest path this suite can generate under `dir` (repository and
/// worktree ids are 64 hex chars; WAL mode adds a `-wal` sidecar).
#[cfg(windows)]
fn deepest_generated_path(dir: &Path) -> PathBuf {
    dir.join(".repoise")
        .join("repos")
        .join(format!("repo-{}", "x".repeat(64)))
        .join("worktrees")
        .join(format!("wt-{}", "y".repeat(64)))
        .join("index.sqlite-wal")
}

#[cfg(windows)]
#[test]
fn temp_dirs_keep_db_paths_under_windows_max_path() {
    let dir = temp_dir("maxpath-check");
    // Only assert when the drive-root anchor was used (exactly one
    // non-root component); a permission fallback to the deep default temp
    // is a developer-machine condition this suite must tolerate.
    let names = dir
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count();
    if names == 1 {
        let deepest = deepest_generated_path(&dir);
        assert!(
            deepest.as_os_str().len() < 260,
            "deepest test path is {} chars: {}",
            deepest.as_os_str().len(),
            deepest.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_git(root: &Path, args: &[&str]) {
    // Pin LF line endings for every call: Windows runners default to
    // `core.autocrlf=true`, which would check out the committed LF fixtures
    // as CRLF (e.g. `git checkout main` in the branch-switch test) and move
    // the content manifest away from the indexed bytes.
    let output = Command::new("git")
        .args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"])
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

/// Writes `content` to `rel` and commits it with `message`.
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

/// A scratch Git repository with one committed document. The in-repo cache
/// directory is excluded from the dirty count (local policy only; the corpus
/// policy excludes it regardless).
fn git_repo(tag: &str) -> PathBuf {
    let root = temp_dir(tag);
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["config", "user.email", "test@example.com"]);
    run_git(&root, &["config", "user.name", "Test"]);
    std::fs::write(
        root.join(".git").join("info").join("exclude"),
        ".repoise/\n",
    )
    .unwrap();
    commit_file(
        &root,
        "README.md",
        "# Guide\n\nInstall the toolchain.\n",
        "Add guide",
    );
    root
}

struct Harness {
    root: PathBuf,
}

impl Harness {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn effective(&self) -> EffectiveConfig {
        EffectiveConfig::resolve(&self.root, &CliOverrides::default()).unwrap()
    }

    fn cache(&self) -> CachePaths {
        CachePaths::resolve(&self.root, ".repoise", None).unwrap()
    }

    fn store(&self, mode: SnapshotMode) -> Store {
        let adapter = GitAdapter::new(&self.root).unwrap();
        let (repo_id, worktree_id, _) = search::scope_for_search(&adapter, mode).unwrap();
        let cache = self.cache();
        Store::new(cache.db_path(&repo_id, &worktree_id))
    }

    fn build(&self, mode: SnapshotMode) {
        let adapter = GitAdapter::new(&self.root).unwrap();
        indexing::index(
            &adapter,
            mode,
            &self.effective(),
            &self.store(mode),
            &self.cache(),
            &IndexRequest::default(),
            None,
            None,
            None,
        )
        .unwrap();
    }

    fn status(&self, mode: SnapshotMode) -> repoise_core::status::StatusView {
        let adapter = GitAdapter::new(&self.root).unwrap();
        repoise_core::status::status(
            &adapter,
            mode,
            &self.effective(),
            None,
            &self.store(mode),
            &self.cache(),
        )
        .unwrap()
    }

    fn check(&self, mode: SnapshotMode, require_fresh: bool) -> check::CheckResult {
        let adapter = GitAdapter::new(&self.root).unwrap();
        check::check(
            &adapter,
            mode,
            &self.effective(),
            None,
            &self.store(mode),
            &self.cache(),
            &CheckOptions { require_fresh },
        )
        .unwrap()
    }

    fn search(&self, mode: SnapshotMode, query: &str, path_filter: Option<&str>) -> String {
        let adapter = GitAdapter::new(&self.root).unwrap();
        let effective = self.effective();
        let request = SearchRequest {
            query: query.to_string(),
            path_filter: path_filter.map(str::to_string),
            role_filter: None,
            max_results: Some(1),
            max_output_tokens: None,
            cursor: None,
            mode: repoise_core::search::SearchMode::Lexical,
            rrf_k: Some(effective.rrf_k),
        };
        let response = search::search(&adapter, mode, &self.store(mode), &request, None).unwrap();
        assert!(
            !response.results.is_empty(),
            "no search results for {query}"
        );
        response.results[0].source_id.clone()
    }
}

#[test]
fn dirty_indexed_working_tree_is_fresh() {
    let h = Harness::new(git_repo("dirty-fresh"));
    h.build(SnapshotMode::WorkingTree);
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "fresh");
    assert_eq!(view.snapshot.dirty_count, 0);

    // A saved, tracked, uncommitted edit: index the saved contents.
    std::fs::write(h.root.join("README.md"), "# Guide\n\nPinned toolchain.\n").unwrap();
    h.build(SnapshotMode::WorkingTree);
    let view = h.status(SnapshotMode::WorkingTree);
    // The index represents the saved snapshot: fresh, with dirtiness visible.
    assert_eq!(
        view.freshness.status, "fresh",
        "expected fresh, got {:?} / {:?}",
        view.freshness.status, view.freshness.reasons
    );
    assert!(view.snapshot.dirty_count > 0, "README.md is uncommitted");
    assert_eq!(view.freshness.dirty_count, view.snapshot.dirty_count);
    assert_eq!(view.freshness.branch.as_deref(), Some("main"));

    // check --fresh agrees with status.
    let result = h.check(SnapshotMode::WorkingTree, true);
    assert_eq!(result.category, check::CheckCategory::Ok);
    assert!(result.fresh);
    assert!(result.coverage_met);
}

#[test]
fn watch_published_generation_is_fresh_without_committing() {
    let h = Harness::new(git_repo("watch-dirty-fresh"));
    h.build(SnapshotMode::WorkingTree);
    let adapter = Box::new(GitAdapter::new(&h.root).unwrap());
    let options = repoise_core::watch::WatchOptions {
        interval: Duration::from_millis(10),
        debounce: Duration::from_millis(10),
        max_pending_paths: repoise_core::watch::DEFAULT_MAX_PENDING_PATHS,
        config_warning_interval: repoise_core::watch::DEFAULT_CONFIG_WARNING_INTERVAL,
    };
    let mut watcher = repoise_core::watch::Watcher::new(
        adapter,
        SnapshotMode::WorkingTree,
        h.effective(),
        h.store(SnapshotMode::WorkingTree),
        h.cache(),
        options,
        None,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);

    std::fs::write(h.root.join("note.md"), "a second document\n").unwrap();
    // Bounded publication wait: step through debounce cycles until published.
    let mut published = None;
    for _ in 0..100 {
        match watcher.step(&cancel).unwrap() {
            repoise_core::watch::WatchStep::Published(outcome) => {
                published = Some(outcome);
                break;
            }
            repoise_core::watch::WatchStep::Idle => {
                std::thread::sleep(Duration::from_millis(20));
            }
            repoise_core::watch::WatchStep::Cancelled => {
                panic!("watch cancelled unexpectedly");
            }
            repoise_core::watch::WatchStep::ConfigIssue(issue) => {
                panic!("watch reported a config issue unexpectedly: {issue:?}");
            }
        }
    }
    let outcome = published.expect("the watcher must publish the new generation");
    assert!(outcome.changed_paths.contains(&"note.md".to_string()));

    // The new generation is fresh even though nothing was committed, and it
    // serves the new content.
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(
        view.freshness.status, "fresh",
        "expected fresh, got {:?}",
        view.freshness.reasons
    );
    assert!(view.snapshot.dirty_count > 0);
    let source_id = h.search(SnapshotMode::WorkingTree, "second", Some("note.md"));
    assert!(!source_id.is_empty());
    assert!(h.check(SnapshotMode::WorkingTree, true).fresh);
}

#[test]
fn unindexed_edit_is_stale_and_exact_reads_refuse_old_bytes() {
    let h = Harness::new(git_repo("stale-after-edit"));
    h.build(SnapshotMode::WorkingTree);
    let source_id = h.search(SnapshotMode::WorkingTree, "toolchain", Some("README.md"));

    // Edit without indexing: the saved snapshot moved away from the index.
    std::fs::write(
        h.root.join("README.md"),
        "# Guide\n\nChanged without indexing.\n",
    )
    .unwrap();
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "stale");
    assert!(
        view.freshness
            .reasons
            .iter()
            .any(|reason| reason.contains("manifest")),
        "expected a manifest mismatch reason, got {:?}",
        view.freshness.reasons
    );
    assert_eq!(
        h.check(SnapshotMode::WorkingTree, true).category,
        check::CheckCategory::Stale
    );

    // A stale exact read cannot silently return the old text.
    let adapter = GitAdapter::new(&h.root).unwrap();
    let result = repoise_core::read::read(
        &adapter,
        SnapshotMode::WorkingTree,
        &h.store(SnapshotMode::WorkingTree),
        &repoise_core::read::ReadRequest {
            source_id,
            max_output_tokens: None,
        },
    );
    match result {
        Err(repoise_core::error::Error::Stale { path, .. }) => assert_eq!(path, "README.md"),
        other => panic!("expected a stale-source read failure, got {other:?}"),
    }
}

#[test]
fn committing_keeps_the_saved_snapshot_fresh_per_mode() {
    let h = Harness::new(git_repo("commit-noop"));
    // Dirty tree, indexed: fresh with dirtiness visible.
    std::fs::write(h.root.join("README.md"), "# Guide\n\nUncommitted edit.\n").unwrap();
    h.build(SnapshotMode::WorkingTree);
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "fresh");
    assert!(view.snapshot.dirty_count > 0);

    // Committing the same bytes: the working-tree snapshot is unchanged, so
    // freshness is unchanged (branch label is not the signal).
    run_git(&h.root, &["add", "-A"]);
    run_git(
        &h.root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "Commit edit",
        ],
    );
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "fresh");
    assert_eq!(view.snapshot.dirty_count, 0);

    // The committed snapshot is a separate scope with the same bytes: fresh.
    h.build(SnapshotMode::Committed);
    assert_eq!(h.status(SnapshotMode::Committed).freshness.status, "fresh");

    // Now dirty the tree again without touching committed bytes: the
    // committed snapshot stays fresh, the working-tree snapshot goes stale.
    std::fs::write(h.root.join("README.md"), "# Guide\n\nDirty again.\n").unwrap();
    assert_eq!(h.status(SnapshotMode::Committed).freshness.status, "fresh");
    assert_eq!(
        h.status(SnapshotMode::WorkingTree).freshness.status,
        "stale"
    );
}

#[test]
fn branch_change_with_different_content_is_stale_and_back_is_fresh() {
    let h = Harness::new(git_repo("branch-switch"));
    h.build(SnapshotMode::WorkingTree);
    assert_eq!(
        h.status(SnapshotMode::WorkingTree).freshness.status,
        "fresh"
    );

    // A branch with different content: checking it out changes the
    // working-tree snapshot.
    run_git(&h.root, &["checkout", "-b", "side"]);
    std::fs::write(h.root.join("README.md"), "# Guide\n\nSide branch text.\n").unwrap();
    run_git(&h.root, &["add", "-A"]);
    run_git(
        &h.root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "Side change",
        ],
    );
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "stale");
    assert!(
        view.freshness
            .reasons
            .iter()
            .any(|reason| reason.contains("manifest")),
        "expected a manifest mismatch reason, got {:?}",
        view.freshness.reasons
    );

    // Back on the indexed branch (same bytes, different branch label): fresh.
    run_git(&h.root, &["checkout", "main"]);
    assert_eq!(
        h.status(SnapshotMode::WorkingTree).freshness.status,
        "fresh"
    );
}

#[test]
fn untracked_eligible_file_stales_but_excluded_untracked_does_not() {
    let h = Harness::new(git_repo("untracked"));
    h.build(SnapshotMode::WorkingTree);
    assert_eq!(
        h.status(SnapshotMode::WorkingTree).freshness.status,
        "fresh"
    );

    // An untracked file under a built-in-excluded directory is not part of
    // the corpus snapshot: dirtiness only, still fresh.
    std::fs::create_dir_all(h.root.join("node_modules")).unwrap();
    std::fs::write(
        h.root.join("node_modules").join("dep.js"),
        "module.exports = 1;\n",
    )
    .unwrap();
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "fresh");
    assert!(view.snapshot.dirty_count > 0);

    // An untracked eligible file changes the filesystem snapshot: stale.
    std::fs::write(h.root.join("notes.md"), "untracked note\n").unwrap();
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "stale");
    assert!(
        view.freshness
            .reasons
            .iter()
            .any(|reason| reason.contains("manifest")),
        "expected a manifest mismatch reason, got {:?}",
        view.freshness.reasons
    );
}

#[test]
fn effective_config_change_stales_the_index() {
    let h = Harness::new(git_repo("config-change"));
    h.build(SnapshotMode::WorkingTree);
    assert_eq!(
        h.status(SnapshotMode::WorkingTree).freshness.status,
        "fresh"
    );

    // A new config file both changes the effective config and (as an
    // eligible file) the manifest: the index must be stale either way.
    std::fs::write(
        h.root.join(repoise_core::CONFIG_FILENAME),
        r#"{"schemaVersion":1,"maxFileBytes":123456}"#,
    )
    .unwrap();
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "stale");
    assert!(
        view.freshness
            .reasons
            .iter()
            .any(|reason| reason.contains("config changed")),
        "expected a config-change reason, got {:?}",
        view.freshness.reasons
    );
}

#[test]
fn missing_index_reports_unknown() {
    let h = Harness::new(git_repo("missing"));
    let view = h.status(SnapshotMode::WorkingTree);
    assert_eq!(view.freshness.status, "unknown");
    assert!(view.index.is_none());
    assert_eq!(
        h.check(SnapshotMode::WorkingTree, true).category,
        check::CheckCategory::Missing
    );
}

#[test]
fn bench_preflight_rejects_dirty_consumer_checkout_even_with_fresh_index() {
    let h = Harness::new(git_repo("preflight-dirty"));
    h.build(SnapshotMode::WorkingTree);
    // The index is fresh over the saved snapshot...
    assert_eq!(
        h.status(SnapshotMode::WorkingTree).freshness.status,
        "fresh"
    );

    // ...but the consumer checkout is dirty, which a commit-snapshot suite
    // must reject independently of index freshness.
    std::fs::write(
        h.root.join("README.md"),
        "# Guide\n\nDirty consumer checkout.\n",
    )
    .unwrap();
    let suite: repoise_core::bench::suite::Suite = serde_json::from_value(serde_json::json!({
        "id": "exp",
        "root": h.root.to_string_lossy().into_owned(),
        "snapshot": { "kind": "commit", "revision": "HEAD" },
        "tasks": [{ "id": "task-a", "prompt": "do it" }],
        "conditions": [{ "id": "baseline", "retrievalMode": "baseline" }],
        "adapter": { "executable": "node" },
        "profile": { "harness": { "name": "fake" } }
    }))
    .unwrap();
    let suite_path = h.root.join("suite.json");
    let output_root = temp_dir("preflight-output");
    let err = repoise_core::bench::preflight::run(&suite, &suite_path, &output_root).unwrap_err();
    assert!(
        err.to_string().contains("working tree is dirty"),
        "expected the clean-snapshot preflight rejection, got {err}"
    );
}

/// A config-only branch switch (the committed config file appears/disappears
/// while no corpus file moves) reloads the effective configuration in the
/// running watcher and republishes accordingly.
#[test]
fn watch_config_only_branch_switch_reloads_effective_config() {
    let root = temp_dir("watch-branch-config");
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["config", "user.email", "test@example.com"]);
    run_git(&root, &["config", "user.name", "Test"]);
    std::fs::write(
        root.join(".git").join("info").join("exclude"),
        ".repoise/\n",
    )
    .unwrap();
    commit_file(
        &root,
        "README.md",
        "# Guide\n\nInstall the toolchain.\n",
        "Add guide",
    );
    commit_file(
        &root,
        "private/notes.md",
        "# Notes\n\nPangolin quokka.\n",
        "Add private notes",
    );

    let h = Harness::new(root);
    h.build(SnapshotMode::WorkingTree);
    let adapter = Box::new(GitAdapter::new(&h.root).unwrap());
    let options = repoise_core::watch::WatchOptions {
        interval: Duration::from_millis(10),
        debounce: Duration::from_millis(10),
        max_pending_paths: repoise_core::watch::DEFAULT_MAX_PENDING_PATHS,
        config_warning_interval: repoise_core::watch::DEFAULT_CONFIG_WARNING_INTERVAL,
    };
    let mut watcher = repoise_core::watch::Watcher::new(
        adapter,
        SnapshotMode::WorkingTree,
        h.effective(),
        h.store(SnapshotMode::WorkingTree),
        h.cache(),
        options,
        None,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    assert!(matches!(
        watcher.step(&cancel).unwrap(),
        repoise_core::watch::WatchStep::Idle
    ));

    // Config-only branch switch: the committed config file appears on disk.
    run_git(&h.root, &["checkout", "-b", "config-branch"]);
    commit_file(
        &h.root,
        "repoise.config.json",
        r#"{"schemaVersion":1,"exclude":["private"]}"#,
        "Exclude private notes",
    );
    let outcome = step_until_published(&mut watcher, &cancel);
    assert!(
        outcome.config_change,
        "the branch switch must show as a config-driven generation"
    );
    assert!(outcome.branch_switch);
    assert!(
        h.check(SnapshotMode::WorkingTree, true).fresh,
        "the republished generation must be fresh"
    );
    // The excluded corpus must leave the index.
    assert!(
        lex_hits_empty(&h, "pangolin"),
        "private notes must be excluded after the switch"
    );

    // Switching back removes the config file: the corpus comes back.
    run_git(&h.root, &["checkout", "main"]);
    let outcome = step_until_published(&mut watcher, &cancel);
    assert!(
        outcome.config_change,
        "removing the config file must also be config-driven"
    );
    assert!(outcome.branch_switch);
    assert!(
        !lex_hits_empty(&h, "pangolin"),
        "the private notes must be indexed again"
    );
}

/// Bounded publication wait: step through debounce cycles until published.
fn step_until_published(
    watcher: &mut repoise_core::watch::Watcher,
    cancel: &AtomicBool,
) -> repoise_core::watch::WatchPublication {
    for _ in 0..200 {
        match watcher.step(cancel).unwrap() {
            repoise_core::watch::WatchStep::Published(outcome) => return outcome,
            repoise_core::watch::WatchStep::Idle => {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => panic!("unexpected watcher step: {other:?}"),
        }
    }
    panic!("the watcher did not publish within the bounded wait")
}

/// True when a working-tree lexical query returns no hits at all.
fn lex_hits_empty(h: &Harness, query: &str) -> bool {
    let adapter = GitAdapter::new(&h.root).unwrap();
    let effective = h.effective();
    let request = SearchRequest {
        query: query.to_string(),
        path_filter: None,
        role_filter: None,
        max_results: Some(5),
        max_output_tokens: None,
        cursor: None,
        mode: repoise_core::search::SearchMode::Lexical,
        rrf_k: Some(effective.rrf_k),
    };
    let response = search::search(
        &adapter,
        SnapshotMode::WorkingTree,
        &h.store(SnapshotMode::WorkingTree),
        &request,
        None,
    )
    .unwrap();
    response.results.is_empty()
}
