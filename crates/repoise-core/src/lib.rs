//! Shared Repoise engine services.
//!
//! This crate provides the neutral source-adapter and provenance contracts, the
//! versioned `repoise.config.json` configuration layer, the explainable
//! include/exclude/secret policy, deterministic discovery/inventory, the
//! `init`/`doctor` services and the first offline docs milestone: persistent
//! SQLite/FTS5 index storage with transactional generation publication,
//! structural prose chunking, incremental reconciliation, lexical search and
//! exact reads. Optional hybrid embedding search (operator-selected providers
//! behind a versioned interface, content-addressed vector cache, RRF fusion)
//! degrades to the offline lexical baseline without provider credentials.
//! The CLI (and a future MCP adapter) are thin adapters over
//! these services; see `docs/architecture.md` for boundaries.

pub mod adapter;
pub mod cache;
pub mod chunk;
pub mod classify;
pub mod config;
pub mod discovery;
pub mod doctor;
pub mod embed;
pub mod error;
pub mod hash;
pub mod history;
pub mod ignore;
pub mod indexing;
pub mod init;
pub mod provenance;
pub mod purge;
pub mod read;
pub mod search;
pub mod status;
pub mod store;

/// Human-readable project identity used by the native CLI.
pub const NAME: &str = "Repoise";

/// Committed root configuration file name.
pub const CONFIG_FILENAME: &str = "repoise.config.json";
/// Untracked local override file name (operator-only; must never be committed).
pub const LOCAL_CONFIG_FILENAME: &str = "repoise.local.json";
/// Manifest file that records the files `repoise init` manages.
pub const OVERLAY_FILENAME: &str = "repoise.overlay.json";
/// Default generated-data directory, always excluded from the corpus.
pub const DEFAULT_CACHE_DIR: &str = ".repoise";
/// Version of the files and blocks `repoise init` writes.
pub const TEMPLATE_VERSION: &str = "1.0.0";
/// Default maximum file size accepted into the corpus (1 MiB).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Versioned JSON schema file published alongside the package.
pub const CONFIG_SCHEMA_FILE: &str = "schemas/repoise.config.v1.schema.json";

/// Version of the persistent index schema (SQLite tables + FTS layout).
/// Version 2 adds the per-generation `chunk_vec` vector table (card K3);
/// version 3 adds code corpus columns, per-generation symbol and reference
/// tables and the generation embedding scope (card K4); version 4 adds the
/// per-generation `history_item` records and `history_fts` lane (card K5).
pub const INDEX_SCHEMA_VERSION: i64 = 4;
/// How many complete published generations a scope retains (current + previous).
pub const GENERATION_RETENTION: i64 = 2;
/// Environment variable that overrides the resolved cache root (tests/containers).
pub const CACHE_ENV_VAR: &str = "REPOISE_CACHE_DIR";

/// Engine error type shared by all core services.
pub type Result<T> = std::result::Result<T, error::Error>;
