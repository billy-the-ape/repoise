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
            format!("Repoise {}\n", env!("CARGO_PKG_VERSION"))
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
