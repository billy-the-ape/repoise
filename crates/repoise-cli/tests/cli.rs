use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args(args)
        .output()
        .expect("CLI should start")
}

fn temp_root(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("repoise-cli-test-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("README.md"), "hello\n").expect("write readme");
    dir
}

#[test]
fn greeting_is_successful() {
    let output = cli(&["greet"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Repoise says hello")
    );
}

#[test]
fn help_and_version_are_successful() {
    for flag in ["help", "--help", "-h"] {
        let output = cli(&[flag]);
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("USAGE:\n    repoise <COMMAND>")
        );
    }
    for flag in ["version", "--version", "-V"] {
        let output = cli(&[flag]);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("repoise {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn unsupported_input_fails_with_usage_on_stderr() {
    for args in [
        &["--unknown"][..],
        &["greet", "extra"],
        &["index", "a", "b"],
    ] {
        let output = cli(args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr).unwrap().contains("USAGE:"));
    }
}

#[test]
fn doctor_reports_effective_settings_for_a_plain_directory() {
    let root = temp_root("doctor");
    let output = cli(&["doctor", root.to_str().unwrap()]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("adapter: Filesystem"));
    assert!(text.contains("preset: docs-code-lexical"));
    assert!(text.contains("policy fingerprint:"));
}

#[test]
fn explain_reports_decisions_for_paths() {
    let root = temp_root("explain");
    let ok = cli(&["explain", "--path", "README.md", root.to_str().unwrap()]);
    assert!(ok.status.success());
    assert!(
        String::from_utf8(ok.stdout)
            .unwrap()
            .contains("decision: included")
    );

    let denied = cli(&["explain", "--path", ".env", root.to_str().unwrap()]);
    assert!(denied.status.success());
    let text = String::from_utf8(denied.stdout).unwrap();
    assert!(text.contains("decision: excluded"));
    assert!(text.contains("secret-deny"));
}

#[test]
fn index_lists_manifest_files_and_skips() {
    let root = temp_root("index");
    let output = cli(&["index", root.to_str().unwrap()]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("manifest:"));
    assert!(text.contains("files: 1"));
}

#[test]
fn init_is_non_interactive_and_idempotent() {
    let root = temp_root("init");
    let first = cli(&[
        "init",
        "--preset",
        "docs-only",
        "--adopt-managed-block",
        "README.md",
        root.to_str().unwrap(),
    ]);
    assert!(first.status.success());
    let text = String::from_utf8(first.stdout).unwrap();
    assert!(text.contains("created: repoise.config.json"));
    assert!(text.contains("created: README.md"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    assert!(readme.starts_with("hello\n"));
    assert!(readme.contains("<!-- repoise:managed begin"));

    let second = cli(&[
        "init",
        "--preset",
        "docs-only",
        "--adopt-managed-block",
        "README.md",
        root.to_str().unwrap(),
    ]);
    assert!(second.status.success());
    assert!(
        String::from_utf8(second.stdout)
            .unwrap()
            .contains("unchanged: repoise.config.json")
    );
}

#[test]
fn status_exit_codes_and_freshness_after_index() {
    let root = temp_root("status");
    // No index yet: exit 3, freshness unknown.
    let before = cli(&["status", root.to_str().unwrap()]);
    assert_eq!(before.status.code(), Some(3));
    assert!(
        String::from_utf8(before.stdout)
            .unwrap()
            .contains("freshness: unknown")
    );
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());
    let after = cli(&["status", root.to_str().unwrap()]);
    assert!(after.status.success());
    let text = String::from_utf8(after.stdout).unwrap();
    assert!(text.contains("freshness: fresh"));
    assert!(text.contains("generation"));
    // Content changes after the build: stale (exit 3).
    std::fs::write(root.join("README.md"), "hello again\n").unwrap();
    let stale = cli(&["status", root.to_str().unwrap()]);
    assert_eq!(stale.status.code(), Some(3));
    assert!(
        String::from_utf8(stale.stdout)
            .unwrap()
            .contains("freshness: stale")
    );
}

#[test]
fn search_and_read_round_trip_with_json() {
    let root = temp_root("search");
    // Search before any index: exit 3.
    let before = cli(&["search", "--query", "hello", root.to_str().unwrap()]);
    assert_eq!(before.status.code(), Some(3));
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());
    let output = cli(&[
        "search",
        "--query",
        "hello",
        "--json",
        root.to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let results = response["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    let source_id = results[0]["source_id"].as_str().unwrap().to_string();
    let hit = &results[0];
    assert_eq!(hit["path"].as_str().unwrap(), "README.md");
    assert_eq!(hit["line_start"], 1);
    let read = cli(&[
        "read",
        "--source-id",
        &source_id,
        "--json",
        root.to_str().unwrap(),
    ]);
    assert!(read.status.success());
    let result: serde_json::Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(result["path"].as_str().unwrap(), "README.md");
    assert_eq!(result["text"].as_str().unwrap(), "hello");
    // A source id from a stale generation fails as an operational error.
    std::fs::write(root.join("README.md"), "changed\n").unwrap();
    let stale_read = cli(&["read", "--source-id", &source_id, root.to_str().unwrap()]);
    assert_eq!(stale_read.status.code(), Some(1));
    assert!(
        String::from_utf8(stale_read.stderr)
            .unwrap()
            .contains("stale source")
    );
}

#[test]
fn purge_requires_scope_or_all_and_removes_cache_only() {
    let root = temp_root("purge");
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());
    let bin = env!("CARGO_BIN_EXE_repoise");
    // Neither --all nor a scope: an operational error, nothing removed.
    let bad = Command::new(bin)
        .args(["purge"])
        .current_dir(&root)
        .output()
        .expect("CLI should start");
    assert_eq!(bad.status.code(), Some(1));
    assert!(root.join(".repoise").exists());
    // Invalid scope ids are rejected before anything is removed.
    let invalid = Command::new(bin)
        .args(["purge", "--repo-id", "../..", "--worktree-id", "x"])
        .current_dir(&root)
        .output()
        .expect("CLI should start");
    assert_eq!(invalid.status.code(), Some(1));
    assert!(root.join(".repoise").exists());
    // --all removes the generated cache root; sources survive.
    let all = Command::new(bin)
        .args(["purge", "--all"])
        .current_dir(&root)
        .output()
        .expect("CLI should start");
    assert!(all.status.success());
    // The generated layout is removed (the empty root directory may remain).
    assert!(!root.join(".repoise").join("repos").exists());
    assert!(root.join("README.md").exists());
}
#[test]
fn check_exit_codes_and_json_category() {
    let root = temp_root("check");
    // No index: exit code 3 (missing), machine-readable category.
    let missing = cli(&["check", "--json", root.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(3));
    let value: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("check --json is valid JSON");
    assert_eq!(value["category"], "missing");

    // Fresh index: exit code 0.
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());
    let fresh = cli(&["check", root.to_str().unwrap()]);
    assert!(fresh.status.success());
    assert!(
        String::from_utf8(fresh.stdout)
            .unwrap()
            .contains("check: Ok")
    );

    // Live change: exit code 3 (stale).
    std::fs::write(root.join("README.md"), "hello again\n").unwrap();
    let stale = cli(&["check", root.to_str().unwrap()]);
    assert_eq!(stale.status.code(), Some(3));
    assert!(
        String::from_utf8(stale.stdout)
            .unwrap()
            .contains("check: Stale")
    );
}

#[test]
fn related_requires_source_id_and_reports_related_sources() {
    let root = temp_root("related");
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());
    let found = cli(&[
        "search",
        "--json",
        "--query",
        "hello",
        root.to_str().unwrap(),
    ]);
    let value: serde_json::Value = serde_json::from_slice(&found.stdout).unwrap();
    let source_id = value["results"][0]["source_id"]
        .as_str()
        .unwrap()
        .to_string();

    let related = cli(&[
        "related",
        "--json",
        "--source-id",
        &source_id,
        root.to_str().unwrap(),
    ]);
    assert!(related.status.success());
    let value: serde_json::Value = serde_json::from_slice(&related.stdout).unwrap();
    assert_eq!(value["source"]["source_id"], source_id);
    assert!(value["results"].is_array());

    // Missing source id is an operational error; unknown kinds are rejected.
    let missing_id = cli(&["related", root.to_str().unwrap()]);
    assert_eq!(missing_id.status.code(), Some(1));
    let bad_kind = cli(&[
        "related",
        "--source-id",
        &source_id,
        "--relation",
        "bogus",
        root.to_str().unwrap(),
    ]);
    assert_eq!(bad_kind.status.code(), Some(1));
}

#[test]
fn overlay_uninstall_and_update_commands() {
    let root = temp_root("overlay");
    let init = cli(&[
        "init",
        "--adopt-managed-block",
        "README.md",
        "--agents-snippet",
        root.to_str().unwrap(),
    ]);
    assert!(init.status.success());
    assert!(root.join("repoise.overlay.json").exists());

    // Dry run removes nothing.
    let dry = cli(&["overlay", "uninstall", "--dry-run", root.to_str().unwrap()]);
    assert!(dry.status.success());
    assert!(root.join("repoise.overlay.json").exists());
    assert!(
        String::from_utf8(dry.stdout)
            .unwrap()
            .contains("dry run: nothing written")
    );

    // Real uninstall: manifest and config go, README body survives.
    let done = cli(&["overlay", "uninstall", root.to_str().unwrap()]);
    assert!(done.status.success());
    assert!(!root.join("repoise.overlay.json").exists());
    assert!(!root.join("repoise.config.json").exists());
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    assert!(readme.starts_with("hello\n"));
    assert!(!readme.contains("repoise:managed"));

    // Re-init, then update with no template change: a no-op success.
    assert!(
        cli(&[
            "init",
            "--adopt-managed-block",
            "README.md",
            root.to_str().unwrap()
        ])
        .status
        .success()
    );
    let update = cli(&["overlay", "update", root.to_str().unwrap()]);
    assert!(update.status.success());
    assert!(root.join("repoise.overlay.json").exists());

    // A missing action is a usage error.
    let no_action = cli(&["overlay", root.to_str().unwrap()]);
    assert_eq!(no_action.status.code(), Some(2));
}
