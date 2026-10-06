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
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(err) => Some(err),
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
