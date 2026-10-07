//! End-to-end history-lane behavior (card K5): local collection, FTS search
//! semantics, path GLOB filters, untrusted-path handling, merge commits and
//! horizon gaps.

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
