//! Opt-in machine audit sink for the stdio MCP server.
//!
//! When an operator (typically the benchmark coordinator) designates a file
//! with `repoise mcp --audit-file <PATH>`, the server appends one bounded,
//! newline-delimited JSON record per lifecycle and tool event. The file
//! carries only what the server itself measured: correlation/scope fields,
//! tool name, requested and effective retrieval mode, degradation, success
//! and duration. It never contains prompt or query text, excerpts, source
//! bodies, credentials, environment values or unrestricted RPC payloads.
//!
//! stdout always carries JSON-RPC protocol traffic only. Audit diagnostics
//! go to stderr, and a sink that cannot be opened or written is visible
//! there while the server continues — the absence of audit records is never
//! silently read as evidence of no tool use (the benchmark coordinator
//! labels that state `unavailable`, not zero use).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

/// Audit record schema version.
pub const AUDIT_SCHEMA_VERSION: u32 = 1;

/// Bound on one serialized audit record; an oversized record is replaced by
/// a fixed-size `audit_error` marker instead of being written.
pub const MAX_AUDIT_RECORD_BYTES: usize = 8 * 1024;

/// One append-only NDJSON audit file.
pub struct Sink {
    writer: Mutex<File>,
}

impl Sink {
    /// Opens the sink file for append (creating it if needed). The parent
    /// directory must already exist. Existing records are preserved: a
    /// re-spawned server appends to the same stream instead of truncating
    /// the earlier `server_start`/`tool_call` records. A sink path that is
    /// itself a symlink is refused (the sink is a coordinator-owned file
    /// and must not be redirected elsewhere); a failure here is an
    /// `io::Error` the caller reports.
    pub fn create(path: &Path) -> std::io::Result<Self> {
        if let Ok(meta) = fs::symlink_metadata(path)
            && meta.file_type().is_symlink()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("audit sink path is a symlink: {}", path.display()),
            ));
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: Mutex::new(file),
        })
    }

    /// Appends one bounded record and flushes it, so a killed server still
    /// leaves its completed records on disk. Serialization and write
    /// failures are reported to stderr (visible) and never panic or abort
    /// the server.
    pub fn record(&self, event: Value) {
        let mut line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(err) => {
                eprintln!("repoise mcp: audit record serialization failed: {err}");
                return;
            }
        };
        if line.len() > MAX_AUDIT_RECORD_BYTES {
            line = serde_json::json!({
                "schemaVersion": AUDIT_SCHEMA_VERSION,
                "kind": "audit_error",
                "reason": "record_size_exceeded",
                "maxBytes": MAX_AUDIT_RECORD_BYTES
            })
            .to_string();
        }
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = writer
            .write_all(line.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| writer.flush());
        if let Err(err) = result {
            eprintln!(
                "repoise mcp: audit write failed (the server continues; this event \
                 is not recorded): {err}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("repoise-audit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_are_bounded_single_line_json() {
        let dir = temp_dir("bounded");
        let path = dir.join("audit.ndjson");
        let sink = Sink::create(&path).unwrap();
        sink.record(json!({
            "schemaVersion": AUDIT_SCHEMA_VERSION,
            "kind": "tool_call",
            "tool": "search_project_knowledge",
            "success": true,
            "durationMs": 12
        }));
        // An oversized record is replaced by the fixed-size marker.
        sink.record(json!({
            "schemaVersion": AUDIT_SCHEMA_VERSION,
            "kind": "tool_call",
            "tool": "x".repeat(64 * 1024)
        }));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() <= MAX_AUDIT_RECORD_BYTES);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["kind"], "tool_call");
        assert_eq!(first["tool"], "search_project_knowledge");
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["kind"], "audit_error");
        assert_eq!(second["reason"], "record_size_exceeded");
        assert!(second.to_string().len() <= MAX_AUDIT_RECORD_BYTES);
    }

    #[test]
    fn create_fails_when_the_parent_directory_is_missing() {
        let dir = temp_dir("missing-parent");
        let path = dir.join("no-such-dir").join("audit.ndjson");
        assert!(Sink::create(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn reopened_sink_appends_without_truncating() {
        // A re-spawned server in the same trial appends to the existing
        // stream; the earlier records survive.
        let dir = temp_dir("append");
        let path = dir.join("audit.ndjson");
        {
            let sink = Sink::create(&path).unwrap();
            sink.record(json!({ "schemaVersion": AUDIT_SCHEMA_VERSION, "kind": "server_start" }));
        }
        {
            let sink = Sink::create(&path).unwrap();
            sink.record(json!({
                "schemaVersion": AUDIT_SCHEMA_VERSION,
                "kind": "tool_call",
                "tool": "search_project_knowledge",
                "success": true
            }));
        }
        let lines: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["kind"], "server_start");
        assert_eq!(lines[1]["kind"], "tool_call");
    }

    #[test]
    fn create_refuses_a_symlink_sink() {
        let dir = temp_dir("symlink");
        let target = dir.join("target.ndjson");
        std::fs::write(&target, b"").unwrap();
        let link = dir.join("link.ndjson");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_file;
            symlink_file(&target, &link).unwrap();
        }
        let err = match Sink::create(&link) {
            Err(err) => err,
            Ok(_) => panic!("a symlink sink must be refused"),
        };
        assert!(err.to_string().contains("symlink"));
        // The target must not have been touched.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
    }
}
