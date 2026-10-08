//! End-to-end history-lane behavior (card K5): local collection, FTS search
//! semantics, path GLOB filters, untrusted-path handling, merge commits,
//! root-commit hunks, horizon gaps and lane read/unknown-lane guards.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args(args)
        .output()
        .expect("CLI should start")
}

fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs git with piped stdin and returns trimmed stdout (fixtures that
/// must feed content to a git plumbing command such as `hash-object`).
fn git_stdin(args: &[&str], dir: &Path, stdin: &str) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("git should start");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("git should finish");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Runs git and returns its trimmed stdout (test fixtures need revisions).
fn git_capture(args: &[&str], dir: &Path) -> String {
    let out = git_capture_unchecked(args, dir);
    String::from_utf8(out).unwrap().trim().to_string()
}

fn git_capture_unchecked(args: &[&str], dir: &Path) -> Vec<u8> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Sets up a temp directory as a git repository with the local policy pieces
/// every fixture needs (identity, in-repo cache exclusion, config file).
fn init_git_repo(name: &str, config: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("repoise-cli-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    git(&["init", "-q", "-b", "main"], &dir);
    git(&["config", "user.email", "test@example.com"], &dir);
    git(&["config", "user.name", "Test"], &dir);
    // Keep the in-repo cache out of the dirty-tree count (local policy).
    std::fs::write(dir.join(".git").join("info").join("exclude"), ".repoise/\n").unwrap();
    std::fs::write(dir.join("repoise.config.json"), config).expect("write config");
    git(&["add", "repoise.config.json"], &dir);
    git(&["commit", "-q", "-m", "Add repoise config"], &dir);
    dir
}

/// One temp git repository with a four-commit first-parent mainline:
/// root, a PR-hint commit, a commit with a unicode path and a file named
/// like a 40-hex-digit sha, and a no-ff merge.
fn temp_git_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("repoise-cli-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    git(&["init", "-q", "-b", "main"], &dir);
    git(&["config", "user.email", "test@example.com"], &dir);
    git(&["config", "user.name", "Test"], &dir);
    // Keep the in-repo cache out of the dirty-tree count (local policy).
    std::fs::write(dir.join(".git").join("info").join("exclude"), ".repoise/\n").unwrap();

    // Enable the lane with a tight horizon (the mainline has 4 commits) and
    // bounded hunk descriptors.
    std::fs::write(
        dir.join("repoise.config.json"),
        r#"{"history":{"enabled":true,"horizon":3,"diffHunks":true}}"#,
    )
    .expect("write config");
    // Commit the config so the working tree stays clean (status freshness).
    git(&["add", "repoise.config.json"], &dir);
    git(&["commit", "-q", "-m", "Add repoise config"], &dir);

    std::fs::create_dir_all(dir.join("docs")).expect("docs dir");
    std::fs::create_dir_all(dir.join("src")).expect("src dir");
    std::fs::write(dir.join("docs").join("guide.md"), "getting started guide\n").unwrap();
    git(&["add", "-A"], &dir);
    git(&["commit", "-q", "-m", "Add guide doc"], &dir);

    std::fs::write(dir.join("src").join("parser.rs"), "fn parser() {}\n").unwrap();
    git(&["add", "-A"], &dir);
    git(&["commit", "-q", "-m", "Fix parser budget (#12)"], &dir);

    std::fs::write(dir.join("docs").join("日本語.md"), "unicode fixture\n").unwrap();
    // A file named exactly like a 40-hex-digit sha: must not be confused
    // with framing or record boundaries.
    std::fs::write(
        dir.join("0123456789abcdef0123456789abcdef01234567"),
        "looks like a sha\n",
    )
    .unwrap();
    git(&["add", "-A"], &dir);
    git(&["commit", "-q", "-m", "Add unicode fixture"], &dir);

    git(&["checkout", "-q", "-b", "side"], &dir);
    std::fs::write(dir.join("src").join("feature.rs"), "fn feature() {}\n").unwrap();
    git(&["add", "-A"], &dir);
    git(&["commit", "-q", "-m", "Add feature branch work"], &dir);
    git(&["checkout", "-q", "main"], &dir);
    git(
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge feature branch (#7)",
            "side",
        ],
        &dir,
    );

    dir
}

#[test]
fn history_lane_records_reports_gaps_and_serves_queries() {
    let root = temp_git_repo("history");
    let root = root.to_str().unwrap();

    let out = cli(&["index", root]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("history: 3 items"), "index output: {text}");
    // The mainline has 4 commits; horizon 3 records an explicit gap.
    assert!(text.contains("Horizon"), "expected a horizon gap; {text}");

    let out = cli(&["status", root]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("history lane: on"), "status output: {text}");

    // A two-word query must require both words in the same record (the
    // per-field OR groups must be parenthesized). No recorded commit has
    // both "unicode" and "feature".
    let out = cli(&[
        "search",
        "--query",
        "unicode feature",
        "--lane",
        "history",
        root,
    ]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with("0 result(s)"),
        "expected no matches; {text}"
    );

    // A multi-word query matching one commit's message.
    let out = cli(&[
        "search",
        "--query",
        "parser budget",
        "--lane",
        "history",
        root,
    ]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("1 result(s)"), "{text}");
    assert!(text.contains("src/parser.rs"), "{text}");

    // The GLOB path filter matches recorded paths individually.
    let out = cli(&[
        "search",
        "--query",
        "fix",
        "--lane",
        "history",
        "--path-filter",
        "src/*",
        root,
    ]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("1 result(s)"), "{text}");
    assert!(text.contains("src/parser.rs"), "{text}");

    // The 40-hex-digit filename and the unicode path must survive
    // collection without corrupting record framing.
    let out = cli(&["search", "--query", "fixture", "--lane", "history", root]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("1 result(s)"), "{text}");
    assert!(
        text.contains("0123456789abcdef0123456789abcdef01234567"),
        "{text}"
    );
    assert!(text.contains("日本語"), "{text}");

    // The merge commit sits on the first-parent mainline and gets hunk
    // descriptors from a tree diff against its first parent.
    let out = cli(&[
        "search", "--query", "merge", "--lane", "history", "--json", root,
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["lane"]["enabled"], true);
    assert_eq!(value["lane"]["items"], 3);
    let results = value["results"].as_array().expect("results array");
    let merge = results
        .iter()
        .find(|hit| {
            hit["message"]
                .as_str()
                .unwrap_or("")
                .contains("Merge feature branch")
        })
        .expect("merge commit recorded");
    let hunks = merge["hunks"].as_array().expect("hunks array");
    assert!(
        !hunks.is_empty(),
        "merge commit should have tree-diff hunks"
    );
    assert!(
        hunks
            .iter()
            .any(|hunk| hunk["path"].as_str() == Some("src/feature.rs")),
        "expected a hunk for src/feature.rs: {hunks:?}"
    );
}

#[test]
fn root_commit_hunks_cover_only_its_own_files() {
    // The root commit (config) sits inside the horizon here; later commits
    // touch other files, so any working-tree leak in the root diff would
    // name files the root never touched.
    let dir = init_git_repo(
        "root-hunks",
        r#"{"history":{"enabled":true,"horizon":10,"diffHunks":true}}"#,
    );
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src").join("b.rs"), "fn b() {}\n").unwrap();
    git(&["add", "-A"], &dir);
    git(&["commit", "-q", "-m", "Second commit"], &dir);
    let root = dir.to_str().unwrap();

    let out = cli(&["index", root]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = cli(&[
        "search",
        "--query",
        "Add repoise config",
        "--lane",
        "history",
        "--json",
        root,
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = value["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "{results:?}");
    let root_hit = &results[0];
    let hunks = root_hit["hunks"].as_array().expect("hunks array");
    assert!(
        !hunks.is_empty(),
        "the root commit must record its own hunks: {results:?}"
    );
    for hunk in hunks {
        assert_eq!(
            hunk["path"].as_str(),
            Some("repoise.config.json"),
            "root hunk names a file the root commit never touched: {hunk:?}"
        );
    }
}

#[test]
fn crafted_header_like_path_cannot_hide_a_commit() {
    let dir = init_git_repo("crafted", r#"{"history":{"enabled":true}}"#);
    std::fs::write(dir.join("a.md"), "target\n").unwrap();
    git(&["add", "a.md"], &dir);
    git(&["commit", "-q", "-m", "Real target commit"], &dir);
    let target_sha = git_capture(&["rev-parse", "HEAD"], &dir);
    // Plant a file whose name starts with the framing byte and then looks
    // like a header for the target commit. Only the per-invocation nonce
    // distinguishes real headers; this path must stay a path.
    // Stage the path through git plumbing (argv bytes, not filesystem
    // paths) so the fixture also runs on platforms where control bytes
    // cannot appear in file names (Windows).
    let evil_name = format!("\u{1}{target_sha}\u{1f}\u{1f}\u{1f}\u{1f}");
    let blob = git_stdin(&["hash-object", "-w", "--stdin"], &dir, "planted\n");
    git(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{evil_name}"),
        ],
        &dir,
    );
    git(&["commit", "-q", "-m", "Plant fake header path"], &dir);
    let root = dir.to_str().unwrap();

    let out = cli(&["index", root]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("history: 3 items"),
        "no commit may vanish from the lane; {text}"
    );
    let out = cli(&[
        "search",
        "--query",
        "real target",
        "--lane",
        "history",
        root,
    ]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("1 result(s)"), "{text}");
}

#[test]
fn missing_token_disables_enrichment_entirely() {
    let dir = init_git_repo(
        "no-token",
        r#"{"history":{"enabled":true,"enrichment":{"host":"github","tokenEnv":"REPOISE_HISTORY_TEST_TOKEN"}}}"#,
    );
    git(
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/private.git",
        ],
        &dir,
    );
    std::fs::write(dir.join("a.md"), "target\n").unwrap();
    git(&["add", "a.md"], &dir);
    git(&["commit", "-q", "-m", "Fix thing (#1)"], &dir);
    let root = dir.to_str().unwrap();

    // A dead proxy: any attempted request would fail and show up as a
    // failed enrichment item. With the token unset, no request may be sent.
    let out = Command::new(env!("CARGO_BIN_EXE_repoise"))
        .arg("index")
        .arg(root)
        .env_remove("REPOISE_HISTORY_TEST_TOKEN")
        .env("http_proxy", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .output()
        .expect("CLI should start");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("history: 2 items (0 verified, 0 no-association, 0 failed)"),
        "no unauthenticated request may be attempted; {text}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("token variable REPOISE_HISTORY_TEST_TOKEN is not set"),
        "expected the skip note; {stderr}"
    );
}

#[test]
fn read_rejects_history_lane_and_unknown_lanes_error() {
    let dir = temp_git_repo("lane-guards");
    let root = dir.to_str().unwrap();
    let out = cli(&["index", root]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = cli(&["read", "--lane", "history", root]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("history items have no exact-read lane"),
        "{stderr}"
    );

    let out = cli(&["search", "--query", "guide", "--lane", "bogus", root]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown search lane: bogus"), "{stderr}");
}
