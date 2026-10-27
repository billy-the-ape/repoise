//! Version/capabilities handshake document (AP-02).
//!
//! `repoise capabilities` is the machine handshake an automation layer
//! uses before driving the CLI: version, event-stream protocol and schema,
//! build features, the command/machine-output matrix, benchmark
//! availability, the stable exit-code table and the compatibility policy.
//! The JSON document (`--json`) is the authoritative form; the default
//! output is a compact human summary.

use serde_json::{Value, json};

use crate::events::{EVENT_SCHEMA_VERSION, EVENT_STREAM_SCHEMA_FILE};

/// Schema version of the capabilities document.
pub const CAPABILITIES_SCHEMA_VERSION: u32 = 1;
/// Checked-in schema file for the capabilities document.
pub const CAPABILITIES_SCHEMA_FILE: &str = "schemas/capabilities.v1.schema.json";

/// Builds the capabilities document for this build.
/// `heartbeat_ms` is the effective heartbeat interval in milliseconds.
pub fn document(heartbeat_ms: u64) -> Value {
    json!({
        "schemaVersion": CAPABILITIES_SCHEMA_VERSION,
        "name": "repoise",
        "version": env!("CARGO_PKG_VERSION"),
        "eventStream": {
            "schema": EVENT_STREAM_SCHEMA_FILE,
            "version": EVENT_SCHEMA_VERSION,
            "operations": ["index", "watch"],
            "heartbeatIntervalMs": heartbeat_ms,
        },
        "features": features(),
        "commands": commands(),
        "benchmark": {
            "supported": true,
            "command": "bench",
            "docs": "docs/bench.md",
            "note": "Suite-driven benchmark coordinator (AP-04): native orchestration; each trial's agent process runs through the optional AP-03 Node host bridge. Exit codes: 0 completed, 1 operational failure, 2 usage, 3 preflight/suite failure, 4 interrupted.",
        },
        "exitCodes": {
            "ok": 0,
            "operational": 1,
            "usage": 2,
            "indexMissingOrStale": 3,
            "coverageNotMet": 4,
            "benchPreflight": 3,
            "benchInterrupted": 4,
        },
        "compatibility": {
            "policy": "additive",
            "supportedEventStreamVersions": [EVENT_SCHEMA_VERSION],
            "notes": [
                "Consumers must ignore unknown event types and unknown fields.",
                "Consumers must reject an event stream whose schemaVersion they do not support.",
                "Breaking changes bump the event stream version; additive events and fields do not.",
                format!("This capabilities document is defined by {CAPABILITIES_SCHEMA_FILE}.")
            ]
        }
    })
}

/// Build features and always-present capabilities.
fn features() -> Vec<Value> {
    let mut features = vec![json!("event-stream"), json!("mcp-stdio")];
    if cfg!(feature = "remote-embedding") {
        features.push(json!("remote-embedding"));
    }
    if cfg!(feature = "github-enrichment") {
        features.push(json!("github-enrichment"));
    }
    features
}

/// Command list with the machine output modes each supports
/// (`json`, `events`, or `mcp-stdio`).
fn commands() -> Value {
    let mut entries: Vec<Value> = Vec::new();
    let add = |entries: &mut Vec<Value>, name: &str, machine: Vec<Value>| {
        entries.push(json!({ "name": name, "machine": machine }));
    };
    add(&mut entries, "index", vec![json!("json"), json!("events")]);
    add(&mut entries, "watch", vec![json!("json"), json!("events")]);
    add(&mut entries, "doctor", vec![json!("json")]);
    add(&mut entries, "explain", vec![json!("json")]);
    add(&mut entries, "status", vec![json!("json")]);
    add(&mut entries, "search", vec![json!("json")]);
    add(&mut entries, "read", vec![json!("json")]);
    add(&mut entries, "related", vec![json!("json")]);
    add(&mut entries, "check", vec![json!("json")]);
    add(&mut entries, "init", vec![json!("json")]);
    add(&mut entries, "overlay", vec![json!("json")]);
    add(&mut entries, "purge", vec![json!("json")]);
    add(&mut entries, "capabilities", vec![json!("json")]);
    add(&mut entries, "mcp", vec![json!("mcp-stdio")]);
    add(&mut entries, "bench", vec![json!("json")]);
    add(&mut entries, "greet", Vec::new());
    add(&mut entries, "help", Vec::new());
    add(&mut entries, "version", Vec::new());
    Value::Array(entries)
}

/// Renders the document as a compact human summary.
pub fn print_human(doc: &Value) {
    println!("repoise {}", doc["version"]);
    println!(
        "event stream: v{} (operations: {}; heartbeat every {} ms; schema: {})",
        doc["eventStream"]["version"],
        doc["eventStream"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(", "),
        doc["eventStream"]["heartbeatIntervalMs"],
        EVENT_STREAM_SCHEMA_FILE
    );
    println!(
        "features: {}",
        doc["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "benchmark: {} (command: {})",
        if doc["benchmark"]["supported"] == true {
            "supported"
        } else {
            "not available in this build"
        },
        doc["benchmark"]["command"]
    );
    let codes = &doc["exitCodes"];
    println!(
        "exit codes: 0 ok; 1 operational; 2 usage; 3 index missing/stale (bench preflight/suite); 4 coverage not met (bench interruption) ({}..{})",
        codes["ok"], codes["coverageNotMet"]
    );
}
