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
fn doctor_reports_selected_config_paths_for_external_overrides() {
    let root = temp_root("doctor-external-config");
    // External config/local-config files outside the repository.
    let external = std::env::temp_dir().join(format!(
        "repoise-cli-ext-{}-{}",
        std::process::id(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&external);
    std::fs::create_dir_all(&external).unwrap();
    let external_config = external.join("ext.repoise.config.json");
    let external_local = external.join("ext.repoise.local.json");
    let missing_local = external.join("absent.repoise.local.json");
    std::fs::write(
        &external_config,
        r#"{"schemaVersion":1,"preset":"docs-only","inheritGitIgnores":true}"#,
    )
    .unwrap();
    std::fs::write(
        &external_local,
        r#"{"schemaVersion":1,"inheritGitIgnores":false}"#,
    )
    .unwrap();
    let root_str = root.to_str().unwrap();

    // Defaults (no overrides): the repository config paths are reported, absent.
    let canonical_root = root.canonicalize().unwrap();
    let default_json = cli(&["doctor", "--json", root_str]);
    assert!(default_json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&default_json.stdout).unwrap();
    assert_eq!(
        value["config_file"],
        canonical_root
            .join("repoise.config.json")
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(value["config_file_exists"], false);
    assert_eq!(
        value["local_config_file"],
        canonical_root
            .join("repoise.local.json")
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(value["local_config_file_exists"], false);

    // External overrides: the exact selected paths and their existence are
    // reported, and the effective config reflects them.
    let override_json = cli(&[
        "doctor",
        "--json",
        "--config",
        external_config.to_str().unwrap(),
        "--local-config",
        external_local.to_str().unwrap(),
        root_str,
    ]);
    assert!(override_json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&override_json.stdout).unwrap();
    assert_eq!(
        value["config_file"],
        external_config.to_string_lossy().into_owned()
    );
    assert_eq!(value["config_file_exists"], true);
    assert_eq!(
        value["local_config_file"],
        external_local.to_string_lossy().into_owned()
    );
    assert_eq!(value["local_config_file_exists"], true);
    // The external config actually drove the effective preset.
    assert_eq!(value["effective"]["preset"], "docs-only");

    // An external local path that does not exist is still reported.
    let missing_json = cli(&[
        "doctor",
        "--json",
        "--config",
        external_config.to_str().unwrap(),
        "--local-config",
        missing_local.to_str().unwrap(),
        root_str,
    ]);
    assert!(missing_json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&missing_json.stdout).unwrap();
    assert_eq!(
        value["local_config_file"],
        missing_local.to_string_lossy().into_owned()
    );
    assert_eq!(value["local_config_file_exists"], false);

    // Human output names the selected paths too.
    let human = cli(&[
        "doctor",
        "--config",
        external_config.to_str().unwrap(),
        "--local-config",
        external_local.to_str().unwrap(),
        root_str,
    ]);
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains(&format!("config: {} (present)", external_config.display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("local: {} (present)", external_local.display())),
        "{text}"
    );
}

#[test]
fn doctor_reports_committed_mode_for_git_roots() {
    let root = std::env::temp_dir().join(format!("repoise-cli-doctor-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["add", "README.md"]);
    git(&["commit", "--quiet", "-m", "initial"]);
    let root_str = root.to_str().unwrap();

    let default = cli(&["doctor", "--json", root_str]);
    assert!(default.status.success());
    let value: serde_json::Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(value["mode"], "working_tree");

    let committed = cli(&["doctor", "--json", "--committed", root_str]);
    assert!(committed.status.success());
    let value: serde_json::Value = serde_json::from_slice(&committed.stdout).unwrap();
    assert_eq!(value["mode"], "committed");

    // Human output reports the same selection.
    let human = cli(&["doctor", "--committed", root_str]);
    assert!(human.status.success());
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .contains("mode: Committed"),
        "human doctor output must name the committed mode"
    );
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

    // Live change: exit code 3 (stale) with --fresh.
    std::fs::write(root.join("README.md"), "hello again\n").unwrap();
    let stale = cli(&["check", "--fresh", root.to_str().unwrap()]);
    assert_eq!(stale.status.code(), Some(3));
    assert!(
        String::from_utf8(stale.stdout)
            .unwrap()
            .contains("check: Stale")
    );

    // Without --fresh the stale index is reported but not a failure.
    let stale_soft = cli(&["check", "--json", root.to_str().unwrap()]);
    assert!(stale_soft.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&stale_soft.stdout).expect("check --json is valid JSON");
    assert_eq!(value["category"], "ok");
    assert_eq!(value["fresh"], false);
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

#[test]
fn index_shows_progress_and_final_summary_on_stdout() {
    let root = temp_root("index-progress");
    let output = cli(&["index", root.to_str().unwrap()]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Immediate start feedback names the scope and the first phase, before
    // any work finishes.
    assert!(
        stdout.contains(": discovery started"),
        "index must announce the start phase: {stdout}"
    );
    // Phase progress lines with counts and elapsed time; final summary.
    assert!(stdout.contains("index: reconcile "));
    assert!(stdout.contains("index: publication "));
    assert!(stdout.contains("files: 1 (reused 0, reparsed 1, skipped 0)"));
    assert!(stdout.contains("elapsed: "));
    // No per-file path noise in the output.
    assert!(!stdout.contains("README.md:"));
    // Nothing was routed to stderr.
    assert!(String::from_utf8(output.stderr).unwrap().is_empty());
}

#[test]
fn index_json_keeps_stdout_single_json_and_progress_on_stderr() {
    let root = temp_root("index-json-progress");
    let output = cli(&["index", "--json", root.to_str().unwrap()]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Stdout is exactly one final JSON document.
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("JSON mode stdout must parse as one JSON value");
    assert_eq!(value["files_indexed"].as_u64(), Some(1));
    assert_eq!(value["files_reparsed"].as_u64(), Some(1));
    assert_eq!(value["files_reused"].as_u64(), Some(0));
    // Progress goes to stderr instead, keeping stdout machine-readable.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(": discovery started"));
    assert!(stderr.contains("index: reconcile "));
}

#[test]
fn index_json_incremental_reports_reuse_in_json_and_progress() {
    let root = temp_root("index-json-incremental");
    let first = cli(&["index", "--json", root.to_str().unwrap()]);
    assert!(first.status.success());
    let first_value: serde_json::Value =
        serde_json::from_str(&String::from_utf8(first.stdout).unwrap()).unwrap();
    assert_eq!(first_value["files_reparsed"].as_u64(), Some(1));

    // Unchanged second build: JSON carries the reuse numbers.
    let second = cli(&["index", "--json", root.to_str().unwrap()]);
    assert!(second.status.success());
    let second_value: serde_json::Value =
        serde_json::from_str(&String::from_utf8(second.stdout).unwrap()).unwrap();
    assert_eq!(second_value["files_reparsed"].as_u64(), Some(0));
    assert_eq!(second_value["files_reused"].as_u64(), Some(1));
    // Progress still ran on stderr and shows the file as fully covered
    // without claiming a reparse.
    let stderr = String::from_utf8(second.stderr).unwrap();
    assert!(stderr.contains(": discovery started"));
    assert!(
        stderr.contains("index: reconcile 1/1 files (100%)"),
        "incremental reconcile must cover the file without reparsing: {stderr}"
    );
}

/// Parses machine stdout as NDJSON envelopes (one JSON value per line).
fn ndjson_events(stdout: &[u8]) -> Vec<serde_json::Value> {
    std::str::from_utf8(stdout)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str(line).expect("each machine stdout line must be a JSON envelope")
        })
        .collect()
}

/// Sends `signal` (e.g. `"INT"`, `"TERM"`) to `pid` with the POSIX `kill`.
#[cfg(unix)]
fn kill(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} {pid} must succeed");
}

fn terminal_events(events: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    events
        .iter()
        .filter(|event| {
            ["completed", "cancelled", "failed"].contains(&event["type"].as_str().unwrap())
        })
        .collect()
}

#[test]
fn capabilities_json_is_a_stable_handshake() {
    let output = cli(&["capabilities", "--json"]);
    assert!(output.status.success());
    let doc: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(doc["schemaVersion"], 1);
    assert_eq!(doc["name"], "repoise");
    assert_eq!(doc["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["eventStream"]["version"], 1);
    let schema = doc["eventStream"]["schema"].as_str().unwrap();
    assert_eq!(schema, "schemas/event-stream.v1.schema.json");
    // The schema ships at the workspace root, two levels above this crate.
    let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(schema);
    assert!(
        schema_path.exists(),
        "the advertised event-stream schema must be checked in"
    );
    // The command matrix advertises the NDJSON machine mode for exactly the
    // streaming operations.
    let commands = doc["commands"].as_array().unwrap();
    let modes = |name: &str| {
        commands
            .iter()
            .find(|command| command["name"] == name)
            .expect("command is listed")["machine"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(modes("index"), vec!["json", "events"]);
    assert_eq!(modes("watch"), vec!["json", "events"]);
    assert_eq!(modes("search"), vec!["json"]);
    assert_eq!(modes("mcp"), vec!["mcp-stdio"]);
    // The exit-code table stays stable for automation.
    assert_eq!(doc["exitCodes"]["ok"], 0);
    assert_eq!(doc["exitCodes"]["operational"], 1);
    assert_eq!(doc["exitCodes"]["usage"], 2);
    assert_eq!(doc["exitCodes"]["indexMissingOrStale"], 3);
    assert_eq!(doc["exitCodes"]["coverageNotMet"], 4);
    assert_eq!(doc["exitCodes"]["benchPreflight"], 3);
    assert_eq!(doc["exitCodes"]["benchInterrupted"], 4);
    // The benchmark coordinator is available; the Node adapter host is
    // optional (only repoise-condition trials require it).
    assert_eq!(doc["benchmark"]["supported"], true);
    assert_eq!(doc["benchmark"]["command"], "bench");
    assert_eq!(doc["benchmark"]["docs"], "docs/bench.md");
    assert_eq!(modes("bench"), vec!["json"]);
}

#[test]
fn index_events_streams_ndjson_with_one_terminal() {
    let root = temp_root("index-events");
    let canonical = std::fs::canonicalize(&root).unwrap();
    let output = cli(&["index", "--events", root.to_str().unwrap()]);
    assert!(output.status.success());
    let events = ndjson_events(&output.stdout);
    assert!(!events.is_empty());
    // Envelope contract: required fields, one run id, sequences from zero.
    let run_id = events[0]["runId"].as_str().unwrap();
    for (position, event) in events.iter().enumerate() {
        for field in [
            "schemaVersion",
            "runId",
            "sequence",
            "timestamp",
            "type",
            "payload",
        ] {
            assert!(
                event.get(field).is_some(),
                "event {position} misses {field}: {event}"
            );
        }
        assert_eq!(event["schemaVersion"], 1);
        assert_eq!(event["runId"], run_id);
        assert_eq!(event["sequence"], position);
    }
    assert_eq!(events[0]["type"], "started");
    assert_eq!(events[0]["payload"]["operation"], "index");
    assert_eq!(
        events[0]["payload"]["scope"]["root"],
        canonical.to_str().unwrap()
    );
    assert_eq!(
        events[0]["payload"]["scope"]["snapshotMode"],
        "plain-directory"
    );
    // Progress events carry phase-scoped counts and units.
    let progress: Vec<&serde_json::Value> = events
        .iter()
        .filter(|event| event["type"] == "progress")
        .collect();
    assert!(!progress.is_empty(), "expected progress events: {events:?}");
    for event in progress {
        assert!(event["payload"]["phase"].is_string());
        assert!(event["payload"]["unit"].is_string());
        assert!(event["payload"]["done"].is_u64());
        assert!(event["payload"]["elapsedMs"].is_u64());
    }
    // Exactly one terminal event, and it is the last line.
    assert_eq!(terminal_events(&events).len(), 1);
    assert_eq!(events.last().unwrap()["type"], "completed");
    assert_eq!(
        events.last().unwrap()["payload"]["result"]["files_indexed"],
        1
    );
    // Machine stdout carries protocol records only; stderr has no envelopes.
    for line in String::from_utf8(output.stderr).unwrap().lines() {
        assert!(
            !line.trim_start().starts_with('{'),
            "an envelope leaked to stderr: {line}"
        );
    }
}

/// A configured embedding whose endpoint env var holds a non-http(s) URL
/// fails session setup; the stream must still end with a `failed` terminal
/// (exit 1), never just stop after `started`.
#[test]
fn index_events_setup_failure_emits_a_failed_terminal() {
    let root = temp_root("index-events-failed");
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    std::fs::write(
        root.join("repoise.config.json"),
        r#"{"embedding":{"provider":"openai-compatible","model":"test-model","dimensions":3,"endpoint":"env:REPOISE_TEST_EMBED_ENDPOINT","apiKeyEnv":"REPOISE_TEST_EMBED_KEY"}}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args(["index", "--events", root.to_str().unwrap()])
        .env("REPOISE_TEST_EMBED_ENDPOINT", "not-a-url")
        .env("REPOISE_TEST_EMBED_KEY", "test-key")
        .output()
        .expect("run index");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a session setup failure must exit 1"
    );
    let events = ndjson_events(&output.stdout);
    assert_eq!(events[0]["type"], "started");
    assert_eq!(
        terminal_events(&events).len(),
        1,
        "exactly one terminal event: {events:?}"
    );
    let failed = events.last().unwrap();
    assert_eq!(failed["type"], "failed");
    assert_eq!(failed["payload"]["code"], "operational");
    assert!(
        failed["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("http(s)")
    );
}

#[cfg(unix)]
#[test]
fn watch_events_cancels_with_a_terminal_event_on_sigint() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = temp_root("watch-events");
    // Prime the scope so the watch loop starts idle (no first publish).
    let primed = cli(&["index", "--json", root.to_str().unwrap()]);
    assert!(primed.status.success());

    let mut child = Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args([
            "watch",
            "--events",
            "--interval",
            "200",
            root.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("watch should start");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the started event");
    let started: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(started["type"], "started");
    assert_eq!(started["payload"]["operation"], "watch");

    kill(child.id(), "INT");
    let status = child.wait().expect("wait for watch to exit");
    assert!(status.success(), "SIGINT must stop watch with exit code 0");

    let mut events = vec![started];
    for result in reader.lines() {
        let line = result.expect("read the remaining events");
        events.push(serde_json::from_str(&line).unwrap());
    }
    assert_eq!(terminal_events(&events).len(), 1);
    assert_eq!(events.last().unwrap()["type"], "cancelled");
    assert_eq!(events.last().unwrap()["payload"]["reason"], "signal");
}

/// A config broken while `watch --events` is running yields a sanitized
/// `warning` event with `kind: "invalid"`; fixing the config applies the
/// revision as a config-driven generation on the same running process.
#[cfg(unix)]
#[test]
fn watch_events_reports_config_issue_and_recovers() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = temp_root("watch-config-events");
    std::fs::create_dir_all(root.join("private")).unwrap();
    std::fs::write(root.join("private/notes.md"), "pangolin quokka\n").unwrap();
    let primed = cli(&["index", "--json", root.to_str().unwrap()]);
    assert!(primed.status.success());

    let mut child = Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args([
            "watch",
            "--events",
            "--interval",
            "100",
            root.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("watch should start");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the started event");
    let started: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(started["type"], "started");

    // Break the config while the watch is running.
    std::fs::write(
        root.join("repoise.config.json"),
        r#"{"schemaVersion":1,"exclude":["private""#,
    )
    .unwrap();
    // Read until the sanitized config warning arrives.
    let warning = loop {
        line.clear();
        reader
            .read_line(&mut line)
            .expect("the watch stream stays open");
        let event: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        if event["type"] == "warning" && event["payload"]["kind"] == "invalid" {
            break event;
        }
    };
    assert!(
        warning["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("repoise.config.json"),
        "the warning names the broken file: {}",
        warning["payload"]["message"]
    );

    // Fix the config: the same process applies the revision as a
    // config-driven generation.
    std::fs::write(
        root.join("repoise.config.json"),
        r#"{"schemaVersion":1,"exclude":["private"]}"#,
    )
    .unwrap();
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .expect("the watch stream stays open");
        let event: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        if event["type"] == "progress"
            && event["payload"]["phase"] == "publication"
            && event["payload"]["configChange"] == true
        {
            break;
        }
    }

    kill(child.id(), "INT");
    assert!(child.wait().expect("wait for watch to exit").success());
    let mut events = vec![started, warning];
    for result in reader.lines() {
        let event_line = result.expect("read the remaining events");
        events.push(serde_json::from_str(&event_line).unwrap());
    }
    assert_eq!(events.last().unwrap()["type"], "cancelled");
}

#[cfg(unix)]
#[test]
fn status_reports_the_active_watch_owner_and_clears_it_after_exit() {
    use std::process::Stdio;

    let root = temp_root("status-owner");
    // Prime the scope so the status probes open an existing database rather
    // than racing watch over the creation of a fresh one.
    let primed = cli(&["index", "--json", root.to_str().unwrap()]);
    assert!(primed.status.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args(["watch", "--interval", "200", root.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("watch should start");
    // Poll until the watch loop has published its owner marker. Status
    // exits 3 while the index is missing, but still prints its JSON.
    let mut owner = None;
    for _ in 0..50 {
        let probe = cli(&["status", "--json", root.to_str().unwrap()]);
        match probe.status.code() {
            Some(0) | Some(3) => {}
            other => panic!(
                "unexpected status exit code: {other:?}; stderr: {}",
                String::from_utf8(probe.stderr).unwrap_or_default()
            ),
        }
        let doc: serde_json::Value =
            serde_json::from_str(&String::from_utf8(probe.stdout).unwrap()).unwrap();
        if doc["owner"]["state"] == "active" && doc["owner"]["operation"] == "watch" {
            owner = Some(doc);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let doc = owner.expect("watch should own the scope while running");
    assert_eq!(doc["owner"]["state"], "active");

    kill(child.id(), "TERM");
    assert!(child.wait().unwrap().success());

    let cleared = cli(&["status", "--json", root.to_str().unwrap()]);
    let cleared: serde_json::Value =
        serde_json::from_str(&String::from_utf8(cleared.stdout).unwrap()).unwrap();
    assert!(
        cleared["owner"].is_null(),
        "the owner marker must be removed on clean exit: {cleared}"
    );
}

fn git(root: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn status_and_check_separate_freshness_from_git_dirtiness() {
    let root = std::env::temp_dir().join(format!("repoise-cli-test-dirty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp dir");
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.email", "test@example.com"]);
    git(&root, &["config", "user.name", "Test"]);
    // Keep the in-repo cache out of the dirty-tree count (local policy).
    std::fs::write(
        root.join(".git").join("info").join("exclude"),
        ".repoise/\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    git(&root, &["add", "README.md"]);
    git(&root, &["commit", "-q", "-m", "Add readme"]);

    // Index, then save an uncommitted edit and index the saved contents.
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());
    std::fs::write(root.join("README.md"), "hello again\n").unwrap();
    assert!(cli(&["index", root.to_str().unwrap()]).status.success());

    // Fresh with the Git dirtiness still visible: exit 0.
    let status = cli(&["status", root.to_str().unwrap()]);
    assert!(status.status.success());
    let text = String::from_utf8(status.stdout).unwrap();
    assert!(text.contains("freshness: fresh"), "{text}");
    assert!(text.contains("dirty: 1"), "{text}");

    // check --fresh passes on the dirty, indexed tree.
    let fresh = cli(&["check", "--fresh", root.to_str().unwrap()]);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    assert!(
        String::from_utf8(fresh.stdout)
            .unwrap()
            .contains("check: Ok")
    );

    // Edit again without indexing: stale (exit 3), dirtiness still reported.
    std::fs::write(root.join("README.md"), "hello third\n").unwrap();
    let stale = cli(&["status", root.to_str().unwrap()]);
    assert_eq!(stale.status.code(), Some(3));
    let stale_text = String::from_utf8(stale.stdout).unwrap();
    assert!(stale_text.contains("freshness: stale"), "{stale_text}");
    assert!(stale_text.contains("dirty: 1"), "{stale_text}");
    let stale_check = cli(&["check", "--fresh", root.to_str().unwrap()]);
    assert_eq!(stale_check.status.code(), Some(3));
}
