//! K6 (part 2): stdio MCP server acceptance — protocol initialization, tool
//! discovery/calls, CLI/MCP result equivalence, stale/hash errors, scope
//! enforcement, refresh permissions, cursors and clean shutdown.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_root(name: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-mcp-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_repoise"))
        .args(args)
        .output()
        .expect("CLI should start")
}

fn cli_json(args: &[&str]) -> Value {
    let output = cli(args);
    assert!(
        output.status.success(),
        "CLI failed: {:?} stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI --json emits valid JSON")
}

/// One stdio MCP session over the spawned server.
struct Mcp {
    child: Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn spawn(root: &std::path::Path, extra: &[&str]) -> Self {
        let root = root.to_string_lossy().into_owned();
        let mut command = Command::new(env!("CARGO_BIN_EXE_repoise"));
        command.arg("mcp");
        for arg in extra {
            command.arg(arg);
        }
        command.arg(root);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("mcp server should start");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = child.stdout.take().expect("stdout pipe");
        Mcp {
            child,
            stdin,
            reader: BufReader::new(stdout),
        }
    }

    fn send_raw(&mut self, line: &str) {
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .expect("stdin write should succeed");
    }

    /// Reads one response line (stdout carries protocol traffic only, so every
    /// line must parse as JSON-RPC).
    fn response(&mut self) -> Value {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("stdout read should succeed");
        serde_json::from_str(&line).expect("stdout line must be valid JSON-RPC: {line:?}")
    }

    fn request(&mut self, id: u32, method: &str, params: Value) -> Value {
        self.send_raw(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        );
        let value = self.response();
        assert_eq!(value["id"], json!(id), "response id mismatch: {value}");
        value
    }

    fn call(&mut self, id: u32, tool: &str, args: Value) -> Value {
        self.request(id, "tools/call", json!({ "name": tool, "arguments": args }))
    }

    /// A successful tool call: the text content is the serialized service
    /// response (the same versioned schema the CLI `--json` emits).
    fn tool_result(&mut self, id: u32, tool: &str, args: Value) -> Value {
        let response = self.call(id, tool, args);
        assert!(
            response.get("error").is_none(),
            "expected a successful tool call: {response}"
        );
        let is_error = response["result"].get("isError");
        assert!(
            is_error.is_none() || is_error == Some(&json!(false)),
            "expected a successful tool call: {response}"
        );
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        serde_json::from_str(text).expect("tool text is valid JSON")
    }

    /// A tool-level error call (`isError: true`) with a text message.
    fn tool_error(&mut self, id: u32, tool: &str, args: Value) -> String {
        let response = self.call(id, tool, args);
        assert_eq!(
            response["result"].get("isError"),
            Some(&json!(true)),
            "expected a tool error: {response}"
        );
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("error text")
            .to_string()
    }

    fn close(mut self) -> std::process::ExitStatus {
        // Dropping the stdin handle closes the pipe: the server sees EOF and
        // shuts down cleanly.
        drop(self.stdin);
        self.child.wait().expect("server should exit")
    }
}
#[test]
fn mcp_initialize_negotiates_and_lists_read_only_tools() {
    let root = temp_root("init");
    let mut mcp = Mcp::spawn(&root, &[]);
    let init = mcp.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "0" }
        }),
    );
    assert!(init.get("error").is_none(), "{init}");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "repoise");
    assert!(init["result"]["serverInfo"]["version"].is_string());
    assert!(init["result"]["capabilities"]["tools"].is_object());
    // Unknown requested versions negotiate the default instead of failing.
    let init_unknown = mcp.request(2, "initialize", json!({ "protocolVersion": "1999-01-01" }));
    assert_eq!(init_unknown["result"]["protocolVersion"], "2025-06-18");
    // The default server is read-only: exactly the four knowledge tools.
    let list = mcp.request(3, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "search_project_knowledge",
            "read_project_knowledge",
            "related_project_knowledge",
            "project_knowledge_status"
        ]
    );
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert!(tool["inputSchema"]["type"] == "object", "{tool}");
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
    let status = mcp.close();
    assert!(status.success(), "clean shutdown on stdin EOF");
}

#[test]
fn mcp_search_and_read_equivalent_to_cli() {
    let root = temp_root("equivalence");
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    let root_arg = root.to_string_lossy().into_owned();
    let _ = cli_json(&["index", "--json", &root_arg]);
    let mut mcp = Mcp::spawn(&root, &[]);
    let mcp_search = mcp.tool_result(1, "search_project_knowledge", json!({ "query": "hello" }));
    let cli_search = cli_json(&["search", "--json", "--query", "hello", &root_arg]);
    assert_eq!(
        mcp_search, cli_search,
        "CLI and MCP search responses must be identical"
    );
    assert!(!mcp_search["results"].as_array().unwrap().is_empty());
    let source_id = mcp_search["results"][0]["source_id"].as_str().unwrap();
    let mcp_read = mcp.tool_result(
        2,
        "read_project_knowledge",
        json!({ "sourceId": source_id }),
    );
    let cli_read = cli_json(&["read", "--json", "--source-id", source_id, &root_arg]);
    assert_eq!(
        mcp_read, cli_read,
        "CLI and MCP read responses must be identical"
    );
    assert_eq!(mcp_read["truncated"], json!(false));
    assert!(mcp_read["output_tokens"].as_u64().unwrap() > 0);
    let mcp_read_capped = mcp.tool_result(
        3,
        "read_project_knowledge",
        json!({ "sourceId": source_id, "maxOutputTokens": 1 }),
    );
    assert_eq!(mcp_read_capped["truncated"], json!(true));
    mcp.close();
}

#[test]
fn mcp_status_and_related_equivalent_to_cli() {
    let root = temp_root("status-related");
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    let root_arg = root.to_string_lossy().into_owned();
    let _ = cli_json(&["index", "--json", &root_arg]);
    let mut mcp = Mcp::spawn(&root, &[]);
    let mcp_status = mcp.tool_result(1, "project_knowledge_status", json!({}));
    let cli_status = cli_json(&["status", "--json", &root_arg]);
    assert_eq!(mcp_status["scope"], cli_status["scope"]);
    assert_eq!(mcp_status["snapshot"], cli_status["snapshot"]);
    assert_eq!(
        mcp_status["index"]["generation_id"],
        cli_status["index"]["generation_id"]
    );
    assert_eq!(mcp_status["freshness"]["status"], "fresh");
    // Related: identical to the CLI related response for the same source.
    let search = mcp.tool_result(2, "search_project_knowledge", json!({ "query": "hello" }));
    let source_id = search["results"][0]["source_id"].as_str().unwrap();
    let mcp_related = mcp.tool_result(
        3,
        "related_project_knowledge",
        json!({ "sourceId": source_id }),
    );
    let cli_related = cli_json(&["related", "--json", "--source-id", source_id, &root_arg]);
    assert_eq!(
        mcp_related, cli_related,
        "CLI and MCP related responses must be identical"
    );
    mcp.close();
}

#[test]
fn mcp_stale_hash_and_missing_errors() {
    let root = temp_root("stale");
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    let root_arg = root.to_string_lossy().into_owned();
    let mut mcp = Mcp::spawn(&root, &[]);
    // Before any index: a clean tool-level index-state error.
    let missing = mcp.tool_error(1, "search_project_knowledge", json!({ "query": "hello" }));
    assert!(missing.contains("no published index"), "{missing}");
    cli_json(&["index", "--json", &root_arg]);
    let search = mcp.tool_result(2, "search_project_knowledge", json!({ "query": "hello" }));
    let source_id = search["results"][0]["source_id"].as_str().unwrap();
    // The file changes outside the index: read fails with a stale diagnostic.
    std::fs::write(root.join("README.md"), "completely different content\n").unwrap();
    let stale = mcp.tool_error(
        3,
        "read_project_knowledge",
        json!({ "sourceId": source_id }),
    );
    assert!(stale.contains("stale source"), "{stale}");
    assert!(stale.contains("content hash changed"), "{stale}");
    // An unknown source id is a reference, not authorization: rejected.
    let unknown = mcp.tool_error(
        4,
        "read_project_knowledge",
        json!({ "sourceId": "bogus-id" }),
    );
    assert!(
        unknown.contains("source id not in the current generation"),
        "{unknown}"
    );
    // Unknown tools are tool-level errors, not protocol errors.
    let unknown_tool = mcp.tool_error(5, "nope", json!({}));
    assert!(unknown_tool.contains("unknown tool"), "{unknown_tool}");
    mcp.close();
}

#[test]
fn mcp_refresh_is_explicitly_opt_in() {
    // Default: the refresh tool is not advertised and calls are denied.
    let root = temp_root("refresh-default");
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    let mut mcp = Mcp::spawn(&root, &[]);
    let list = mcp.request(1, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"refresh_project_knowledge"));
    let denied = mcp.tool_error(2, "refresh_project_knowledge", json!({}));
    assert!(denied.contains("read-only"), "{denied}");
    // --allow-refresh: advertised, and the call publishes a new generation.
    let root2 = temp_root("refresh-flag");
    std::fs::write(root2.join("README.md"), "hello world\n").unwrap();
    let mut mcp2 = Mcp::spawn(&root2, &["--allow-refresh"]);
    let list2 = mcp2.request(1, "tools/list", json!({}));
    let names2: Vec<&str> = list2["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names2.contains(&"refresh_project_knowledge"));
    let outcome = mcp2.tool_result(2, "refresh_project_knowledge", json!({}));
    assert_eq!(outcome["files_indexed"], json!(1));
    assert!(outcome["generation_id"].as_i64().unwrap() >= 1);
    // The refresh served the changed content: search now works, status fresh.
    let search = mcp2.tool_result(3, "search_project_knowledge", json!({ "query": "hello" }));
    assert!(!search["results"].as_array().unwrap().is_empty());
    let status = mcp2.tool_result(4, "project_knowledge_status", json!({}));
    assert_eq!(status["freshness"]["status"], "fresh");
    mcp2.close();
    mcp.close();
}

#[test]
fn mcp_refresh_enabled_by_local_config() {
    let root = temp_root("refresh-config");
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    std::fs::write(
        root.join("repoise.local.json"),
        r#"{"mcp": {"refresh": true}}"#,
    )
    .unwrap();
    let mut mcp = Mcp::spawn(&root, &[]);
    let list = mcp.request(1, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"refresh_project_knowledge"),
        "mcp.refresh config must enable the tool"
    );
    let outcome = mcp.tool_result(2, "refresh_project_knowledge", json!({}));
    assert!(
        outcome["files_indexed"].as_u64().unwrap() >= 1,
        "the refresh must index the repository: {outcome}"
    );
    mcp.close();
}

#[test]
fn mcp_scope_enforcement() {
    let root = temp_root("scope");
    std::fs::write(root.join("README.md"), "hello world\n").unwrap();
    let root_arg = root.to_string_lossy().into_owned();
    cli_json(&["index", "--json", &root_arg]);
    let mut mcp = Mcp::spawn(&root, &[]);
    // Opaque ids and scope arguments must match the server-configured scope.
    let bad_repo = mcp.tool_error(
        1,
        "search_project_knowledge",
        json!({ "query": "hello", "repoId": "not-this-repo" }),
    );
    assert!(bad_repo.contains("scope mismatch"), "{bad_repo}");
    let bad_worktree = mcp.tool_error(
        2,
        "read_project_knowledge",
        json!({ "sourceId": "x", "worktreeId": "other-worktree" }),
    );
    assert!(bad_worktree.contains("scope mismatch"), "{bad_worktree}");
    let bad_mode = mcp.tool_error(
        3,
        "project_knowledge_status",
        json!({ "snapshotMode": "committed" }),
    );
    assert!(bad_mode.contains("scope mismatch"), "{bad_mode}");
    // Matching scope arguments are accepted (a filesystem root is plain-directory).
    let status = mcp.tool_result(
        4,
        "project_knowledge_status",
        json!({ "snapshotMode": "plain-directory" }),
    );
    assert_eq!(status["freshness"]["status"], "fresh");
    mcp.close();
}

#[test]
fn mcp_cursor_binding_and_pagination() {
    let root = temp_root("cursor");
    std::fs::create_dir_all(root.join("docs")).unwrap();
    for name in ["a.md", "b.md", "c.md"] {
        std::fs::write(
            root.join(format!("docs/{name}")),
            format!("# {name}\n\ngizmo notes\n"),
        )
        .unwrap();
    }
    let root_arg = root.to_string_lossy().into_owned();
    cli_json(&["index", "--json", &root_arg]);
    let mut mcp = Mcp::spawn(&root, &[]);
    let page1 = mcp.tool_result(
        1,
        "search_project_knowledge",
        json!({ "query": "gizmo", "maxResults": 2 }),
    );
    assert_eq!(page1["results"].as_array().unwrap().len(), 2);
    assert_eq!(page1["truncated"], json!(true));
    let cursor = page1["next_cursor"].as_str().unwrap().to_string();
    let page2 = mcp.tool_result(
        2,
        "search_project_knowledge",
        json!({ "query": "gizmo", "maxResults": 2, "cursor": cursor }),
    );
    assert_eq!(page2["truncated"], json!(false));
    let first_ids: Vec<&str> = page1["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["source_id"].as_str().unwrap())
        .collect();
    for hit in page2["results"].as_array().unwrap() {
        assert!(!first_ids.contains(&hit["source_id"].as_str().unwrap()));
    }
    // The cursor is bound to this exact query and filters.
    let bad = mcp.tool_error(
        3,
        "search_project_knowledge",
        json!({ "query": "other", "maxResults": 2, "cursor": cursor }),
    );
    assert!(bad.contains("cursor bound to a different query"), "{bad}");
    mcp.close();
}

#[test]
fn mcp_shutdown_and_protocol_errors() {
    let root = temp_root("shutdown");
    let mut mcp = Mcp::spawn(&root, &[]);
    // Unknown method: JSON-RPC -32601.
    let unknown = mcp.request(1, "bogus/method", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601));
    // Malformed JSON line: JSON-RPC -32700, with a null id.
    mcp.send_raw("not json");
    let parse = mcp.response();
    assert_eq!(parse["error"]["code"], json!(-32700));
    assert_eq!(parse["id"], Value::Null);
    // Notifications get no response: the next read is the ping response.
    mcp.send_raw(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
    let ping = mcp.request(2, "ping", json!({}));
    assert!(ping.get("error").is_none(), "{ping}");
    assert!(ping["result"].is_object());
    // Closing stdin: clean shutdown, exit code 0.
    let status = mcp.close();
    assert!(status.success(), "clean shutdown on stdin EOF");
}
