//! First-class `.env` loading at the CLI boundary.
//!
//! Configured environment references (`env:` endpoints, `apiKeyEnv`, history
//! `tokenEnv`) and the `REPOISE_CACHE_DIR` override resolve from the process
//! environment first, then from the target repository root's `.env` as a
//! fallback. The file is parsed strictly as data; values never enter
//! diagnostics, caches, or the indexed corpus.

use std::path::Path;
use std::process::{Command, Output};

fn cli_in(cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_repoise"));
    cmd.args(args).current_dir(cwd);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.output().expect("CLI should start")
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("repoise-dtest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn configured_variable_resolves_from_target_root_dotenv() {
    let root = temp_dir("dotenv-cache");
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    let cache = temp_dir("dotenv-cache-target");
    std::fs::write(
        root.join(".env"),
        format!("REPOISE_CACHE_DIR={}\n", cache.display()),
    )
    .expect("write .env");

    let output = cli_in(&root, &["status", root.to_str().unwrap()], &[]);
    // No published index yet: operational exit 3, but the scope is reported.
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("cache: {}", cache.display())),
        "cache path must come from the .env value: {stdout}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("loaded .env"),
        "a load note (without values) is expected: {stderr}"
    );
}

#[test]
fn process_environment_overrides_the_same_key_in_dotenv() {
    let root = temp_dir("dotenv-override");
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    let from_dotenv = temp_dir("dotenv-override-file");
    let from_process = temp_dir("dotenv-override-process");
    std::fs::write(
        root.join(".env"),
        format!("REPOISE_CACHE_DIR={}\n", from_dotenv.display()),
    )
    .expect("write .env");

    let process_value = from_process.display().to_string();
    let output = cli_in(
        &root,
        &["status", root.to_str().unwrap()],
        &[("REPOISE_CACHE_DIR", &process_value)],
    );
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("cache: {}", from_process.display())),
        "process environment must win: {stdout}"
    );
    assert!(
        !stdout.contains(&format!("cache: {}", from_dotenv.display())),
        "the .env value must not be used while the process environment is set: {stdout}"
    );
}

#[test]
fn commands_outside_the_repository_load_the_target_root_dotenv() {
    let outside = temp_dir("dotenv-outside");
    let root = temp_dir("dotenv-target");
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    let cache = temp_dir("dotenv-target-cache");
    std::fs::write(
        root.join(".env"),
        format!("REPOISE_CACHE_DIR={}\n", cache.display()),
    )
    .expect("write .env");

    // The current directory has no .env of its own.
    let output = cli_in(&outside, &["status", root.to_str().unwrap()], &[]);
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("cache: {}", cache.display())),
        "the target root's .env must apply when launched outside the repo: {stdout}"
    );
}

#[test]
fn missing_dotenv_preserves_current_behavior() {
    let root = temp_dir("dotenv-missing");
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");

    let output = cli_in(&root, &["status", root.to_str().unwrap()], &[]);
    assert_eq!(output.status.code(), Some(3));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let default_cache = root.join(".repoise");
    assert!(
        stdout.contains(&format!("cache: {}", default_cache.display())),
        "the default project-local cache is expected: {stdout}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("loaded .env"),
        "no .env was present: {stderr}"
    );
}

#[test]
fn dotenv_values_never_appear_in_logs_or_indexed_documents() {
    let root = temp_dir("dotenv-secret");
    std::fs::write(root.join("README.md"), "hello world\n").expect("write readme");
    let cache = temp_dir("dotenv-secret-cache");
    let secret = "zzz-dotenv-secret-value-42";
    std::fs::write(
        root.join(".env"),
        format!(
            "REPOISE_CACHE_DIR={}\nREPOISE_FAKE_KEY={secret}\n",
            cache.display()
        ),
    )
    .expect("write .env");

    let index = cli_in(&root, &["index", root.to_str().unwrap()], &[]);
    assert!(
        index.status.success(),
        "{}",
        String::from_utf8_lossy(&index.stderr)
    );
    let index_stdout = String::from_utf8(index.stdout).unwrap();
    let index_stderr = String::from_utf8(index.stderr).unwrap();
    assert!(!index_stdout.contains(secret), "stdout leaked .env values");
    assert!(!index_stderr.contains(secret), "stderr leaked .env values");

    // The .env stays secret-denied out of the corpus.
    let explain = cli_in(
        &root,
        &["explain", "--path", ".env", root.to_str().unwrap()],
        &[],
    );
    assert!(explain.status.success());
    let explain_text = String::from_utf8(explain.stdout).unwrap();
    assert!(explain_text.contains("decision: excluded"));
    assert!(explain_text.contains("secret-deny"));

    // The value is not retrievable through the index.
    let search = cli_in(
        &root,
        &["search", "--query", secret, root.to_str().unwrap()],
        &[],
    );
    assert!(search.status.success());
    let search_stdout = String::from_utf8(search.stdout).unwrap();
    let search_stderr = String::from_utf8(search.stderr).unwrap();
    assert!(search_stdout.contains("0 result(s)"));
    assert!(
        !search_stdout.contains(secret),
        "search output leaked .env values"
    );
    assert!(
        !search_stderr.contains(secret),
        "search stderr leaked .env values"
    );
}

#[test]
fn malformed_dotenv_fails_with_a_value_free_diagnostic() {
    let root = temp_dir("dotenv-malformed");
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    let bad_line = "THIS LINE HAS NO KEY EQUALS VALUE PAIR";
    std::fs::write(root.join(".env"), format!("{bad_line}\n")).expect("write .env");

    let output = cli_in(&root, &["status", root.to_str().unwrap()], &[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(".env"),
        "diagnostic names the file: {stderr}"
    );
    assert!(
        stderr.contains("malformed"),
        "diagnostic explains the cause: {stderr}"
    );
    assert!(
        !stderr.contains(bad_line),
        "diagnostic must not echo file content: {stderr}"
    );
}

#[test]
fn embedding_credentials_resolve_from_the_target_root_dotenv() {
    let root = temp_dir("dotenv-embed");
    std::fs::write(root.join("README.md"), "hello world\n").expect("write readme");
    let cache = temp_dir("dotenv-embed-cache");
    std::fs::write(
        root.join("repoise.local.json"),
        r#"{
          "schemaVersion": 1,
          "embedding": {
            "provider": "openai-compatible",
            "endpoint": "env:REPOISE_DOTENV_ENDPOINT",
            "model": "dtest-model",
            "dimensions": 8,
            "apiKeyEnv": "REPOISE_DOTENV_API_KEY",
            "timeoutMs": 2000,
            "maxRetries": 0
          }
        }
        "#,
    )
    .expect("write local config");
    std::fs::write(
        root.join(".env"),
        format!(
            "REPOISE_CACHE_DIR={}\nREPOISE_DOTENV_ENDPOINT=http://127.0.0.1:9/v1\nREPOISE_DOTENV_API_KEY=zzz-not-a-real-key\n",
            cache.display()
        ),
    )
    .expect("write .env");

    let output = cli_in(&root, &["index", root.to_str().unwrap()], &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Both credential references resolved from the .env: the build attempted
    // (and failed) provider requests instead of degrading to lexical-only.
    assert!(
        !stderr.contains("REPOISE_DOTENV_ENDPOINT is not set"),
        "endpoint must come from the .env: {stderr}"
    );
    assert!(
        !stderr.contains("REPOISE_DOTENV_API_KEY is not set"),
        "API key must come from the .env: {stderr}"
    );
    assert!(
        stdout.contains("1 failed"),
        "the embedding batch should have been attempted against the fake endpoint: {stdout}"
    );
    assert!(
        !stdout.contains("zzz-not-a-real-key"),
        "stdout leaked the API key"
    );
    assert!(
        !stderr.contains("zzz-not-a-real-key"),
        "stderr leaked the API key"
    );
}
