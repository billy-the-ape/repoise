//! stdio MCP server over the shared engine services (card K6, master plan
//! sections 8 and 10).
//!
//! The server is a thin adapter: every tool call runs the same `search`,
//! `read`, `related`, `status` and `indexing` services the CLI already uses,
//! so CLI and MCP return identical versioned schemas. Stdout carries MCP
//! protocol traffic (newline-delimited JSON-RPC 2.0) only; all diagnostics go
//! to stderr. The server is bound at startup to one configured scope
//! (repository root + snapshot mode) and rechecks that scope on every call:
//! opaque source ids are references, not authorization. The server is
//! read-only unless the operator explicitly enables the refresh tool (config
//! `mcp.refresh` or `--allow-refresh`); refresh runs the same bounded,
//! transactional index operation as `repoise index` and never executes
//! repository scripts.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use repoise_core::adapter::SnapshotMode;
use repoise_core::classify::Role;
use repoise_core::related::RelationKind;
use repoise_core::store::Store;
use serde_json::{Map, Value, json};

/// Protocol versions the server negotiates with clients.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
/// Version negotiated when the client requests an unknown one.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
/// Hard cap on one serialized tool result (server output cap, in addition to
/// the service-level result/token limits).
const MAX_TOOL_OUTPUT_BYTES: usize = 256 * 1024;

/// One stdio MCP server session.
struct Server {
    context: crate::Context,
    allow_refresh: bool,
}

/// Runs the stdio MCP server until stdin closes (clean shutdown, exit 0).
pub fn run_mcp(opts: &crate::CliOptions) -> Result<ExitCode, String> {
    let context = crate::build_context(opts)?;
    let allow_refresh = opts.allow_refresh || context.effective.mcp_refresh;
    let server = Server {
        context,
        allow_refresh,
    };
    eprintln!(
        "repoise mcp: serving {}/{} on stdio; read-only{}",
        server.context.repo_id,
        server.context.worktree_id,
        if allow_refresh {
            "; refresh enabled"
        } else {
            ""
        }
    );
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("repoise mcp: stdin read error: {err}; shutting down");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let response = handle_message(&server, line);
        if let Some(value) = response {
            if let Err(err) = writeln!(out, "{value}") {
                eprintln!("repoise mcp: stdout write error: {err}; shutting down");
                break;
            }
            let _ = out.flush();
        }
    }
    let _ = out.flush();
    eprintln!("repoise mcp: stdin closed; shutting down");
    Ok(ExitCode::SUCCESS)
}

/// Handles one JSON-RPC message; returns a response value for requests only
/// (notifications produce none).
fn handle_message(server: &Server, line: &str) -> Option<Value> {
    let parsed: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("repoise mcp: parse error: {err}");
            return Some(error_response(Value::Null, -32700, "parse error"));
        }
    };
    let object = match &parsed {
        Value::Object(object) => object,
        Value::Array(_) => {
            return Some(error_response(
                Value::Null,
                -32600,
                "batch requests are not supported",
            ));
        }
        _ => return Some(error_response(Value::Null, -32600, "invalid request")),
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let is_request = id != Value::Null;
    let method = match object.get("method").and_then(Value::as_str) {
        Some(method) => method.to_string(),
        None => {
            return if is_request {
                Some(error_response(
                    id,
                    -32600,
                    "invalid request: missing method",
                ))
            } else {
                None
            };
        }
    };
    if !is_request {
        // Notifications (initialized, cancelled, unknown): no response.
        return None;
    }
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match method.as_str() {
        "initialize" => Some(success_response(id, initialize_result(&params))),
        "ping" => Some(success_response(id, Value::Object(Map::new()))),
        "tools/list" => Some(success_response(id, json!({ "tools": server.tools() }))),
        "tools/call" => Some(success_response(id, server.tool_call(&params))),
        other => Some(error_response(
            id,
            -32601,
            &format!("method not found: {other}"),
        )),
    }
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        DEFAULT_PROTOCOL_VERSION
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "repoise",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true
    })
}
impl Server {
    /// The advertised tools (refresh only when explicitly enabled).
    fn tools(&self) -> Vec<Value> {
        let mut tools = vec![
            json!({
                "name": "search_project_knowledge",
                "description": "Offline lexical (or hybrid) search over the configured repository's index. Read-only. Scope is fixed by the server launch; optional repoId/worktreeId/snapshotMode arguments must match it.",
                "inputSchema": with_scope(json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "minLength": 1, "description": "Natural-language or identifier query" },
                        "pathFilter": { "type": "string", "description": "Glob filter on the repository-relative path" },
                        "role": { "type": "string", "enum": ["instruction", "current-doc", "decision", "plan", "execution-record", "historical", "code", "config"] },
                        "maxResults": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Page size (default 5, cap 20)" },
                        "maxOutputTokens": { "type": "integer", "minimum": 1, "description": "Output token budget, estimated (default 1200)" },
                        "cursor": { "type": "string", "description": "Opaque pagination token from a previous page" },
                        "mode": { "type": "string", "enum": ["lexical", "hybrid", "vectors-only"], "description": "Retrieval mode (default lexical)" },
                        "rrfK": { "type": "integer", "minimum": 1, "description": "Reciprocal-rank-fusion k for hybrid mode" }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                })),
            }),
            json!({
                "name": "read_project_knowledge",
                "description": "Exact read-back of one opaque source id with expected-hash validation. Returns the validated exact text, truncated and marked when it exceeds maxOutputTokens (default 2500).",
                "inputSchema": with_scope(json!({
                    "type": "object",
                    "properties": {
                        "sourceId": { "type": "string", "minLength": 1, "description": "Opaque source id from a search/related result" },
                        "maxOutputTokens": { "type": "integer", "minimum": 1, "description": "Output token budget, estimated (default 2500)" }
                    },
                    "required": ["sourceId"],
                    "additionalProperties": false
                })),
            }),
            json!({
                "name": "related_project_knowledge",
                "description": "Follow structural/section links (reference edges, referencers, parent/child chunks) from one opaque source id.",
                "inputSchema": with_scope(json!({
                    "type": "object",
                    "properties": {
                        "sourceId": { "type": "string", "minLength": 1, "description": "Opaque source id from a search/related result" },
                        "relations": { "type": "array", "items": { "type": "string", "enum": ["references", "referenced-by", "children", "parent"] }, "description": "Relation kinds to follow (all when omitted)" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Result limit (default 10, cap 20)" },
                        "maxOutputTokens": { "type": "integer", "minimum": 1, "description": "Output token budget, estimated (default 2500)" }
                    },
                    "required": ["sourceId"],
                    "additionalProperties": false
                })),
            }),
            json!({
                "name": "project_knowledge_status",
                "description": "Operator status for the configured scope: published index, live snapshot, freshness, coverage, cache and last error.",
                "inputSchema": scope_input_schema(),
            }),
        ];
        if self.allow_refresh {
            tools.push(json!({
                "name": "refresh_project_knowledge",
                "description": "Opt-in incremental refresh: rebuild and publish the index for the configured scope (the same operation as `repoise index`). Never executes repository scripts.",
                "inputSchema": json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            }));
        }
        tools
    }

    /// Dispatches one `tools/call` request.
    fn tool_call(&self, params: &Value) -> Value {
        let object = match params.as_object() {
            Some(object) if object.get("name").and_then(Value::as_str).is_some() => object,
            _ => return tool_error("invalid params: tools/call requires a string tool name"),
        };
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let empty = Map::new();
        let arguments: &Map<String, Value> = match object.get("arguments") {
            Some(Value::Object(map)) => map,
            Some(_) => return tool_error("invalid params: arguments must be an object"),
            None => &empty,
        };
        match self.call_tool(&name, arguments) {
            Ok(value) => {
                let text = value.to_string();
                if text.len() > MAX_TOOL_OUTPUT_BYTES {
                    return tool_error(&format!(
                        "tool result exceeded the server output cap ({MAX_TOOL_OUTPUT_BYTES} bytes); reduce maxResults or maxOutputTokens"
                    ));
                }
                json!({ "content": [ { "type": "text", "text": text } ] })
            }
            Err(message) => tool_error(&message),
        }
    }

    fn call_tool(&self, name: &str, args: &Map<String, Value>) -> Result<Value, String> {
        match name {
            "search_project_knowledge" => self.call_search(args),
            "read_project_knowledge" => self.call_read(args),
            "related_project_knowledge" => self.call_related(args),
            "project_knowledge_status" => self.call_status(args),
            "refresh_project_knowledge" => self.call_refresh(),
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// Validates optional scope arguments against the server-configured
    /// scope. Opaque ids are references, not authorization: every operation
    /// rechecks the configured scope.
    fn check_scope(&self, args: &Map<String, Value>) -> Result<(), String> {
        if let Some(repo) = args.get("repoId").and_then(Value::as_str)
            && repo != self.context.repo_id
        {
            return Err(format!(
                "scope mismatch: this server is bound to repo {}, refusing {repo}",
                self.context.repo_id
            ));
        }
        if let Some(worktree) = args.get("worktreeId").and_then(Value::as_str)
            && worktree != self.context.worktree_id
        {
            return Err(format!(
                "scope mismatch: this server is bound to worktree {}, refusing {worktree}",
                self.context.worktree_id
            ));
        }
        if let Some(mode) = args.get("snapshotMode").and_then(Value::as_str) {
            let expected = match self.context.mode {
                SnapshotMode::WorkingTree => "working-tree",
                SnapshotMode::Committed => "committed",
                SnapshotMode::PlainDirectory => "plain-directory",
            };
            if mode != expected {
                return Err(format!(
                    "scope mismatch: this server is bound to snapshot mode {expected}, refusing {mode}"
                ));
            }
        }
        Ok(())
    }

    fn store(&self) -> Store {
        Store::new(
            self.context
                .cache
                .db_path(&self.context.repo_id, &self.context.worktree_id),
        )
    }

    fn call_search(&self, args: &Map<String, Value>) -> Result<Value, String> {
        self.check_scope(args)?;
        let query = string_arg(args, "query")?;
        if query.trim().is_empty() {
            return Err("query must not be empty".into());
        }
        let path_filter = args
            .get("pathFilter")
            .and_then(Value::as_str)
            .map(str::to_string);
        let role_filter = match args.get("role").and_then(Value::as_str) {
            Some(name) => Some(
                Role::parse(name)
                    .ok_or_else(|| format!("unknown role: {name} (expected instruction, current-doc, decision, plan, execution-record, historical, code, or config)"))?,
            ),
            None => None,
        };
        let max_results = u32_arg(args, "maxResults")?;
        let max_output_tokens = u64_arg(args, "maxOutputTokens")?;
        let cursor = args
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mode = match args.get("mode").and_then(Value::as_str) {
            Some("hybrid") => repoise_core::search::SearchMode::Hybrid,
            Some("vectors-only") => repoise_core::search::SearchMode::VectorsOnly,
            Some("lexical") | None => repoise_core::search::SearchMode::Lexical,
            Some(other) => {
                return Err(format!(
                    "unknown mode: {other} (expected lexical, hybrid, or vectors-only)"
                ));
            }
        };
        let rrf_k = u32_arg(args, "rrfK")?;
        let query_embedder = if mode != repoise_core::search::SearchMode::Lexical {
            crate::query_embedder_from_config(&self.context.effective)?
        } else {
            None
        };
        let request = repoise_core::search::SearchRequest {
            query,
            path_filter,
            role_filter,
            max_results,
            max_output_tokens,
            cursor,
            mode,
            rrf_k: rrf_k.or(Some(self.context.effective.rrf_k)),
        };
        let response = repoise_core::search::search(
            self.context.adapter.as_ref(),
            self.context.mode,
            &self.store(),
            &request,
            query_embedder.as_deref(),
        )
        .map_err(|err| err.to_string())?;
        serde_json::to_value(&response).map_err(|err| err.to_string())
    }

    fn call_read(&self, args: &Map<String, Value>) -> Result<Value, String> {
        self.check_scope(args)?;
        let source_id = string_arg(args, "sourceId")?;
        let max_output_tokens = u64_arg(args, "maxOutputTokens")?;
        let result = repoise_core::read::read(
            self.context.adapter.as_ref(),
            self.context.mode,
            &self.store(),
            &repoise_core::read::ReadRequest {
                source_id,
                max_output_tokens,
            },
        )
        .map_err(|err| err.to_string())?;
        serde_json::to_value(&result).map_err(|err| err.to_string())
    }

    fn call_related(&self, args: &Map<String, Value>) -> Result<Value, String> {
        self.check_scope(args)?;
        let source_id = string_arg(args, "sourceId")?;
        let kinds = match args.get("relations") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .and_then(RelationKind::parse)
                        .ok_or_else(|| format!("unknown relation kind: {item}"))
                })
                .collect::<Result<Vec<_>, String>>()?,
            Some(_) => return Err("relations must be an array of strings".into()),
        };
        let limit = u32_arg(args, "limit")?;
        let max_output_tokens = u64_arg(args, "maxOutputTokens")?;
        let response = repoise_core::related::related(
            self.context.adapter.as_ref(),
            self.context.mode,
            &self.store(),
            &repoise_core::related::RelatedRequest {
                source_id,
                kinds,
                limit,
                max_output_tokens,
            },
        )
        .map_err(|err| err.to_string())?;
        serde_json::to_value(&response).map_err(|err| err.to_string())
    }

    fn call_status(&self, args: &Map<String, Value>) -> Result<Value, String> {
        self.check_scope(args)?;
        let view = repoise_core::status::status(
            self.context.adapter.as_ref(),
            self.context.mode,
            &self.context.effective,
            self.context.config_file.as_deref(),
            &self.store(),
            &self.context.cache,
        )
        .map_err(|err| err.to_string())?;
        serde_json::to_value(&view).map_err(|err| err.to_string())
    }

    fn call_refresh(&self) -> Result<Value, String> {
        if !self.allow_refresh {
            return Err(
                "refresh_project_knowledge is not enabled: this server is read-only. Enable it with the mcp.refresh configuration (repoise.local.json / repoise.config.json) or --allow-refresh."
                    .into(),
            );
        }
        let client = crate::embedding_client(&self.context.effective)?;
        let enrichment = crate::enrichment_session(&self.context)?;
        let outcome = repoise_core::indexing::index(
            self.context.adapter.as_ref(),
            self.context.mode,
            &self.context.effective,
            &self.store(),
            &self.context.cache,
            &repoise_core::indexing::IndexRequest::default(),
            client.as_ref(),
            enrichment.as_ref(),
        )
        .map_err(|err| err.to_string())?;
        serde_json::to_value(&outcome).map_err(|err| err.to_string())
    }
}

/// Scope-verification input schema (optional; must match the server scope).
fn scope_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repoId": { "type": "string", "description": "Must match the server-configured repository scope (optional)" },
            "worktreeId": { "type": "string", "description": "Must match the server-configured worktree scope (optional)" },
            "snapshotMode": { "type": "string", "enum": ["working-tree", "committed", "plain-directory"], "description": "Must match the server-configured snapshot mode (optional)" }
        },
        "additionalProperties": false
    })
}

/// Adds the scope-verification properties to a tool input schema.
fn with_scope(mut schema: Value) -> Value {
    let scope = scope_input_schema();
    if let (Some(properties), Some(scope_properties)) = (
        schema["properties"].as_object_mut(),
        scope["properties"].as_object(),
    ) {
        for (key, value) in scope_properties {
            properties.insert(key.clone(), value.clone());
        }
    }
    schema
}

/// Required non-empty string argument.
fn string_arg(args: &Map<String, Value>, name: &str) -> Result<String, String> {
    match args.get(name) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(format!("{name} must not be empty")),
        Some(_) => Err(format!("{name} must be a string")),
        None => Err(format!("{name} is required")),
    }
}

/// Optional positive integer argument (u32).
fn u32_arg(args: &Map<String, Value>, name: &str) -> Result<Option<u32>, String> {
    match args.get(name) {
        None => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("{name} must be a positive integer"))
            .map(Some),
        Some(_) => Err(format!("{name} must be a positive integer")),
    }
}

/// Optional positive integer argument (u64).
fn u64_arg(args: &Map<String, Value>, name: &str) -> Result<Option<u64>, String> {
    match args.get(name) {
        None => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| format!("{name} must be a positive integer"))
            .map(Some),
        Some(_) => Err(format!("{name} must be a positive integer")),
    }
}
