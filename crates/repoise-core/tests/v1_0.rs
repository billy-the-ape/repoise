//! V1-0 contract tests: adapters, policy, config, discovery and init.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use repoise_core::adapter::{
    SnapshotMode, SourceAdapter, SourceEntry, fake::FakeRevisionAdapter,
    filesystem::FilesystemAdapter, git::GitAdapter,
};
use repoise_core::classify::{self, ClassificationSource, Lifecycle, Role, RoleRule};
use repoise_core::config::{CliOverrides, EffectiveConfig, Preset};
use repoise_core::discovery::{SkipReason, inventory};
use repoise_core::ignore::{Policy, scan_secret_content};
use repoise_core::init::{InitOptions, apply, plan};
use repoise_core::provenance::SnapshotManifest;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("repoise-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write file");
}

#[test]
fn hash_is_deterministic_sha256() {
    assert_eq!(
        repoise_core::hash::sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        repoise_core::hash::sha256_hex(""),
        repoise_core::hash::sha256_hex("")
    );
}

#[test]
fn manifest_is_deterministic_and_content_sensitive() {
    let entries = vec![
        SourceEntry {
            path: "b.md".into(),
            content_hash: "bb".into(),
            size: 2,
        },
        SourceEntry {
            path: "a.md".into(),
            content_hash: "aa".into(),
            size: 2,
        },
    ];
    let first = SnapshotManifest::build(entries.clone());
    let second = SnapshotManifest::build(entries.clone());
    assert_eq!(first.manifest_hash, second.manifest_hash);
    assert_eq!(first.entries[0].path.as_os_str(), "a.md");

    let mut changed = entries.clone();
    changed[0].content_hash = "cc".into();
    assert_ne!(
        first.manifest_hash,
        SnapshotManifest::build(changed).manifest_hash
    );
}

#[test]
fn classify_uses_front_matter_then_rules_then_inference() {
    let front = "---\nrole: decision\nstatus: accepted\n---\ntext";
    assert_eq!(
        classify::classify(Path::new("docs/whatever.md"), &[], front),
        (
            Role::Decision,
            Lifecycle::Accepted,
            ClassificationSource::Explicit
        )
    );

    let rule = RoleRule {
        pattern: "docs/decisions/**".into(),
        role: Role::Decision,
    };
    assert_eq!(
        classify::classify(
            Path::new("docs/decisions/001.md"),
            std::slice::from_ref(&rule),
            "no front matter"
        )
        .0,
        Role::Decision
    );

    let (role, lifecycle, source) =
        classify::classify(Path::new("docs/plans/v1-0.md"), &[], "plain");
    assert_eq!((role, lifecycle), (Role::Plan, Lifecycle::Proposed));
    assert_eq!(source, ClassificationSource::Inferred);

    assert_eq!(classify::detect_language(Path::new("a.rs")), "rs");
    assert_eq!(classify::detect_language(Path::new("guide.MD")), "markdown");
    assert_eq!(classify::detect_language(Path::new("Makefile")), "text");
}

#[test]
fn policy_applies_gitignore_semantics_with_absolute_secrets() {
    let eff = EffectiveConfig::resolve_layers(
        None,
        None,
        &CliOverrides {
            preset: None,
            include: vec!["!dist/keep.md".into()],
            exclude: vec!["*.tmp".into(), "notes".into()],
            max_file_bytes: None,
        },
    )
    .expect("policy builds");
    let native: Vec<(std::path::PathBuf, std::path::PathBuf, Vec<String>)> = vec![(
        std::path::PathBuf::new(),
        std::path::PathBuf::from(".gitignore"),
        vec!["vendor/".into(), "build".into()],
    )];
    let policy = Policy::build(&eff, &native).expect("policy builds");

    // Default include.
    assert!(policy.decide(Path::new("docs/guide.md")).included);
    // Config exclude glob.
    assert!(!policy.decide(Path::new("scratch.tmp")).included);
    // Directory exclude: files below are excluded; negation cannot re-include.
    assert!(!policy.decide(Path::new("notes/file.md")).included);
    assert!(!policy.decide(Path::new("dist/keep.md")).included);
    assert!(!policy.decide(Path::new("dist")).included);
    // Native gitignore: vendor directory and build directory excluded.
    assert!(!policy.decide(Path::new("vendor/lib.js")).included);
    assert!(!policy.decide(Path::new("build/output.css")).included);
    // Package defaults apply without any gitignore.
    assert!(
        !policy
            .decide(Path::new("node_modules/pkg/index.js"))
            .included
    );
    assert!(!policy.decide(Path::new("Cargo.lock")).included);
    // Secrets are absolute: an explicit include cannot bypass them.
    assert!(!policy.decide(Path::new(".env")).included);
    assert!(
        !policy
            .decide(Path::new("secrets/credentials.json"))
            .included
    );
    // Known env templates are re-includable.
    assert!(policy.decide(Path::new(".env.example")).included);

    // Fingerprint is stable and changes with the policy.
    let fingerprint = policy.fingerprint();
    assert_eq!(fingerprint, policy.fingerprint());
    let other = EffectiveConfig::resolve_layers(
        None,
        None,
        &CliOverrides {
            preset: None,
            include: vec![],
            exclude: vec!["other.tmp".into()],
            max_file_bytes: None,
        },
    )
    .expect("policy builds");
    let other_policy = Policy::build(&other, &native).expect("policy builds");
    assert_ne!(fingerprint, other_policy.fingerprint());
}

#[test]
fn secret_content_scan_detects_common_shapes_without_logging() {
    let key = format!("AKIA{}", "A".repeat(16));
    assert_eq!(
        scan_secret_content(&format!("aws = {key}")),
        Some("secret-content")
    );
    let token = format!("ghp_{}", "a1".repeat(18));
    assert_eq!(
        scan_secret_content(&format!("token: {token}")),
        Some("secret-content")
    );
    assert_eq!(
        scan_secret_content("-----BEGIN RSA PRIVATE KEY-----\nxx"),
        Some("secret-content")
    );
    assert_eq!(scan_secret_content("nothing to see here"), None);
}

#[test]
fn config_precedence_local_over_committed_over_cli_defaults() {
    let root = temp_dir("config-precedence");
    write(
        &root.join("repoise.config.json"),
        r#"{"schemaVersion":1,"preset":"docs-only","include":["extra/**"],"maxFileBytes":5000}"#,
    );
    write(
        &root.join("repoise.local.json"),
        r#"{"preset":"hybrid","maxFileBytes":7000}"#,
    );
    let eff = EffectiveConfig::resolve(&root, &CliOverrides::default()).expect("resolve");
    assert_eq!(eff.preset, Preset::Hybrid); // local wins over committed
    assert_eq!(eff.max_file_bytes, 7000); // local wins over committed
    assert!(eff.include.iter().any(|(p, _)| p == "extra/**")); // committed retained

    // CLI overrides win over everything.
    let cli = CliOverrides {
        preset: Some(Preset::DocsOnly),
        include: Vec::new(),
        exclude: vec!["override.tmp".into()],
        max_file_bytes: Some(42),
    };
    let eff = EffectiveConfig::resolve(&root, &cli).expect("resolve");
    assert_eq!(eff.preset, Preset::DocsOnly);
    assert_eq!(eff.max_file_bytes, 42);
    assert!(eff.exclude.iter().any(|(p, _)| p == "override.tmp"));
}

#[test]
fn config_rejects_unknown_keys_and_wrong_schema_version() {
    let root = temp_dir("config-invalid");
    write(&root.join("repoise.config.json"), r#"{"bogus":1}"#);
    assert!(EffectiveConfig::resolve(&root, &CliOverrides::default()).is_err());

    write(&root.join("repoise.config.json"), r#"{"schemaVersion":99}"#);
    assert!(EffectiveConfig::resolve(&root, &CliOverrides::default()).is_err());
}

#[test]
fn embedding_endpoint_must_be_environment_reference() {
    let root = temp_dir("config-endpoint");
    write(
        &root.join("repoise.config.json"),
        r#"{"schemaVersion":1,"preset":"hybrid","embedding":{"provider":"openai","endpoint":"http://x","model":"m1"}}"#,
    );
    let eff = EffectiveConfig::resolve(&root, &CliOverrides::default()).expect("resolve");
    assert!(!eff.validate().is_empty());

    write(
        &root.join("repoise.config.json"),
        r#"{"schemaVersion":1,"preset":"hybrid","embedding":{"provider":"openai-compatible","endpoint":"env:OPENAI_ENDPOINT","model":"m1","dimensions":8,"apiKeyEnv":"OPENAI_API_KEY"}}"#,
    );
    let eff = EffectiveConfig::resolve(&root, &CliOverrides::default()).expect("resolve");
    assert!(eff.validate().is_empty());
}

#[test]
fn docs_only_preset_excludes_code() {
    let eff = EffectiveConfig::resolve_layers(
        None,
        None,
        &CliOverrides {
            preset: Some(Preset::DocsOnly),
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: None,
        },
    )
    .expect("policy builds");
    let policy = Policy::build(&eff, &[]).expect("policy builds");
    assert!(!policy.decide(Path::new("src/app.rs")).included);
    assert!(!policy.decide(Path::new("lib/util.py")).included);
    assert!(policy.decide(Path::new("docs/guide.md")).included);
}

#[test]
fn inventory_is_deterministic_and_applies_skips() {
    let mut tree: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    tree.insert("docs/a.md".into(), "hello world".as_bytes().to_vec());
    tree.insert(
        "bin/data.blob".into(),
        vec![0x89, 0x50, 0x4E, 0x47, 0, 1, 2, 3],
    );
    tree.insert("big.md".into(), vec![b'x'; 500]);
    tree.insert(
        "note.txt".into(),
        format!("key AKIA{} end", "A".repeat(16))
            .as_bytes()
            .to_vec(),
    );
    let mut revisions: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    revisions.insert("v1".into(), tree);
    let adapter = FakeRevisionAdapter::new("fake-root", revisions);
    let eff = EffectiveConfig::resolve_layers(
        None,
        None,
        &CliOverrides {
            preset: None,
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_bytes: Some(100),
        },
    )
    .unwrap();
    let first = inventory(&adapter, SnapshotMode::WorkingTree, &eff).unwrap();
    let second = inventory(&adapter, SnapshotMode::WorkingTree, &eff).unwrap();
    assert_eq!(first.manifest.manifest_hash, second.manifest.manifest_hash);
    assert!(first.files.iter().any(|f| f.path == Path::new("docs/a.md")));
    assert!(
        first
            .skips
            .iter()
            .any(|s| s.path == Path::new("bin/data.blob")
                && matches!(s.reason, SkipReason::BinaryEncoding))
    );
    assert!(
        first
            .skips
            .iter()
            .any(|s| s.path == Path::new("big.md") && matches!(s.reason, SkipReason::FileTooLarge))
    );
    assert!(
        first
            .skips
            .iter()
            .any(|s| s.path == Path::new("note.txt")
                && matches!(s.reason, SkipReason::SecretContent))
    );
    assert!(first.snapshot.dirty_overlay_digest.is_some());
    assert!(first.repository.repo_id.starts_with("repo-"));
    // Deterministic snapshot id for the same inputs.
    assert_eq!(first.snapshot.snapshot_id, second.snapshot.snapshot_id);
}

#[test]
fn filesystem_adapter_never_follows_symlinks_and_contains_reads() {
    let outside = temp_dir("fs-outside");
    write(&outside.join("x.txt"), "outside");
    let root = temp_dir("fs-adapter");
    write(&root.join("docs/a.md"), "hello");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("x.txt"), root.join("link.txt")).unwrap();
    let adapter = FilesystemAdapter::new(&root).unwrap();
    let revision = adapter.resolve(None, SnapshotMode::PlainDirectory).unwrap();
    let entries = adapter
        .enumerate(&revision, SnapshotMode::PlainDirectory)
        .unwrap();
    let paths: Vec<String> = entries
        .iter()
        .map(|e| repoise_core::adapter::to_posix(&e.path))
        .collect();
    assert!(paths.contains(&"docs/a.md".to_string()));
    #[cfg(unix)]
    assert!(!paths.contains(&"link.txt".to_string()));
    // Containment: a relative escape must fail.
    let result = adapter.read(
        &revision,
        SnapshotMode::PlainDirectory,
        Path::new("../fs-outside/x.txt"),
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

#[test]
fn git_adapter_committed_and_working_tree_separate_states() {
    let root = temp_dir("git-adapter");
    write(&root.join("file.txt"), "v1\n");
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["add", "file.txt"]);
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
    write(&root.join("file.txt"), "v2\n");
    let adapter = GitAdapter::new(&root).unwrap();
    let committed = adapter.resolve(None, SnapshotMode::Committed).unwrap();
    let working = adapter.resolve(None, SnapshotMode::WorkingTree).unwrap();
    assert_eq!(committed.id.as_str(), working.id.as_str());
    assert_eq!(committed.branch.as_deref(), Some("main"));

    let c_content = adapter
        .read(&committed, SnapshotMode::Committed, Path::new("file.txt"))
        .unwrap();
    let w_content = adapter
        .read(&working, SnapshotMode::WorkingTree, Path::new("file.txt"))
        .unwrap();
    assert_eq!(String::from_utf8(c_content).unwrap(), "v1\n");
    assert_eq!(String::from_utf8(w_content).unwrap(), "v2\n");

    // Committed snapshot excludes uncommitted files.
    write(&root.join("new.txt"), "n\n");
    let w2 = adapter
        .enumerate(&working, SnapshotMode::WorkingTree)
        .unwrap();
    assert!(w2.iter().any(|e| e.path == Path::new("new.txt")));
    let c2 = adapter
        .enumerate(&committed, SnapshotMode::Committed)
        .unwrap();
    assert!(!c2.iter().any(|e| e.path == Path::new("new.txt")));
}

#[test]
fn git_adapter_detached_head_reports_no_branch() {
    let root = temp_dir("git-detached");
    write(&root.join("file.txt"), "v1\n");
    run_git(&root, &["init", "-b", "main"]);
    run_git(&root, &["add", "file.txt"]);
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
    run_git(&root, &["checkout", "--detach"]);
    let adapter = GitAdapter::new(&root).unwrap();
    let revision = adapter.resolve(None, SnapshotMode::WorkingTree).unwrap();
    assert!(revision.branch.is_none());
}

#[test]
fn init_is_idempotent_nondestructive_and_preserves_managed_blocks() {
    let root = temp_dir("init-lifecycle");
    write(&root.join("README.md"), "# Title\n");
    let opts = InitOptions {
        preset: Preset::DocsCodeLexical,
        dry_run: true,
        yes: true,
        provider: None,
        adopt_managed_block: Some("README.md".into()),
        agents_snippet: false,
    };
    // Dry run never writes.
    let p = plan(&root, &opts).unwrap();
    assert!(
        p.files
            .iter()
            .all(|f| f.action == repoise_core::init::FileAction::Create)
    );
    assert!(!root.join(repoise_core::CONFIG_FILENAME).exists());

    // Apply creates config, overlay and the managed block.
    let opts = InitOptions {
        dry_run: false,
        ..opts
    };
    let p = plan(&root, &opts).unwrap();
    let outcome = apply(&p).unwrap();
    assert!(outcome.conflicts.is_empty());
    let readme = fs::read_to_string(root.join("README.md")).unwrap();
    assert!(readme.starts_with("# Title\n"));
    assert!(readme.contains("<!-- repoise:managed begin"));
    assert!(readme.ends_with("<!-- repoise:managed end -->\n"));

    // Second run is fully idempotent.
    let p = plan(&root, &opts).unwrap();
    assert!(
        p.files
            .iter()
            .all(|f| f.action == repoise_core::init::FileAction::Unchanged)
    );

    // User modifications are never overwritten; they are conflicts.
    write(&root.join(repoise_core::CONFIG_FILENAME), "{}\n");
    let p = plan(&root, &opts).unwrap();
    assert!(
        p.files
            .iter()
            .any(|f| f.action == repoise_core::init::FileAction::Conflict)
    );
    let outcome = apply(&p).unwrap();
    assert!(!outcome.conflicts.is_empty());
    assert_eq!(
        fs::read_to_string(root.join(repoise_core::CONFIG_FILENAME)).unwrap(),
        "{}\n"
    );

    // Hybrid preset requires an explicit provider.
    let root2 = temp_dir("init-hybrid");
    let bad = InitOptions {
        preset: Preset::Hybrid,
        dry_run: false,
        yes: true,
        provider: None,
        adopt_managed_block: None,
        agents_snippet: false,
    };
    assert!(plan(&root2, &bad).is_err());
    let good = InitOptions {
        preset: Preset::Hybrid,
        dry_run: false,
        yes: true,
        provider: Some("openai".into()),
        adopt_managed_block: None,
        agents_snippet: false,
    };
    let p = plan(&root2, &good).unwrap();
    apply(&p).unwrap();
    let config = fs::read_to_string(root2.join(repoise_core::CONFIG_FILENAME)).unwrap();
    assert!(config.contains("\"provider\""));
}
