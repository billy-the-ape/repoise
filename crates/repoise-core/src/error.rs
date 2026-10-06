//! Shared engine error type.

use std::error::Error as StdError;
use std::fmt;

/// Errors surfaced by shared engine services.
#[derive(Debug)]
pub enum Error {
    /// Filesystem or subprocess I/O failure.
    Io(std::io::Error),
    /// Source-adapter failure (see [`crate::adapter::AdapterError`] for details).
    Adapter(crate::adapter::AdapterError),
    /// Configuration is missing, unparsable, or fails validation.
    Config(String),
    /// A requested path violates policy or containment.
    Policy(String),
    /// An `init`/overlay operation failed.
    Init(String),
    /// JSON parsing or serialization failure.
    Json(String),
    /// Persistent index (SQLite) failure.
    Sqlite(rusqlite::Error),
    /// The index is missing or stale for the requested scope.
    IndexState(String),
    /// The exact source changed since the index was built (validation failed).
    Stale {
        /// Relative path that is stale.
        path: String,
        /// Current content hash when re-read.
        revision_hash: String,
        /// Why validation failed.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => write!(f, "I/O error: {err}"),
            Error::Adapter(err) => write!(f, "adapter error: {err}"),
            Error::Config(msg) => write!(f, "config error: {msg}"),
            Error::Policy(msg) => write!(f, "policy error: {msg}"),
            Error::Init(msg) => write!(f, "init error: {msg}"),
            Error::Json(msg) => write!(f, "json error: {msg}"),
            Error::Sqlite(err) => write!(f, "index storage error: {err}"),
            Error::IndexState(msg) => write!(f, "index state error: {msg}"),
            Error::Stale { path, reason, .. } => {
                write!(f, "stale source ({path}): {reason}")
            }
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            Error::Sqlite(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<crate::adapter::AdapterError> for Error {
    fn from(err: crate::adapter::AdapterError) -> Self {
        Error::Adapter(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Json(err.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::Sqlite(err)
    }
}
