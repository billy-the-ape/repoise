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
//!
//! Optional operator audit: `--audit-file <PATH>` appends bounded NDJSON
//! tool-call records to the designated file (server-side observation for the
//! benchmark coordinator). Audit records carry only measured evidence (tool,
//! requested/effective mode, degradation, success, duration) — never query
//! text, excerpts or credentials — and stdout still carries JSON-RPC traffic
//! only (see [crate::audit]).

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::time::Instant;

use repoise_core::adapter::SnapshotMode;
use repoise_core::classify::Role;
use repoise_core::related::RelationKind;
use repoise_core::store::Store;
use serde_json::{Map, Value, json};

use crate::audit;

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
    /// Pinned retrieval mode (`--search-mode`); when set, tool requests
    /// naming a different mode are refused.
    search_mode: Option<repoise_core::search::SearchMode>,
    /// Opt-in machine audit sink designated by the operator (the benchmark
    /// coordinator provisions one per trial); absent by default.
    audit: Option<audit::Sink>,
}

/// Audit-relevant facts the server measured for one search call (never the
/// query text, filters or results).
#[derive(Clone)]
struct SearchAudit {
    /// The retrieval mode the request resolved to (the pinned server mode
    /// applies when the request omits `mode`); unknown before resolution.
    requested: Option<String>,
    /// The retrieval mode the completed search response reported.
    effective: Option<String>,
    /// The index generation served by a completed search.
    served_generation: Option<i64>,
}

/// Runs the stdio MCP server until stdin closes (clean shutdown, exit 0).
pub fn run_mcp(opts: &crate::CliOptions) -> Result<ExitCode, String> {
    let context = crate::build_context(opts)?;
    let allow_refresh = opts.allow_refresh || context.effective.mcp_refresh;
    let search_mode = match opts.mcp_search_mode.as_deref() {
        Some("hybrid") => Some(repoise_core::search::SearchMode::Hybrid),
        Some("vectors-only") => Some(repoise_core::search::SearchMode::VectorsOnly),
        Some("lexical") => Some(repoise_core::search::SearchMode::Lexical),
        None => None,
        _ => unreachable!("validated in parse_args"),
    };
    let audit = match &opts.mcp_audit_file {
        Some(path) => match audit::Sink::create(path) {
            Ok(sink) => {
                eprintln!("repoise mcp: audit sink enabled: {}", path.display());
                Some(sink)
            }
            Err(err) => {
                eprintln!(
                    "repoise mcp: audit sink unavailable at {}: {err}; the server \
                     continues without audit (the absence of audit records is not \
                     evidence of no tool use)",
                    path.display()
                );
                None
            }
        },
        None => None,
    };
    let server = Server {
        context,
        allow_refresh,
        search_mode,
        audit,
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
    if let Some(audit) = &server.audit {
        audit.record(server.start_record());
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut shutdown_reason = "stdin_closed";
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("repoise mcp: stdin read error: {err}; shutting down");
                shutdown_reason = "stdin_error";
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
                shutdown_reason = "stdout_error";
                break;
            }
            let _ = out.flush();
        }
    }
    let _ = out.flush();
    if let Some(audit) = &server.audit {
        audit.record(json!({
            "schemaVersion": audit::AUDIT_SCHEMA_VERSION,
            "t": repoise_core::bench::runner::now_utc(),
            "kind": "server_end",
            "reason": shutdown_reason
        }));
    }
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
        "tools/call" => {
            let started = Instant::now();
            let tool = params.get("name").and_then(Value::as_str);
            let invocation = server.tool_call(&params);
            if let Some(audit) = &server.audit {
                audit.record(tool_call_record(
                    tool,
                    &invocation,
                    started.elapsed().as_millis(),
                ));
            }
            match invocation {
                Ok((result, _)) => Some(success_response(id, result)),
                Err((code, message)) => Some(error_response(id, code, &message)),
            }
        }
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
                "description": "Offline lexical (or hybrid) search over the configured repository's index. Read-only. Scope is fixed by the server launch; optional repoId/worktreeId/snapshotMode arguments must match it. Lexical matching is strict: every whitespace query term must match the same chunk's searchable fields, so broad natural-language queries may return zero results even when relevant evidence is indexed. Results carry short excerpts that locate evidence, not the full source; a short excerpt is not evidence the indexed source is unavailable - use read_project_knowledge on a relevant sourceId to inspect the implementation. For empty or irrelevant results, try a shorter identifier/symbol query and appropriate pathFilter/role filters before concluding indexed evidence is insufficient. When results are truncated and more are useful, pass the response's next_cursor value as the next request's cursor. Keep retries bounded; do not repeat poor searches or call this tool when sufficient evidence is already available.",
                "inputSchema": with_scope(json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "minLength": 1, "description": "Natural-language or identifier query" },
                        "pathFilter": { "type": "string", "description": "Glob filter on the repository-relative path" },
                        "role": { "type": "string", "enum": ["instruction", "current-doc", "decision", "plan", "execution-record", "historical", "code", "config"] },
                        "maxResults": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Page size (default 5, cap 20)" },
                        "maxOutputTokens": { "type": "integer", "minimum": 1, "description": "Output token budget, estimated (default 1200)" },
                        "cursor": { "type": "string", "description": "Opaque pagination token from a previous page" },
                        "mode": { "type": "string", "enum": ["lexical", "hybrid", "vectors-only"], "description": search_mode_description(self.search_mode) },
                        "rrfK": { "type": "integer", "minimum": 1, "description": "Reciprocal-rank-fusion k for hybrid mode" }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                })),
            }),
            json!({
                "name": "read_project_knowledge",
                "description": "Exact read-back of one opaque source id with expected-hash validation. Returns the validated exact text of the indexed range; truncated and marked only when it exceeds maxOutputTokens (default 2500), which is the only truncation this tool performs. Search excerpts are shortened previews, never proof that the source is unavailable.",
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
                "description": "Follow structural links (reference edges, referencers, parent/child chunks) from one opaque source id. An empty result only means no indexed relations exist for that source; it does not establish that source evidence is missing. If indexed evidence remains insufficient, fall back to targeted filesystem reads and explain the actual limitation.",
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
                "description": "Operator status for the configured scope: published index, live snapshot, freshness, coverage, cache and last error. Freshness means the index matches the live saved snapshot and effective config; Git dirtiness (dirtyCount) is reported separately and does not make the index stale.",
                "inputSchema": scope_input_schema(),
            }),
        ];
        if self.allow_refresh {
            tools.push(json!({
                "name": "refresh_project_knowledge",
                "description": "Opt-in incremental refresh: rebuild and publish the index for the configured scope (the same operation as `repoise index`). Never executes repository scripts. Refused with a busy error while another writer (`repoise index` or `repoise watch`) owns the scope; retry once the writer exits.",
                "inputSchema": scope_input_schema(),
            }));
        }
        tools
    }

    /// Dispatches one `tools/call` request. Argument-validation failures are
    /// JSON-RPC `-32602` invalid-params errors (no service is invoked); service
    /// failures are MCP tool results with `isError: true`. The `Option` in the
    /// success tuple carries the search audit facts (search calls only) for
    /// the opt-in operator audit; error tuples are plain `(code, message)`.
    fn tool_call(&self, params: &Value) -> Result<(Value, Option<SearchAudit>), (i64, String)> {
        let object = match params.as_object() {
            Some(object) if object.get("name").and_then(Value::as_str).is_some() => object,
            _ => {
                return Err((
                    -32602,
                    "invalid params: tools/call requires a string tool name".into(),
                ));
            }
        };
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let empty = Map::new();
        let arguments: &Map<String, Value> = match object.get("arguments") {
            Some(Value::Object(map)) => map,
            Some(_) => {
                return Err((-32602, "invalid params: arguments must be an object".into()));
            }
            None => &empty,
        };
        let validation = match name.as_str() {
            "search_project_knowledge" => self.validate_search_args(arguments),
            "read_project_knowledge" => self.validate_read_args(arguments),
            "related_project_knowledge" => self.validate_related_args(arguments),
            "project_knowledge_status" => self.validate_status_args(arguments),
            "refresh_project_knowledge" => self.validate_refresh_args(arguments),
            other => return Err((-32602, format!("unknown tool: {other}"))),
        };
        if let Err(message) = validation {
            return Err((-32602, format!("invalid params for {name}: {message}")));
        }
        let (outcome, search_audit) = match name.as_str() {
            "search_project_knowledge" => match self.call_search(arguments) {
                Ok((value, audit)) => (Ok(value), Some(audit)),
                Err((message, audit)) => (Err(message), Some(audit)),
            },
            "read_project_knowledge" => (self.call_read(arguments), None),
            "related_project_knowledge" => (self.call_related(arguments), None),
            "project_knowledge_status" => (self.call_status(arguments), None),
            "refresh_project_knowledge" => (self.call_refresh(arguments), None),
            other => return Err((-32602, format!("unknown tool: {other}"))),
        };
        match outcome {
            Ok(value) => {
                let text = value.to_string();
                if text.len() > MAX_TOOL_OUTPUT_BYTES {
                    return Ok((
                        tool_error(&format!(
                            "tool result exceeded the server output cap ({MAX_TOOL_OUTPUT_BYTES} bytes); reduce maxResults or maxOutputTokens"
                        )),
                        search_audit,
                    ));
                }
                Ok((
                    json!({ "content": [ { "type": "text", "text": text } ] }),
                    search_audit,
                ))
            }
            Err(message) => Ok((tool_error(&message), search_audit)),
        }
    }

    /// Validates search arguments against the advertised input schema
    /// (required fields, types, enums, numeric bounds, additionalProperties).
    fn validate_search_args(&self, args: &Map<String, Value>) -> Result<(), String> {
        validate_required_string(args, "query")?;
        validate_known(
            args,
            &[
                "query",
                "pathFilter",
                "role",
                "maxResults",
                "maxOutputTokens",
                "cursor",
                "mode",
                "rrfK",
            ],
        )?;
        if let Some(value) = args.get("pathFilter") {
            validate_string(value, "pathFilter")?;
        }
        if let Some(value) = args.get("role") {
            validate_string(value, "role")?;
            if Role::parse(value.as_str().unwrap()).is_none() {
                return Err(
                    "role must be one of: instruction, current-doc, decision, plan, execution-record, historical, code, config"
                        .into(),
                );
            }
        }
        validate_u32(args, "maxResults", 1, Some(20))?;
        validate_u64(args, "maxOutputTokens", 1)?;
        if let Some(value) = args.get("cursor") {
            validate_string(value, "cursor")?;
        }
        if let Some(value) = args.get("mode") {
            validate_string(value, "mode")?;
            if !matches!(
                value.as_str().unwrap(),
                "lexical" | "hybrid" | "vectors-only"
            ) {
                return Err("mode must be one of: lexical, hybrid, vectors-only".into());
            }
        }
        validate_u32(args, "rrfK", 1, None)?;
        self.check_scope(args)
    }

    /// Validates read arguments against the advertised input schema.
    fn validate_read_args(&self, args: &Map<String, Value>) -> Result<(), String> {
        validate_required_string(args, "sourceId")?;
        validate_known(args, &["sourceId", "maxOutputTokens"])?;
        validate_u64(args, "maxOutputTokens", 1)?;
        self.check_scope(args)
    }

    /// Validates related arguments against the advertised input schema.
    fn validate_related_args(&self, args: &Map<String, Value>) -> Result<(), String> {
        validate_required_string(args, "sourceId")?;
        validate_known(args, &["sourceId", "relations", "limit", "maxOutputTokens"])?;
        if let Some(value) = args.get("relations") {
            let Some(items) = value.as_array() else {
                return Err("relations must be an array of relation-kind strings".into());
            };
            for item in items {
                let Some(kind) = item.as_str() else {
                    return Err(format!("relations items must be strings, found {item}"));
                };
                if RelationKind::parse(kind).is_none() {
                    return Err(format!(
                        "unknown relation kind: {kind} (expected references, referenced-by, children, or parent)"
                    ));
                }
            }
        }
        validate_u32(args, "limit", 1, Some(20))?;
        validate_u64(args, "maxOutputTokens", 1)?;
        self.check_scope(args)
    }

    /// Validates status arguments (scope verification only).
    fn validate_status_args(&self, args: &Map<String, Value>) -> Result<(), String> {
        validate_known(args, &[])?;
        self.check_scope(args)
    }

    /// Validates refresh arguments (scope verification only).
    fn validate_refresh_args(&self, args: &Map<String, Value>) -> Result<(), String> {
        validate_known(args, &[])?;
        self.check_scope(args)
    }

    /// Type-checks the optional scope arguments and validates them against
    /// the server-configured scope. Opaque ids are references, not
    /// authorization: every operation rechecks the configured scope.
    fn check_scope(&self, args: &Map<String, Value>) -> Result<(), String> {
        if let Some(value) = args.get("repoId") {
            let repo = match value.as_str() {
                Some(repo) => repo,
                None => return Err("repoId must be a string".into()),
            };
            if repo != self.context.repo_id {
                return Err(format!(
                    "scope mismatch: this server is bound to repo {}, refusing {repo}",
                    self.context.repo_id
                ));
            }
        }
        if let Some(value) = args.get("worktreeId") {
            let worktree = match value.as_str() {
                Some(worktree) => worktree,
                None => return Err("worktreeId must be a string".into()),
            };
            if worktree != self.context.worktree_id {
                return Err(format!(
                    "scope mismatch: this server is bound to worktree {}, refusing {worktree}",
                    self.context.worktree_id
                ));
            }
        }
        if let Some(value) = args.get("snapshotMode") {
            let mode = match value.as_str() {
                Some(mode) => mode,
                None => return Err("snapshotMode must be a string".into()),
            };
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

    /// The audit `server_start` record: the scope and configuration this
    /// server is bound to (correlation only; no request content).
    fn start_record(&self) -> Value {
        json!({
            "schemaVersion": audit::AUDIT_SCHEMA_VERSION,
            "t": repoise_core::bench::runner::now_utc(),
            "kind": "server_start",
            "repoiseVersion": env!("CARGO_PKG_VERSION"),
            "server": {
                "repoId": self.context.repo_id,
                "worktreeId": self.context.worktree_id,
                "snapshotMode": match self.context.mode {
                    SnapshotMode::WorkingTree => "working-tree",
                    SnapshotMode::Committed => "committed",
                    SnapshotMode::PlainDirectory => "plain-directory",
                },
                "pinnedSearchMode": self.search_mode.map(search_mode_label),
                "refreshEnabled": self.allow_refresh
            }
        })
    }

    /// Runs the search service. The returned `SearchAudit` records the
    /// server-measured requested/effective retrieval mode and served
    /// generation — never the query text, filters or results.
    fn call_search(
        &self,
        args: &Map<String, Value>,
    ) -> Result<(Value, SearchAudit), (String, SearchAudit)> {
        let unknown = SearchAudit {
            requested: None,
            effective: None,
            served_generation: None,
        };
        let fail = |message: String| -> (String, SearchAudit) { (message, unknown.clone()) };
        self.check_scope(args).map_err(fail)?;
        let query = required_string(args, "query");
        if query.trim().is_empty() {
            return Err(fail("query must not be empty".into()));
        }
        let path_filter = string_arg(args, "pathFilter");
        let role_filter = match string_arg(args, "role").as_deref() {
            Some(name) => Some(
                Role::parse(name)
                    .ok_or_else(|| format!(
                        "unknown role: {name} (expected instruction, current-doc, decision, plan, execution-record, historical, code, or config)"
                    ))
                    .map_err(fail)?,
            ),
            None => None,
        };
        let max_results = u32_value(args, "maxResults");
        let max_output_tokens = u64_value(args, "maxOutputTokens");
        let cursor = string_arg(args, "cursor");
        let requested = match string_arg(args, "mode").as_deref() {
            Some("hybrid") => Some(repoise_core::search::SearchMode::Hybrid),
            Some("vectors-only") => Some(repoise_core::search::SearchMode::VectorsOnly),
            Some("lexical") => Some(repoise_core::search::SearchMode::Lexical),
            None => None,
            // Unreachable: the `mode` enum is validated upstream.
            Some(_) => None,
        };
        // A pinned server mode (--search-mode) is the enforced retrieval
        // mode: omitting `mode` uses it; requesting a different one is a
        // scope mismatch, never a silent downgrade or upgrade.
        let mode = match (self.search_mode, requested) {
            (Some(pinned), Some(requested)) if requested != pinned => {
                let (pinned, requested) = (search_mode_label(pinned), search_mode_label(requested));
                return Err(fail(format!(
                    "scope mismatch: this server is bound to search mode {pinned}, refusing {requested}"
                )));
            }
            (Some(pinned), _) => pinned,
            (None, requested) => requested.unwrap_or(repoise_core::search::SearchMode::Lexical),
        };
        let requested_mode = Some(search_mode_label(mode).to_string());
        let not_completed = || SearchAudit {
            requested: requested_mode.clone(),
            effective: None,
            served_generation: None,
        };
        let rrf_k = u32_value(args, "rrfK");
        let query_embedder = if mode != repoise_core::search::SearchMode::Lexical {
            match crate::query_embedder_from_config(&self.context.effective) {
                Ok(provider) => provider,
                Err(err) => return Err((err.to_string(), not_completed())),
            }
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
        .map_err(|err| (err.to_string(), not_completed()))?;
        let audit = SearchAudit {
            requested: requested_mode,
            effective: Some(response.retrieval_mode.clone()),
            served_generation: Some(response.generation_id),
        };
        match serde_json::to_value(&response) {
            Ok(value) => Ok((value, audit)),
            Err(err) => Err((err.to_string(), audit)),
        }
    }

    fn call_read(&self, args: &Map<String, Value>) -> Result<Value, String> {
        self.check_scope(args)?;
        let source_id = required_string(args, "sourceId");
        let max_output_tokens = u64_value(args, "maxOutputTokens");
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
        let source_id = required_string(args, "sourceId");
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
        let limit = u32_value(args, "limit");
        let max_output_tokens = u64_value(args, "maxOutputTokens");
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

    fn call_refresh(&self, args: &Map<String, Value>) -> Result<Value, String> {
        self.check_scope(args)?;
        if !self.allow_refresh {
            return Err(
                "refresh_project_knowledge is not enabled: this server is read-only. Enable it with the mcp.refresh configuration (repoise.local.json / repoise.config.json) or --allow-refresh."
                    .into(),
            );
        }
        // One writer per scope: the refresh runs the same bounded,
        // transactional index operation as `repoise index`, under the scope's
        // ownership lock; a concurrent `repoise index`/`watch` writer refuses
        // the call before any provider work or publication.
        let scope_dir = self
            .context
            .cache
            .worktree_dir(&self.context.repo_id, &self.context.worktree_id);
        let _lock = repoise_core::ownership::ScopeLock::acquire(&scope_dir, "mcp-refresh")
            .map_err(|err| err.to_string())?;
        let client = crate::embedding_client(
            &self.context.effective,
            repoise_core::embed::CancellationToken::new(),
        )?;
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
            None,
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

/// The retrieval mode's wire label.
fn search_mode_label(mode: repoise_core::search::SearchMode) -> &'static str {
    match mode {
        repoise_core::search::SearchMode::Lexical => "lexical",
        repoise_core::search::SearchMode::Hybrid => "hybrid",
        repoise_core::search::SearchMode::VectorsOnly => "vectors-only",
    }
}

/// Description for the search tool's `mode` property, noting the pinned
/// server mode (`--search-mode`) when one is set.
fn search_mode_description(pinned: Option<repoise_core::search::SearchMode>) -> String {
    match pinned {
        Some(pinned) => format!(
            "Retrieval mode. This server is pinned to {}; requests naming a \
             different mode are refused.",
            search_mode_label(pinned)
        ),
        None => "Retrieval mode (default lexical)".to_string(),
    }
}

/// The fixed audit label for a tool name: only the known tool vocabulary
/// is ever recorded. The raw client-supplied `params.name` (untrusted,
/// unbounded and attacker-controllable) never reaches the sink.
fn audit_tool_label(name: Option<&str>) -> &'static str {
    match name {
        Some("search_project_knowledge") => "search_project_knowledge",
        Some("read_project_knowledge") => "read_project_knowledge",
        Some("related_project_knowledge") => "related_project_knowledge",
        Some("project_knowledge_status") => "project_knowledge_status",
        Some("refresh_project_knowledge") => "refresh_project_knowledge",
        _ => "unknown",
    }
}

/// One audit `tool_call` record: only measurements the server took (tool
/// name, requested/effective retrieval mode, degradation, success and
/// duration). Never the request arguments or result content.
fn tool_call_record(
    tool: Option<&str>,
    invocation: &Result<(Value, Option<SearchAudit>), (i64, String)>,
    duration_ms: u128,
) -> Value {
    let (success, search) = match invocation {
        Ok((result, search)) => {
            // A tool result with `isError: true` is a failed tool call
            // (scope mismatch, service error, output cap), not a success.
            let is_error = result.get("isError").and_then(Value::as_bool) == Some(true);
            (!is_error, search.as_ref())
        }
        Err(_) => (false, None),
    };
    let requested = search.and_then(|search| search.requested.clone());
    let effective = search.and_then(|search| search.effective.clone());
    let served_generation = search.and_then(|search| search.served_generation);
    let degraded = match search {
        Some(search) => match (search.requested.as_deref(), search.effective.as_deref()) {
            (Some(requested), Some(effective)) => Some(requested != effective),
            _ => None,
        },
        None => None,
    };
    let degradation_reason = match (degraded, effective.as_deref()) {
        (Some(true), Some("lexical")) => Some("degraded_to_lexical".to_string()),
        (Some(true), _) => Some("degraded".to_string()),
        _ => None,
    };
    json!({
        "schemaVersion": audit::AUDIT_SCHEMA_VERSION,
        "t": repoise_core::bench::runner::now_utc(),
        "kind": "tool_call",
        "tool": audit_tool_label(tool),
        "requestedMode": requested,
        "effectiveMode": effective,
        "degraded": degraded,
        "degradationReason": degradation_reason,
        "servedGeneration": served_generation,
        "success": success,
        "durationMs": duration_ms.min(u64::MAX as u128) as u64
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

/// Validates that `args` only contains properties advertised by the tool
/// (all input schemas set `additionalProperties: false`). Scope-verification
/// properties are allowed on every tool.
fn validate_known(args: &Map<String, Value>, known: &[&str]) -> Result<(), String> {
    for key in args.keys() {
        let known = known.contains(&key.as_str())
            || matches!(key.as_str(), "repoId" | "worktreeId" | "snapshotMode");
        if !known {
            return Err(format!(
                "unknown property: {key} (it is not part of this tool's input schema)"
            ));
        }
    }
    Ok(())
}

/// Validates a required string argument (present, string, non-empty).
fn validate_required_string(args: &Map<String, Value>, name: &str) -> Result<(), String> {
    match args.get(name) {
        Some(Value::String(value)) if !value.is_empty() => Ok(()),
        Some(Value::String(_)) => Err(format!("{name} must not be empty")),
        Some(_) => Err(format!("{name} must be a string")),
        None => Err(format!("{name} is required")),
    }
}

/// Validates an optional argument is a string (type check only).
fn validate_string(value: &Value, name: &str) -> Result<(), String> {
    match value.as_str() {
        Some(_) => Ok(()),
        None => Err(format!("{name} must be a string")),
    }
}

/// Validates an optional integer argument (u32) within advertised bounds
/// (`minimum` defaults to 1, matching `minimum: 1` in the schemas).
fn validate_u32(
    args: &Map<String, Value>,
    name: &str,
    minimum: u32,
    maximum: Option<u32>,
) -> Result<(), String> {
    let Some(value) = args.get(name) else {
        return Ok(());
    };
    if let Some(int) = value.as_i64()
        && int < 0
    {
        return Err(format!("{name} must be at least {minimum}"));
    }
    let Some(number) = value.as_u64().and_then(|number| u32::try_from(number).ok()) else {
        return Err(format!("{name} must be an integer"));
    };
    if number < minimum {
        return Err(format!("{name} must be at least {minimum}"));
    }
    if let Some(maximum) = maximum
        && number > maximum
    {
        return Err(format!("{name} must be at most {maximum}"));
    }
    Ok(())
}

/// Validates an optional integer argument (u64) within advertised bounds
/// (`minimum` is 1, matching `minimum: 1` in the schemas).
fn validate_u64(args: &Map<String, Value>, name: &str, minimum: u64) -> Result<(), String> {
    let Some(value) = args.get(name) else {
        return Ok(());
    };
    if let Some(int) = value.as_i64()
        && int < 0
    {
        return Err(format!("{name} must be at least {minimum}"));
    }
    let Some(number) = value.as_u64() else {
        return Err(format!("{name} must be an integer"));
    };
    if number < minimum {
        return Err(format!("{name} must be at least {minimum}"));
    }
    Ok(())
}

/// Required string argument (validated to be a non-empty string before use).
fn required_string(args: &Map<String, Value>, name: &str) -> String {
    args.get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Optional string argument (validated to be a string before use).
fn string_arg(args: &Map<String, Value>, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(str::to_string)
}

/// Optional integer argument (u32; validated within advertised bounds).
fn u32_value(args: &Map<String, Value>, name: &str) -> Option<u32> {
    args.get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

/// Optional integer argument (u64; validated within advertised bounds).
fn u64_value(args: &Map<String, Value>, name: &str) -> Option<u64> {
    args.get(name).and_then(Value::as_u64)
}
