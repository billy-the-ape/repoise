//! Policy-aware, bounded intake contracts: excluded and oversized files must
//! contribute zero corpus content reads/hashes, size limits must apply
//! before content I/O, and the merged scan race behavior (missing root is
//! fatal, vanishing children tolerated, genuine errors on eligible files
//! remain errors) must hold for the metadata walk and bounded reads.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use repoise_core::adapter::{
    AdapterError, Capabilities, Revision, RevisionId, SnapshotMode, SourceAdapter, SourceEntry,
    SourceKind, fake::FakeRevisionAdapter, filesystem::FilesystemAdapter, git::GitAdapter,
    to_posix,
};
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::discovery::{SkipReason, inventory};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("repoise-intake-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write file");
}

fn effective(max_file_bytes: u64, include: Vec<String>, exclude: Vec<String>) -> EffectiveConfig {
    EffectiveConfig::resolve_layers(
        None,
        None,
        &CliOverrides {
            preset: None,
            include,
            exclude,
            max_file_bytes: Some(max_file_bytes),
        },
    )
    .expect("policy builds")
}

fn file_paths(inv: &repoise_core::discovery::Inventory) -> Vec<String> {
    inv.files.iter().map(|file| to_posix(&file.path)).collect()
}

fn skip_reasons(inv: &repoise_core::discovery::Inventory) -> Vec<(String, SkipReason)> {
    inv.skips
        .iter()
        .map(|skip| (to_posix(&skip.path), skip.reason))
        .collect()
}

/// Deterministic I/O instrumentation: a counting adapter over the fake
/// revision tree. `read_bounded` counts every content read and fails with a
/// genuine I/O error for excluded directories, so any content read below an
/// excluded subtree would fail the inventory instead of being skipped.
struct CountingAdapter {
    fake: FakeRevisionAdapter,
    reads: AtomicUsize,
}

impl SourceAdapter for CountingAdapter {
    fn kind(&self) -> SourceKind {
        SourceKind::Fake
    }

    fn capabilities(&self) -> Capabilities {
        self.fake.capabilities()
    }

    fn root(&self) -> &Path {
        self.fake.root()
    }

    fn canonical_root(&self) -> Result<PathBuf, AdapterError> {
        self.fake.canonical_root()
    }

    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        mode: SnapshotMode,
    ) -> Result<Revision, AdapterError> {
        self.fake.resolve(requested, mode)
    }

    fn enumerate(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        self.fake.enumerate(revision, mode)
    }

    fn read(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        self.fake.read(revision, mode, relative)
    }

    fn read_bounded(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
        _max_bytes: u64,
    ) -> Result<Vec<u8>, AdapterError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let key = to_posix(relative);
        if key.starts_with("node_modules/") || key.starts_with("build/") {
            // A content read below an excluded subtree would surface here as
            // a genuine error, proving policy gated the read.
            return Err(AdapterError::Io(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "simulated unreadable",
            )));
        }
        match key.as_str() {
            // Simulates a file that grew past the bound after the metadata
            // check (candidate size in the fake tree is 10 bytes).
            "grow.md" => Ok(vec![b'g'; 25]),
            // Simulates a file that changed size but stayed within the bound.
            "drift.md" => Ok(vec![b'd'; 12]),
            _ => self.fake.read(revision, mode, relative),
        }
    }
}

#[test]
fn inventory_never_reads_excluded_or_oversized_content() {
    let mut tree: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    tree.insert("docs/guide.md".into(), b"hello guide".to_vec());
    tree.insert("src/app.rs".into(), b"fn main() {}".to_vec());
    tree.insert("node_modules/pkg/index.js".into(), vec![b'j'; 100]);
    tree.insert("node_modules/pkg/deep/inner.js".into(), vec![b'j'; 50]);
    tree.insert("node_modules/pkg/huge.js".into(), vec![b'j'; 300]);
    tree.insert("build/out.css".into(), vec![b'c'; 10]);
    tree.insert("Cargo.lock".into(), vec![b'l'; 10]);
    tree.insert("big.md".into(), vec![b'x'; 150]);
    tree.insert(".env".into(), b"key=1".to_vec());
    let mut revisions: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    revisions.insert("v1".into(), tree);
    let adapter = CountingAdapter {
        fake: FakeRevisionAdapter::new("fake-root", revisions),
        reads: AtomicUsize::new(0),
    };
    let eff = effective(100, Vec::new(), Vec::new());
    let inv = inventory(&adapter, SnapshotMode::WorkingTree, &eff, None).unwrap();

    // Exactly the accepted, size-eligible corpus files were read; excluded
    // subtrees (dependencies, build output, lockfile) and the oversized file
    // contributed zero content reads.
    assert_eq!(adapter.reads.load(Ordering::SeqCst), 2);
    assert_eq!(file_paths(&inv), vec!["docs/guide.md", "src/app.rs"]);
    assert_eq!(
        skip_reasons(&inv),
        vec![("big.md".to_string(), SkipReason::FileTooLarge)]
    );
    // Excluded files produce no diagnostics and no records.
    for path in [
        "node_modules/pkg/index.js",
        "build/out.css",
        "Cargo.lock",
        ".env",
    ] {
        assert!(!file_paths(&inv).contains(&path.to_string()));
        assert!(
            !skip_reasons(&inv).iter().any(|(skip, _)| skip == path),
            "excluded {path} must not appear in skips"
        );
    }

    // The manifest is deterministic across runs and carries exact hashes.
    let second = inventory(&adapter, SnapshotMode::WorkingTree, &eff, None).unwrap();
    assert_eq!(inv.manifest.manifest_hash, second.manifest.manifest_hash);
    assert_eq!(
        inv.manifest.entries[0].content_hash,
        repoise_core::hash::sha256_hex(b"hello guide")
    );
    assert_eq!(inv.manifest.entries[0].size, 11);
}

#[test]
fn inventory_bounds_files_that_grow_after_the_metadata_check() {
    let mut tree: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    tree.insert("stable.md".into(), vec![b's'; 10]);
    tree.insert("grow.md".into(), vec![b'g'; 10]);
    tree.insert("drift.md".into(), vec![b'd'; 10]);
    let mut revisions: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    revisions.insert("v1".into(), tree);
    let adapter = CountingAdapter {
        fake: FakeRevisionAdapter::new("fake-root", revisions),
        reads: AtomicUsize::new(0),
    };
    let eff = effective(20, Vec::new(), Vec::new());
    let inv = inventory(&adapter, SnapshotMode::WorkingTree, &eff, None).unwrap();

    // Grew past the bound: bounded, reported FileTooLarge, never fully read.
    assert!(skip_reasons(&inv).contains(&("grow.md".to_string(), SkipReason::FileTooLarge)));
    // Changed size within the bound: honest ChangedDuringScan.
    assert!(skip_reasons(&inv).contains(&("drift.md".to_string(), SkipReason::ChangedDuringScan)));
    // Stable file: read once, exact hash.
    assert_eq!(file_paths(&inv), vec!["stable.md"]);
    assert_eq!(
        inv.manifest.entries[0].content_hash,
        repoise_core::hash::sha256_hex(vec![b's'; 10])
    );
    assert_eq!(adapter.reads.load(Ordering::SeqCst), 3);
}

#[cfg(unix)]
#[test]
fn inventory_never_touches_ignored_unreadable_files() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_dir("unreadable-ignored");
    write(&root.join("node_modules/pkg/broken.js"), "secret content\n");
    fs::set_permissions(
        root.join("node_modules/pkg/broken.js"),
        fs::Permissions::from_mode(0o0),
    )
    .unwrap();
    write(&root.join("docs/guide.md"), "hello\n");
    // The test is only meaningful when the file is genuinely unreadable
    // (running as root would bypass the mode bits).
    let unreadable = fs::read(root.join("node_modules/pkg/broken.js")).is_err();
    if !unreadable {
        return;
    }
    let adapter = FilesystemAdapter::new(&root).unwrap();
    let eff = effective(1_000_000, Vec::new(), Vec::new());
    let inv = inventory(&adapter, SnapshotMode::PlainDirectory, &eff, None).unwrap();
    assert_eq!(file_paths(&inv), vec!["docs/guide.md"]);
}

#[cfg(unix)]
#[test]
fn inventory_keeps_genuine_read_errors_fatal_for_eligible_files() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_dir("unreadable-eligible");
    write(&root.join("docs/locked.md"), "hello\n");
    fs::set_permissions(root.join("docs/locked.md"), fs::Permissions::from_mode(0o0)).unwrap();
    if fs::read(root.join("docs/locked.md")).is_ok() {
        return; // readable (e.g. root); the permission check does not apply
    }
    let adapter = FilesystemAdapter::new(&root).unwrap();
    let eff = effective(1_000_000, Vec::new(), Vec::new());
    let result = inventory(&adapter, SnapshotMode::PlainDirectory, &eff, None);
    assert!(
        result.is_err(),
        "a genuine read failure on an eligible file must fail"
    );
}

#[test]
fn intake_scan_collects_policy_files_and_candidate_metadata_only() {
    let root = temp_dir("intake-scan");
    write(&root.join(".gitignore"), "node_modules/\n");
    write(&root.join("docs/guide.md"), "hello\n");
    write(&root.join("node_modules/pkg/index.js"), &"x".repeat(1024));
    write(&root.join("node_modules/pkg/.gitignore"), "cache/\n");
    write(&root.join("build/out.css"), "body {}\n");
    let canonical = FilesystemAdapter::new(&root)
        .unwrap()
        .canonical_root()
        .unwrap();
    let intake = repoise_core::adapter::filesystem::intake_scan(&canonical).unwrap();

    // Every regular file is a candidate with its observed size (metadata
    // only); traversal conservatively continues below excluded directories
    // so the nested policy file is still collected.
    let by_path: std::collections::HashMap<String, u64> = intake
        .candidates
        .iter()
        .map(|candidate| (to_posix(&candidate.path), candidate.size))
        .collect();
    assert_eq!(by_path.get("node_modules/pkg/index.js"), Some(&1024));
    assert_eq!(by_path.get("docs/guide.md"), Some(&6));
    assert_eq!(by_path.get(".gitignore"), Some(&14));
    assert_eq!(by_path.get("build/out.css"), Some(&8));
    let labels: Vec<String> = intake
        .native
        .iter()
        .map(|(_, label, _)| to_posix(label))
        .collect();
    assert_eq!(labels, vec![".gitignore", "node_modules/pkg/.gitignore"]);
    // Native lines are intact.
    assert_eq!(intake.native[1].2, vec!["cache/".to_string()]);
}

#[test]
fn intake_scan_never_follows_symlinks() {
    let outside = temp_dir("intake-outside");
    write(&outside.join("x.txt"), "outside\n");
    let root = temp_dir("intake-symlinks");
    write(&root.join("docs/a.md"), "hello\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(outside.join("x.txt"), root.join("link.txt")).unwrap();
        symlink(&outside, root.join("link-dir")).unwrap();
        // A self-cycle must not be followed either.
        symlink(&root, root.join("cycle")).unwrap();
    }
    let canonical = FilesystemAdapter::new(&root)
        .unwrap()
        .canonical_root()
        .unwrap();
    let intake = repoise_core::adapter::filesystem::intake_scan(&canonical).unwrap();
    let paths: Vec<String> = intake
        .candidates
        .iter()
        .map(|candidate| to_posix(&candidate.path))
        .collect();
    assert_eq!(paths, vec!["docs/a.md"]);
}

#[test]
fn intake_scan_of_a_missing_root_is_an_error() {
    let missing = temp_dir("intake-missing").join("does-not-exist");
    assert!(repoise_core::adapter::filesystem::intake_scan(&missing).is_err());
}

#[test]
fn intake_scan_tolerates_entries_vanishing_mid_walk() {
    let root = temp_dir("intake-churn");
    write(&root.join("docs/stable.md"), "hello\n");
    let churn = root.join("churn");
    fs::create_dir_all(&churn).unwrap();
    let handle = thread::spawn(move || {
        for _ in 0..400 {
            let _ = fs::write(churn.join("f.txt"), "x");
            let _ = fs::remove_file(churn.join("f.txt"));
            thread::sleep(Duration::from_micros(500));
        }
    });
    for _ in 0..5 {
        let canonical = FilesystemAdapter::new(&root)
            .unwrap()
            .canonical_root()
            .unwrap();
        let intake = repoise_core::adapter::filesystem::intake_scan(&canonical).unwrap();
        let paths: Vec<String> = intake
            .candidates
            .iter()
            .map(|candidate| to_posix(&candidate.path))
            .collect();
        assert!(paths.contains(&"docs/stable.md".to_string()));
    }
    handle.join().unwrap();
}

#[test]
fn read_bounded_stops_at_the_limit() {
    let root = temp_dir("read-bounded");
    write(&root.join("big.txt"), &"y".repeat(100));
    let adapter = FilesystemAdapter::new(&root).unwrap();
    let revision = adapter.resolve(None, SnapshotMode::PlainDirectory).unwrap();
    let bytes = adapter
        .read_bounded(
            &revision,
            SnapshotMode::PlainDirectory,
            Path::new("big.txt"),
            10,
        )
        .unwrap();
    assert_eq!(bytes.len(), 11);
    // A relative escape must still fail containment.
    let result = adapter.read_bounded(
        &revision,
        SnapshotMode::PlainDirectory,
        Path::new("../outside.txt"),
        10,
    );
    assert!(result.is_err());
}

fn run_git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
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

/// Counting wrapper over the real Git adapter: counts bounded content reads
/// and turns reads below excluded subtrees into genuine I/O errors.
struct CountingGit {
    inner: GitAdapter,
    reads: AtomicUsize,
}

impl SourceAdapter for CountingGit {
    fn kind(&self) -> SourceKind {
        SourceKind::Git
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn canonical_root(&self) -> Result<PathBuf, AdapterError> {
        self.inner.canonical_root()
    }

    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        mode: SnapshotMode,
    ) -> Result<Revision, AdapterError> {
        self.inner.resolve(requested, mode)
    }

    fn enumerate(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        self.inner.enumerate(revision, mode)
    }

    fn read(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        self.inner.read(revision, mode, relative)
    }

    fn remote_identity(&self) -> Result<Option<String>, AdapterError> {
        self.inner.remote_identity()
    }

    fn reuse_family(&self) -> Result<Option<repoise_core::adapter::ReuseFamily>, AdapterError> {
        self.inner.reuse_family()
    }

    fn history_provider(&self) -> Option<&dyn repoise_core::history::HistoryProvider> {
        self.inner.history_provider()
    }

    fn intake(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<repoise_core::adapter::SnapshotIntake, AdapterError> {
        self.inner.intake(revision, mode)
    }

    fn read_bounded(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, AdapterError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let key = to_posix(relative);
        if key.starts_with("node_modules/") || key.starts_with("third_party/sub/") {
            return Err(AdapterError::Io(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "simulated unreadable",
            )));
        }
        self.inner.read_bounded(revision, mode, relative, max_bytes)
    }
}

#[test]
fn git_committed_intake_reads_only_policy_and_accepted_blobs() {
    let root = temp_dir("git-intake");
    write(&root.join(".gitignore"), "node_modules/\n");
    write(&root.join("docs/guide.md"), "hello\n");
    write(
        &root.join("node_modules/pkg/index.js"),
        &"x".repeat(200 * 1024),
    );
    write(&root.join("third_party/.gitignore"), "sub/\n");
    write(&root.join("third_party/keep.txt"), "keep\n");
    write(&root.join("third_party/sub/inner.txt"), "inner\n");
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["add", "."]);
    run_git(
        &root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "one",
        ],
    );
    let adapter = CountingGit {
        inner: GitAdapter::new(&root).unwrap(),
        reads: AtomicUsize::new(0),
    };
    let eff = effective(1_000_000, Vec::new(), Vec::new());

    // Committed snapshot: no Git object data is read below excluded
    // subtrees; only accepted blobs (plus the policy files, which are
    // accepted corpus files here) are read.
    let committed = inventory(&adapter, SnapshotMode::Committed, &eff, None).unwrap();
    assert_eq!(
        file_paths(&committed),
        vec![
            ".gitignore",
            "docs/guide.md",
            "third_party/.gitignore",
            "third_party/keep.txt"
        ]
    );
    assert_eq!(adapter.reads.load(Ordering::SeqCst), 4);

    // A clean working tree inventories the identical corpus.
    adapter.reads.store(0, Ordering::SeqCst);
    let working = inventory(&adapter, SnapshotMode::WorkingTree, &eff, None).unwrap();
    assert_eq!(file_paths(&working), file_paths(&committed));
    assert_eq!(
        committed.manifest.manifest_hash,
        working.manifest.manifest_hash
    );
}

#[test]
fn git_bounded_blob_read_stops_at_the_limit() {
    let root = temp_dir("git-bounded");
    write(&root.join("big.txt"), &"y".repeat(100));
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["add", "."]);
    run_git(
        &root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "one",
        ],
    );
    let adapter = GitAdapter::new(&root).unwrap();
    let revision = adapter.resolve(None, SnapshotMode::Committed).unwrap();
    let bytes = adapter
        .read_bounded(&revision, SnapshotMode::Committed, Path::new("big.txt"), 10)
        .unwrap();
    assert_eq!(bytes.len(), 11);
}

#[test]
fn policy_precedence_and_reinclusion_drive_intake() {
    let root = temp_dir("policy-precedence");
    write(&root.join(".gitignore"), "notes/\n*.tmp\n!important.tmp\n");
    write(&root.join("notes/keep.md"), "keep\n");
    write(&root.join("notes/other.md"), "other\n");
    write(&root.join("scratch.tmp"), "scratch\n");
    write(&root.join("important.tmp"), "important\n");
    write(&root.join("Cargo.lock"), "lock\n");
    write(&root.join("extra/file.md"), "extra\n");
    write(&root.join("docs/guide.md"), "hello\n");
    let adapter = FilesystemAdapter::new(&root).unwrap();
    let eff = effective(
        1_000_000,
        // Config rules outrank package defaults: Cargo.lock is
        // re-includable (file-level include beats the package-default
        // exclude); the attempted notes/keep.md include cannot beat the
        // excluded ancestor directory; extra/ is excluded.
        vec!["Cargo.lock".into(), "notes/keep.md".into()],
        vec!["extra/".into()],
    );
    let inv = inventory(&adapter, SnapshotMode::PlainDirectory, &eff, None).unwrap();
    let paths = file_paths(&inv);
    // File-level negation re-includes despite the `*.tmp` exclude.
    assert!(paths.contains(&"important.tmp".to_string()));
    // Config include outranks the package-default Cargo.lock exclude.
    assert!(paths.contains(&"Cargo.lock".to_string()));
    // Directory exclusion cannot be re-included for files below it.
    assert!(!paths.contains(&"notes/keep.md".to_string()));
    assert!(!paths.contains(&"notes/other.md".to_string()));
    assert!(!paths.contains(&"scratch.tmp".to_string()));
    // Config exclude applies.
    assert!(!paths.contains(&"extra/file.md".to_string()));
    assert!(paths.contains(&"docs/guide.md".to_string()));
    // Excluded paths leave no skip diagnostics.
    assert!(skip_reasons(&inv).is_empty());
}

#[test]
fn inventory_honors_inherit_git_ignores_false() {
    let root = temp_dir("inherit-false");
    write(&root.join(".gitignore"), "ignored.md\n");
    write(&root.join("ignored.md"), "native ignored\n");
    write(&root.join("docs/guide.md"), "hello\n");
    let adapter = FilesystemAdapter::new(&root).unwrap();

    // Default (inherit enabled): the native rule excludes the file.
    let eff_on = effective(1_000_000, Vec::new(), Vec::new());
    let inv_on = inventory(&adapter, SnapshotMode::PlainDirectory, &eff_on, None).unwrap();
    assert_eq!(file_paths(&inv_on), vec![".gitignore", "docs/guide.md"]);

    // Inheritance disabled: native ignore files contribute no policy rules,
    // so the natively ignored file enters the corpus.
    let mut eff_off = effective(1_000_000, Vec::new(), Vec::new());
    eff_off.inherit_git_ignores = false;
    let inv_off = inventory(&adapter, SnapshotMode::PlainDirectory, &eff_off, None).unwrap();
    assert_eq!(
        file_paths(&inv_off),
        vec![".gitignore", "docs/guide.md", "ignored.md"]
    );

    // explain_path must report the same policy the inventory applies.
    let explained = repoise_core::discovery::explain_path(
        &adapter,
        SnapshotMode::PlainDirectory,
        &eff_off,
        Path::new("ignored.md"),
    )
    .unwrap();
    assert!(explained.included);
    assert!(explained.winning_rule.is_none());
}

#[test]
fn git_inherit_git_ignores_false_ignores_gitignore_and_local_excludes() {
    let root = temp_dir("git-inherit-false");
    write(&root.join(".gitignore"), "ignored.md\n");
    write(&root.join("ignored.md"), "native ignored\n");
    write(&root.join("docs/guide.md"), "hello\n");
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["add", "."]);
    run_git(
        &root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "one",
        ],
    );
    // Local (non-committed) excludes are part of the native intake for
    // working-tree snapshots.
    write(&root.join(".git/info/exclude"), "local.md\n");
    write(&root.join("local.md"), "locally excluded\n");
    let adapter = GitAdapter::new(&root).unwrap();

    // With inheritance disabled, neither the committed .gitignore nor the
    // local .git/info/exclude contributes policy rules.
    let mut eff = effective(1_000_000, Vec::new(), Vec::new());
    eff.inherit_git_ignores = false;
    let inv = inventory(&adapter, SnapshotMode::WorkingTree, &eff, None).unwrap();
    assert_eq!(
        file_paths(&inv),
        vec![".gitignore", "docs/guide.md", "ignored.md", "local.md"]
    );

    // With inheritance enabled (default), both native sources apply.
    let eff_on = effective(1_000_000, Vec::new(), Vec::new());
    let inv_on = inventory(&adapter, SnapshotMode::WorkingTree, &eff_on, None).unwrap();
    assert_eq!(file_paths(&inv_on), vec![".gitignore", "docs/guide.md"]);
}
