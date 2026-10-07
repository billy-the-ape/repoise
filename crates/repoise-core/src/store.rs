//! Persistent index storage: SQLite with FTS5 and transactional generations.
//!
//! One database per scope (`repos/<repoId>/worktrees/<worktreeId>/index.sqlite`
//! under the resolved cache root). A build writes a complete new generation —
//! files, chunks and the current-generation FTS rows — inside a single
//! transaction; readers see either the old or the new complete generation.
//! Retention keeps the current plus the previous complete generation.
//!
//! `chunk_fts` holds only the current generation's rows (rebuilt inside the
//! publication transaction) so lexical search never mixes generations.
//!
//! SQLite opens in WAL mode so concurrent readers work while a build
//! publishes; the database directory (including `-wal`/`-shm` sidecars) is
//! the unit of movement/removal.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::error::Error;
use crate::{GENERATION_RETENTION, INDEX_SCHEMA_VERSION};

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS generation (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  repo_id TEXT NOT NULL,
  worktree_id TEXT NOT NULL,
  snapshot_id TEXT NOT NULL,
  snapshot_mode TEXT NOT NULL,
  revision_id TEXT,
  manifest_hash TEXT NOT NULL,
  config_fingerprint TEXT NOT NULL,
  parser_fingerprint TEXT NOT NULL,
  built_at_ms INTEGER NOT NULL,
  state TEXT NOT NULL,
  files INTEGER NOT NULL,
  chunks INTEGER NOT NULL,
  vectors INTEGER NOT NULL DEFAULT 0,
  vector_profile TEXT,
  embedding_scope TEXT NOT NULL DEFAULT 'docs',
  history INTEGER NOT NULL DEFAULT 0,
  history_meta TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_generation_scope
  ON generation(repo_id, worktree_id, id);
CREATE TABLE IF NOT EXISTS file (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  role TEXT NOT NULL,
  lifecycle TEXT NOT NULL,
  classification_source TEXT NOT NULL,
  language TEXT NOT NULL,
  parser_version TEXT,
  corpus TEXT,
  parser_errors INTEGER NOT NULL DEFAULT 0,
  size INTEGER NOT NULL,
  PRIMARY KEY (generation_id, path)
);
CREATE TABLE IF NOT EXISTS chunk (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  chunk_id TEXT NOT NULL,
  parent_chunk_id TEXT,
  path TEXT NOT NULL,
  heading_path TEXT NOT NULL,
  corpus TEXT NOT NULL,
  text TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  symbol TEXT NOT NULL DEFAULT '',
  context TEXT,
  line_start INTEGER NOT NULL,
  line_end INTEGER NOT NULL,
  byte_start INTEGER NOT NULL,
  byte_end INTEGER NOT NULL,
  PRIMARY KEY (generation_id, chunk_id)
);
CREATE INDEX IF NOT EXISTS idx_chunk_scope_path
  ON chunk(generation_id, path, line_start);
CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(
  chunk_id UNINDEXED,
  path,
  heading,
  symbol,
  context,
  body,
  tokenize='unicode61'
);
CREATE TABLE IF NOT EXISTS symbol (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  symbol_id TEXT NOT NULL,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  line_start INTEGER NOT NULL,
  line_end INTEGER NOT NULL,
  parent_symbol_id TEXT,
  exported INTEGER NOT NULL DEFAULT 0,
  chunk_id TEXT,
  PRIMARY KEY (generation_id, symbol_id)
);
CREATE INDEX IF NOT EXISTS idx_symbol_scope_name
  ON symbol(generation_id, path, name);
CREATE TABLE IF NOT EXISTS reference (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  ref_id TEXT NOT NULL,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence TEXT NOT NULL,
  line INTEGER NOT NULL,
  chunk_id TEXT,
  target_symbol_id TEXT,
  PRIMARY KEY (generation_id, ref_id)
);
CREATE INDEX IF NOT EXISTS idx_reference_scope
  ON reference(generation_id, path);
CREATE TABLE IF NOT EXISTS chunk_vec (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  chunk_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  dimension INTEGER NOT NULL,
  vector BLOB NOT NULL,
  PRIMARY KEY (generation_id, chunk_id)
);
CREATE INDEX IF NOT EXISTS idx_chunk_vec_profile
  ON chunk_vec(generation_id, fingerprint);
CREATE TABLE IF NOT EXISTS history_item (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  item_id TEXT NOT NULL,
  revision_id TEXT NOT NULL,
  parents TEXT NOT NULL DEFAULT '[]',
  message TEXT NOT NULL,
  author TEXT,
  committed_at_ms INTEGER,
  affected_paths TEXT NOT NULL DEFAULT '[]',
  paths_truncated INTEGER NOT NULL DEFAULT 0,
  hunks TEXT NOT NULL DEFAULT '[]',
  hunks_truncated INTEGER NOT NULL DEFAULT 0,
  pr_hint TEXT,
  host_metadata TEXT,
  PRIMARY KEY (generation_id, item_id)
);
CREATE INDEX IF NOT EXISTS idx_history_scope
  ON history_item(generation_id, revision_id);
CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(
  item_id UNINDEXED,
  revision_id,
  message,
  paths,
  host,
  tokenize='unicode61'
);
"#;

/// Additive migration from schema version 2 (card K3) to version 3 (card K4):
/// code corpus columns, per-generation symbol and reference tables and the
/// generation embedding scope. The FTS `context` column cannot be altered in
/// place, so `Store::open` rebuilds the FTS table when it is missing (the
/// table serves only the current generation, so no data is lost — context
/// is backfilled as empty until the next publication).
const MIGRATION_TABLES_V2_TO_V3: &str = r#"
CREATE TABLE IF NOT EXISTS symbol (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  symbol_id TEXT NOT NULL,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  line_start INTEGER NOT NULL,
  line_end INTEGER NOT NULL,
  parent_symbol_id TEXT,
  exported INTEGER NOT NULL DEFAULT 0,
  chunk_id TEXT,
  PRIMARY KEY (generation_id, symbol_id)
);
CREATE INDEX IF NOT EXISTS idx_symbol_scope_name
  ON symbol(generation_id, path, name);
CREATE TABLE IF NOT EXISTS reference (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  ref_id TEXT NOT NULL,
  path TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence TEXT NOT NULL,
  line INTEGER NOT NULL,
  chunk_id TEXT,
  target_symbol_id TEXT,
  PRIMARY KEY (generation_id, ref_id)
);
CREATE INDEX IF NOT EXISTS idx_reference_scope
  ON reference(generation_id, path);
"#;
/// Additive migration from schema version 1 (card K2) to version 2 (card K3):
/// adds the per-generation `chunk_vec` vector table and the generation
/// vector columns. Existing databases are upgraded in place; only
/// brand-new databases are created with the full schema above.
const MIGRATION_TABLES_V1_TO_V2: &str = r#"
CREATE TABLE IF NOT EXISTS chunk_vec (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  chunk_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  dimension INTEGER NOT NULL,
  vector BLOB NOT NULL,
  PRIMARY KEY (generation_id, chunk_id)
);
CREATE INDEX IF NOT EXISTS idx_chunk_vec_profile
  ON chunk_vec(generation_id, fingerprint);
"#;
/// Additive migration from schema version 3 (card K4) to version 4 (card K5):
/// the per-generation `history_item` records and the separate `history_fts`
/// lane. Existing databases are upgraded in place; the lane is empty until
/// the next history-enabled publication.
const MIGRATION_TABLES_V3_TO_V4: &str = r#"
CREATE TABLE IF NOT EXISTS history_item (
  generation_id INTEGER NOT NULL REFERENCES generation(id) ON DELETE CASCADE,
  item_id TEXT NOT NULL,
  revision_id TEXT NOT NULL,
  parents TEXT NOT NULL DEFAULT '[]',
  message TEXT NOT NULL,
  author TEXT,
  committed_at_ms INTEGER,
  affected_paths TEXT NOT NULL DEFAULT '[]',
  paths_truncated INTEGER NOT NULL DEFAULT 0,
  hunks TEXT NOT NULL DEFAULT '[]',
  hunks_truncated INTEGER NOT NULL DEFAULT 0,
  pr_hint TEXT,
  host_metadata TEXT,
  PRIMARY KEY (generation_id, item_id)
);
CREATE INDEX IF NOT EXISTS idx_history_scope
  ON history_item(generation_id, revision_id);
CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(
  item_id UNINDEXED,
  revision_id,
  message,
  paths,
  host,
  tokenize='unicode61'
);
"#;
/// Adds one column to an existing table unless it already exists (table and
/// column names are compile-time constants of the migrations).
fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(Error::Sqlite)?;
    let mut has = false;
    for row in rows {
        if row.map_err(Error::Sqlite)? == column {
            has = true;
            break;
        }
    }
    if !has {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))
        .map_err(Error::Sqlite)?;
    }
    Ok(())
}

/// Handle to one scope's index database. Cheap; connections open per
/// operation so readers and publishers can share the scope concurrently.
#[derive(Clone, Debug)]
pub struct Store {
    /// Database file path.
    pub path: PathBuf,
}

impl Store {
    /// Creates a store handle (the database file is created on first open).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Opens the database, creating the schema if absent, and verifies the
    /// stored schema version matches this engine.
    pub fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&self.path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA_SQL)?;
        // Version seeding, check and migration run inside one transaction so a
        // concurrent open or a crash can never leave a half-migrated database
        // (a rollback restores the previous state).
        conn.execute("BEGIN IMMEDIATE", [])?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![INDEX_SCHEMA_VERSION.to_string()],
        )?;
        let stored: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(|err| Error::IndexState(err.to_string()))?;
        match stored.as_str() {
            version if version == INDEX_SCHEMA_VERSION.to_string().as_str() => {
                conn.execute("COMMIT", [])?;
            }
            "1" | "2" | "3" | "4" => {
                // Additive card-K3 upgrade (only for v1 databases).
                if stored == "1" {
                    conn.execute_batch(MIGRATION_TABLES_V1_TO_V2)?;
                    ensure_column(&conn, "generation", "vectors", "INTEGER NOT NULL DEFAULT 0")?;
                    ensure_column(&conn, "generation", "vector_profile", "TEXT")?;
                }
                // Additive card-K4 upgrade.
                conn.execute_batch(MIGRATION_TABLES_V2_TO_V3)?;
                ensure_column(
                    &conn,
                    "generation",
                    "embedding_scope",
                    "TEXT NOT NULL DEFAULT 'docs'",
                )?;
                ensure_column(&conn, "file", "parser_errors", "INTEGER NOT NULL DEFAULT 0")?;
                ensure_column(&conn, "chunk", "symbol", "TEXT NOT NULL DEFAULT ''")?;
                ensure_column(&conn, "chunk", "context", "TEXT")?;
                // Additive card-K5 upgrade (history lane).
                conn.execute_batch(MIGRATION_TABLES_V3_TO_V4)?;
                ensure_column(&conn, "generation", "history", "INTEGER NOT NULL DEFAULT 0")?;
                // Additive card-K5 upgrade: persisted history lane coverage
                // summary (gaps and enrichment outcome) for status/search.
                ensure_column(
                    &conn,
                    "generation",
                    "history_meta",
                    "TEXT NOT NULL DEFAULT ''",
                )?;
                rebuild_chunk_fts_if_needed(&conn)?;
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![INDEX_SCHEMA_VERSION.to_string()],
                )?;
                conn.execute("COMMIT", [])?;
            }
            _ => {
                let _ = conn.execute("ROLLBACK", []);
                return Err(Error::IndexState(format!(
                    "index schema version {stored} is incompatible with engine version {}; rebuild the scope",
                    INDEX_SCHEMA_VERSION
                )));
            }
        }
        Ok(conn)
    }

    /// Reports whether this build of SQLite supports FTS5 (doctor check).
    pub fn fts5_supported() -> Result<bool> {
        let conn = Connection::open_in_memory()?;
        match conn.execute("CREATE VIRTUAL TABLE fts_probe USING fts5(x)", []) {
            Ok(_) => {
                let _ = conn.execute("DROP TABLE fts_probe", []);
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }
}
/// Rebuilds `chunk_fts` when it predates the v3 `context` column, preserving
/// the current generation's rows (context backfilled as empty; the next
/// publication rebuilds the table fully).
fn rebuild_chunk_fts_if_needed(conn: &Connection) -> Result<()> {
    let has_context: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('chunk_fts') WHERE name = 'context'",
            [],
            |row| row.get(0),
        )
        .map_err(Error::Sqlite)?;
    if has_context > 0 {
        return Ok(());
    }
    conn.execute("ALTER TABLE chunk_fts RENAME TO chunk_fts_v2", [])
        .map_err(Error::Sqlite)?;
    conn.execute(
        "CREATE VIRTUAL TABLE chunk_fts USING fts5(
           chunk_id UNINDEXED, path, heading, symbol, context, body,
           tokenize='unicode61')",
        [],
    )
    .map_err(Error::Sqlite)?;
    conn.execute(
        "INSERT INTO chunk_fts (chunk_id, path, heading, symbol, context, body)
         SELECT chunk_id, path, heading, symbol, '', body FROM chunk_fts_v2",
        [],
    )
    .map_err(Error::Sqlite)?;
    conn.execute("DROP TABLE chunk_fts_v2", [])
        .map_err(Error::Sqlite)?;
    Ok(())
}

/// A file record to publish in a generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    /// Repository-relative path (POSIX separators).
    pub path: String,
    /// SHA-256 of the file content in the snapshot.
    pub content_hash: String,
    /// Classification role.
    pub role: String,
    /// Classification lifecycle.
    pub lifecycle: String,
    /// How the classification was determined.
    pub classification_source: String,
    /// Detected language.
    pub language: String,
    /// Chunker version (None for files not chunked in this PR).
    pub parser_version: Option<String>,
    /// Logical corpus (None for code files until PR 4).
    pub corpus: Option<String>,
    /// Parser-error range count recorded for the file (code files only).
    pub parser_errors: u32,
    /// File size in bytes.
    pub size: u64,
}

/// A chunk record to publish in a generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkRow {
    /// Opaque chunk id (content/structure/scope addressed).
    pub chunk_id: String,
    /// Opaque parent chunk id for labeled splits.
    pub parent_chunk_id: Option<String>,
    /// Repository-relative path (POSIX separators).
    pub path: String,
    /// Heading ancestry joined with " > " (empty at document root).
    pub heading_path: String,
    /// Logical corpus.
    pub corpus: String,
    /// Chunk text (labeled splits include the synthetic context line).
    pub text: String,
    /// SHA-256 of the chunk text.
    pub text_hash: String,
    /// Primary defined symbol (empty for module/fallback chunks).
    pub symbol: String,
    /// Parent/signature context for a labeled split (metadata only).
    pub context: Option<String>,
    /// 1-based inclusive source line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Inclusive byte range in the snapshot file.
    pub byte_start: u64,
    pub byte_end: u64,
}

/// A defined symbol record to publish in a generation (per generation,
/// carried forward unchanged for unchanged files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolRow {
    /// Opaque symbol id (stable per scope path/name/position).
    pub symbol_id: String,
    /// Repository-relative path (POSIX separators).
    pub path: String,
    /// Symbol name.
    pub name: String,
    /// Declaration kind (`function`, `method`, `class`, ...).
    pub kind: String,
    /// 1-based inclusive source line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Enclosing symbol id, if any.
    pub parent_symbol_id: Option<String>,
    /// Module-level export.
    pub exported: bool,
    /// Chunk id covering the symbol's declaration.
    pub chunk_id: Option<String>,
}

/// A syntactic reference edge record to publish in a generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceRow {
    /// Opaque reference id (stable per scope).
    pub ref_id: String,
    /// Repository-relative path of the referencing file.
    pub path: String,
    /// Referenced name.
    pub name: String,
    /// Edge kind (`import`, `reexport`, `call`, `reference`).
    pub kind: String,
    /// Edge confidence (`certain` | `uncertain`).
    pub confidence: String,
    /// Line of the reference.
    pub line: u32,
    /// Chunk covering the reference.
    pub chunk_id: Option<String>,
    /// Resolved target symbol id, if resolution succeeded.
    pub target_symbol_id: Option<String>,
}

/// A bounded history-lane record to publish in a generation (card K5).
/// JSON-encoded collections mirror the column layout of `history_item`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryRow {
    /// Opaque history item id (stable per scope and revision).
    pub item_id: String,
    /// Adapter-qualified opaque revision id.
    pub revision_id: String,
    /// Optional parent revision ids (adapter-qualified).
    pub parents: Vec<String>,
    /// Commit message (secret shapes redacted).
    pub message: String,
    /// Author name, when recorded.
    pub author: Option<String>,
    /// Commit time, milliseconds since the Unix epoch.
    pub committed_at_ms: Option<i64>,
    /// Bounded affected-path list (POSIX separators).
    pub affected_paths: Vec<String>,
    /// Number of affected paths omitted by the bound.
    pub paths_truncated: u32,
    /// Optional bounded diff hunk descriptors.
    pub hunks: Vec<crate::history::HunkDescriptor>,
    /// Number of hunks omitted by the bound.
    pub hunks_truncated: u32,
    /// Unverified change-request hints parsed from the message.
    pub pr_hint: Option<Vec<u32>>,
    /// Optional verified host/change-request association (host metadata).
    pub association: Option<crate::history::PrAssociation>,
}

/// A vector record to publish in a generation (one profile per generation).
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkVecRow {
    /// Opaque chunk id (must belong to this generation's chunks).
    pub chunk_id: String,
    /// Full embedding profile fingerprint the vector belongs to.
    pub fingerprint: String,
    /// SHA-256 of the normalized embedding input that produced it.
    pub input_hash: String,
    /// Vector dimension.
    pub dimension: u32,
    /// Validated vector (dimension and finiteness checked before use).
    pub vector: Vec<f32>,
}

/// Inputs for one transactional generation publication.
#[derive(Clone, Debug)]
pub struct GenerationInput {
    /// Stable repository scope id.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Snapshot record identity.
    pub snapshot_id: String,
    /// Snapshot mode (`working-tree` | `committed` | `plain-directory`).
    pub snapshot_mode: String,
    /// Adapter-qualified opaque revision id.
    pub revision_id: Option<String>,
    /// Content manifest hash of the inventory.
    pub manifest_hash: String,
    /// Effective config fingerprint.
    pub config_fingerprint: String,
    /// Combined parser fingerprint.
    pub parser_fingerprint: String,
    /// Build time in milliseconds since the Unix epoch.
    pub built_at_ms: i64,
    /// Complete file record set (every inventoried file).
    pub files: Vec<FileRow>,
    /// Complete chunk record set (every chunk of every chunked file).
    pub chunks: Vec<ChunkRow>,
    /// Validated vector records for this generation (one profile; empty when
    /// the generation is lexical-only).
    pub vectors: Vec<ChunkVecRow>,
    /// Full embedding profile fingerprint when vectors are present.
    pub vector_profile: Option<String>,
    /// Which chunks the enabled profile applies to (card K4).
    pub embedding_scope: crate::embed::EmbeddingScope,
    /// Complete symbol record set (carried forward per unchanged file).
    pub symbols: Vec<SymbolRow>,
    /// Complete reference edge record set (carried forward per unchanged file).
    pub references: Vec<ReferenceRow>,
    /// Complete bounded history-lane record set (empty when the lane is off
    /// or unavailable for this scope).
    pub history: Vec<HistoryRow>,
    /// Persisted history-lane coverage/enrichment summary (JSON; empty when
    /// the lane is off), surfaced by `status` and history search.
    pub history_meta: String,
}

/// Metadata of the current published generation for a scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationMeta {
    /// Generation id (monotonic per database).
    pub generation_id: i64,
    /// Snapshot record identity.
    pub snapshot_id: String,
    /// Snapshot mode.
    pub snapshot_mode: String,
    /// Adapter-qualified opaque revision id.
    pub revision_id: Option<String>,
    /// Content manifest hash of the inventory.
    pub manifest_hash: String,
    /// Effective config fingerprint at build time.
    pub config_fingerprint: String,
    /// Combined parser fingerprint at build time.
    pub parser_fingerprint: String,
    /// Build time in milliseconds since the Unix epoch.
    pub built_at_ms: i64,
    /// File record count.
    pub files: i64,
    /// Chunk record count.
    pub chunks: i64,
    /// Vector record count (0 for lexical-only generations).
    pub vectors: i64,
    /// Full embedding profile fingerprint when vectors are present.
    pub vector_profile: Option<String>,
    /// Which chunks the enabled profile applies to (card K4).
    pub embedding_scope: crate::embed::EmbeddingScope,
    /// History-lane item count (0 when the lane is off or unavailable).
    pub history_items: i64,
    /// Persisted history-lane coverage/enrichment summary (JSON; empty when
    /// the lane is off or the generation predates the column).
    pub history_meta: String,
}

/// Reads the current published generation metadata for a scope, if any.
pub fn current_generation(
    conn: &Connection,
    repo_id: &str,
    worktree_id: &str,
) -> Result<Option<GenerationMeta>> {
    let Some(current) = meta_value(conn, "current_generation")? else {
        return Ok(None);
    };
    let Ok(current) = current.parse::<i64>() else {
        return Err(Error::IndexState(format!(
            "corrupt current_generation: {current}"
        )));
    };
    let mut row = conn
        .prepare(
            "SELECT id, snapshot_id, snapshot_mode, revision_id, manifest_hash, \
             config_fingerprint, parser_fingerprint, built_at_ms, files, chunks, \
             vectors, vector_profile, embedding_scope, history, history_meta \
             FROM generation WHERE repo_id = ?1 AND worktree_id = ?2 AND id = ?3",
        )
        .map_err(Error::Sqlite)?;
    let Some(meta_row) = row
        .query_row(params![repo_id, worktree_id, current], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, String>(14)?,
            ))
        })
        .optional()
        .map_err(Error::Sqlite)?
    else {
        return Err(Error::IndexState(
            "current_generation points at a missing generation".into(),
        ));
    };
    let (
        generation_id,
        snapshot_id,
        snapshot_mode,
        revision_id,
        manifest_hash,
        config_fingerprint,
        parser_fingerprint,
        built_at_ms,
        files,
        chunks,
        vectors,
        vector_profile,
        scope,
        history_items,
        history_meta,
    ) = meta_row;
    let meta = GenerationMeta {
        generation_id,
        snapshot_id,
        snapshot_mode,
        revision_id,
        manifest_hash,
        config_fingerprint,
        parser_fingerprint,
        built_at_ms,
        files,
        chunks,
        vectors,
        vector_profile,
        embedding_scope: crate::embed::EmbeddingScope::parse(&scope)
            .ok_or_else(|| Error::IndexState(format!("unknown stored embedding scope: {scope}")))?,
        history_items,
        history_meta,
    };
    Ok(Some(meta))
}

/// Publishes one complete generation atomically and switches the scope's
/// current-generation pointer inside the same transaction. A failure before
/// commit leaves the previous generation untouched; retained generations
/// beyond the bounded retention window are garbage-collected afterwards.
pub fn publish(conn: &Connection, input: &GenerationInput) -> Result<i64> {
    conn.execute("BEGIN IMMEDIATE", []).map_err(Error::Sqlite)?;
    let result = (|| -> Result<i64> {
        conn.execute(
            "INSERT INTO generation (repo_id, worktree_id, snapshot_id, snapshot_mode, \
             revision_id, manifest_hash, config_fingerprint, parser_fingerprint, \
             built_at_ms, state, files, chunks, vectors, vector_profile, embedding_scope, history, history_meta) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'building', ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                input.repo_id,
                input.worktree_id,
                input.snapshot_id,
                input.snapshot_mode,
                input.revision_id,
                input.manifest_hash,
                input.config_fingerprint,
                input.parser_fingerprint,
                input.built_at_ms,
                input.files.len() as i64,
                input.chunks.len() as i64,
                input.vectors.len() as i64,
                input.vector_profile.as_deref(),
                input.embedding_scope.as_str(),
                input.history.len() as i64,
                &input.history_meta,
            ],
        )
        .map_err(Error::Sqlite)?;
        let generation_id = conn.last_insert_rowid();
        for file in &input.files {
            conn.execute(
                "INSERT INTO file (generation_id, path, content_hash, role, lifecycle, \
                 classification_source, language, parser_version, corpus, parser_errors, size) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    generation_id,
                    file.path,
                    file.content_hash,
                    file.role,
                    file.lifecycle,
                    file.classification_source,
                    file.language,
                    file.parser_version,
                    file.corpus,
                    file.parser_errors as i64,
                    file.size as i64,
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        for chunk in &input.chunks {
            conn.execute(
                "INSERT INTO chunk (generation_id, chunk_id, parent_chunk_id, path, \
                 heading_path, corpus, text, text_hash, symbol, context, line_start, \
                 line_end, byte_start, byte_end) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    generation_id,
                    chunk.chunk_id,
                    chunk.parent_chunk_id,
                    chunk.path,
                    chunk.heading_path,
                    chunk.corpus,
                    chunk.text,
                    chunk.text_hash,
                    chunk.symbol,
                    chunk.context,
                    chunk.line_start as i64,
                    chunk.line_end as i64,
                    chunk.byte_start as i64,
                    chunk.byte_end as i64,
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        // The FTS table serves exactly the current generation: rebuild it
        // inside this transaction so readers see old or new rows, never a mix.
        conn.execute("DELETE FROM chunk_fts", [])
            .map_err(Error::Sqlite)?;
        for chunk in &input.chunks {
            conn.execute(
                "INSERT INTO chunk_fts (chunk_id, path, heading, symbol, context, body) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    chunk.chunk_id,
                    chunk.path,
                    chunk.heading_path,
                    chunk.symbol,
                    chunk.context.as_deref().unwrap_or(""),
                    chunk.text,
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        // Symbols and references are per-generation records: validated against
        // this generation's chunks and each other before insertion so a
        // failed publish never leaves dangling edges.
        let chunk_ids: std::collections::HashSet<&str> = input
            .chunks
            .iter()
            .map(|chunk| chunk.chunk_id.as_str())
            .collect();
        let symbol_ids: std::collections::HashSet<&str> = input
            .symbols
            .iter()
            .map(|symbol| symbol.symbol_id.as_str())
            .collect();
        for symbol in &input.symbols {
            if let Some(parent) = symbol
                .parent_symbol_id
                .as_deref()
                .filter(|parent| !symbol_ids.contains(parent))
            {
                return Err(Error::IndexState(format!(
                    "symbol {} references unknown parent {parent}",
                    symbol.symbol_id
                )));
            }
            if let Some(chunk_id) = symbol
                .chunk_id
                .as_deref()
                .filter(|chunk_id| !chunk_ids.contains(chunk_id))
            {
                return Err(Error::IndexState(format!(
                    "symbol {} points at unknown chunk {chunk_id}",
                    symbol.symbol_id
                )));
            }
            conn.execute(
                "INSERT INTO symbol (generation_id, symbol_id, path, name, kind, \
                 line_start, line_end, parent_symbol_id, exported, chunk_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    generation_id,
                    symbol.symbol_id,
                    symbol.path,
                    symbol.name,
                    symbol.kind,
                    symbol.line_start as i64,
                    symbol.line_end as i64,
                    symbol.parent_symbol_id,
                    symbol.exported as i64,
                    symbol.chunk_id,
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        for reference in &input.references {
            if let Some(chunk_id) = reference
                .chunk_id
                .as_deref()
                .filter(|chunk_id| !chunk_ids.contains(chunk_id))
            {
                return Err(Error::IndexState(format!(
                    "reference {} points at unknown chunk {chunk_id}",
                    reference.ref_id
                )));
            }
            if let Some(target) = reference
                .target_symbol_id
                .as_deref()
                .filter(|target| !symbol_ids.contains(target))
            {
                return Err(Error::IndexState(format!(
                    "reference {} resolves to unknown symbol {target}",
                    reference.ref_id
                )));
            }
            conn.execute(
                "INSERT INTO reference (generation_id, ref_id, path, name, kind, \
                 confidence, line, chunk_id, target_symbol_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    generation_id,
                    reference.ref_id,
                    reference.path,
                    reference.name,
                    reference.kind,
                    reference.confidence,
                    reference.line as i64,
                    reference.chunk_id,
                    reference.target_symbol_id,
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        // The history lane publishes in the same transaction: bounded local
        // records (card K5) plus its own FTS table rebuilt for exactly this
        // generation, so history search never mixes generations.
        conn.execute("DELETE FROM history_fts", [])
            .map_err(Error::Sqlite)?;
        for item in &input.history {
            conn.execute(
                "INSERT INTO history_item (generation_id, item_id, revision_id, \
                  parents, message, author, committed_at_ms, affected_paths, \
                  paths_truncated, hunks, hunks_truncated, pr_hint, host_metadata) \
                  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    generation_id,
                    item.item_id,
                    item.revision_id,
                    serde_json::to_string(&item.parents).map_err(|err| Error::Json(format!(
                        "history serialization failed: {err}"
                    )))?,
                    item.message,
                    item.author,
                    item.committed_at_ms,
                    serde_json::to_string(&item.affected_paths).map_err(|err| Error::Json(
                        format!("history serialization failed: {err}")
                    ))?,
                    item.paths_truncated as i64,
                    serde_json::to_string(&item.hunks).map_err(|err| Error::Json(format!(
                        "history serialization failed: {err}"
                    )))?,
                    item.hunks_truncated as i64,
                    item.pr_hint
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|err| Error::Json(format!("history serialization failed: {err}")))?
                        .unwrap_or_default(),
                    item.association
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|err| Error::Json(format!("history serialization failed: {err}")))?
                        .unwrap_or_default(),
                ],
            )
            .map_err(Error::Sqlite)?;
            conn.execute(
                "INSERT INTO history_fts (item_id, revision_id, message, paths, host) \
                  VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    item.item_id,
                    item.revision_id,
                    item.message,
                    item.affected_paths.join(" "),
                    // Index only the readable host text (title/body/state),
                    // never the raw association JSON (its keys would become
                    // searchable terms).
                    crate::history::association_fts_text(item.association.as_ref()),
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        // Vectors publish in the same transaction: they are per-generation
        // and keyed by this generation's chunk ids, so they can never mix
        // generations or serve replaced text.
        let chunk_ids: std::collections::HashSet<&str> = input
            .chunks
            .iter()
            .map(|chunk| chunk.chunk_id.as_str())
            .collect();
        for vector in &input.vectors {
            if !chunk_ids.contains(vector.chunk_id.as_str()) {
                return Err(Error::IndexState(format!(
                    "vector for unknown chunk in this generation: {}",
                    vector.chunk_id
                )));
            }
            if !crate::embed::validate_vector(&vector.vector, vector.dimension) {
                return Err(Error::IndexState(format!(
                    "invalid vector for chunk {}: expected {} finite components",
                    vector.chunk_id, vector.dimension
                )));
            }
            conn.execute(
                "INSERT INTO chunk_vec (generation_id, chunk_id, fingerprint, input_hash, \
                 dimension, vector) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    generation_id,
                    vector.chunk_id,
                    vector.fingerprint,
                    vector.input_hash,
                    vector.dimension as i64,
                    crate::embed::encode_vector(&vector.vector),
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        conn.execute(
            "UPDATE generation SET state = 'published' WHERE id = ?1",
            params![generation_id],
        )
        .map_err(Error::Sqlite)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('current_generation', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![generation_id.to_string()],
        )
        .map_err(Error::Sqlite)?;
        Ok(generation_id)
    })();
    match result {
        Ok(generation_id) => {
            conn.execute("COMMIT", []).map_err(Error::Sqlite)?;
            retain_generations(conn, &input.repo_id, &input.worktree_id)?;
            Ok(generation_id)
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(err)
        }
    }
}

/// Deletes published generations beyond the bounded retention window.
fn retain_generations(conn: &Connection, repo_id: &str, worktree_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM generation WHERE repo_id = ?1 AND worktree_id = ?2 AND id NOT IN \
         (SELECT id FROM generation WHERE repo_id = ?1 AND worktree_id = ?2 \
          ORDER BY id DESC LIMIT ?3)",
        params![repo_id, worktree_id, GENERATION_RETENTION],
    )
    .map_err(Error::Sqlite)?;
    Ok(())
}

/// A chunk from a previously published generation (for reconciliation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredChunk {
    /// Opaque chunk id.
    pub chunk_id: String,
    /// Parent chunk id for labeled splits.
    pub parent_chunk_id: Option<String>,
    /// Repository-relative path.
    pub path: String,
    /// Heading ancestry joined with " > ".
    pub heading_path: String,
    /// Logical corpus.
    pub corpus: String,
    /// Chunk text (labeled splits include the synthetic context line).
    pub text: String,
    /// SHA-256 of the chunk text.
    pub text_hash: String,
    /// Primary defined symbol.
    pub symbol: String,
    /// Parent/signature context for a labeled split.
    pub context: Option<String>,
    /// 1-based inclusive source line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Inclusive byte range.
    pub byte_start: u64,
    pub byte_end: u64,
}

/// The previously published generation for a scope: metadata, file records
/// and chunk records (including text, so reused chunks publish unchanged).
#[derive(Clone, Debug)]
pub struct PreviousGeneration {
    /// Generation metadata.
    pub meta: GenerationMeta,
    /// File records of the previous generation.
    pub files: Vec<FileRow>,
    /// Chunk records of the previous generation.
    pub chunks: Vec<StoredChunk>,
    /// Symbol records of the previous generation.
    pub symbols: Vec<SymbolRow>,
    /// Reference edge records of the previous generation.
    pub references: Vec<ReferenceRow>,
    /// History-lane records of the previous generation.
    pub history: Vec<HistoryRow>,
}

/// Loads the current published generation's records for a scope, if any.
pub fn load_generation(
    conn: &Connection,
    repo_id: &str,
    worktree_id: &str,
) -> Result<Option<PreviousGeneration>> {
    let Some(meta) = current_generation(conn, repo_id, worktree_id)? else {
        return Ok(None);
    };
    let mut files = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT path, content_hash, role, lifecycle, classification_source, \
                 language, parser_version, corpus, parser_errors, size \
                 FROM file WHERE generation_id = ?1 ORDER BY path",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let parser_errors: i64 = r.get(8)?;
                let size: i64 = r.get(9)?;
                Ok(FileRow {
                    path: r.get(0)?,
                    content_hash: r.get(1)?,
                    role: r.get(2)?,
                    lifecycle: r.get(3)?,
                    classification_source: r.get(4)?,
                    language: r.get(5)?,
                    parser_version: r.get(6)?,
                    corpus: r.get(7)?,
                    parser_errors: parser_errors as u32,
                    size: size as u64,
                })
            })
            .map_err(Error::Sqlite)?;
        for row in rows {
            files.push(row.map_err(Error::Sqlite)?);
        }
    }
    let mut chunks = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT chunk_id, parent_chunk_id, path, heading_path, corpus, \
                 text, text_hash, symbol, context, line_start, line_end, \
                 byte_start, byte_end \
                 FROM chunk WHERE generation_id = ?1 ORDER BY path, line_start, chunk_id",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let line_start: i64 = r.get(9)?;
                let line_end: i64 = r.get(10)?;
                let byte_start: i64 = r.get(11)?;
                let byte_end: i64 = r.get(12)?;
                Ok(StoredChunk {
                    chunk_id: r.get(0)?,
                    parent_chunk_id: r.get(1)?,
                    path: r.get(2)?,
                    heading_path: r.get(3)?,
                    corpus: r.get(4)?,
                    text: r.get(5)?,
                    text_hash: r.get(6)?,
                    symbol: r.get(7)?,
                    context: r.get(8)?,
                    line_start: line_start as u32,
                    line_end: line_end as u32,
                    byte_start: byte_start as u64,
                    byte_end: byte_end as u64,
                })
            })
            .map_err(Error::Sqlite)?;
        for row in rows {
            chunks.push(row.map_err(Error::Sqlite)?);
        }
    }
    let mut symbols = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT symbol_id, path, name, kind, line_start, line_end, \
                 parent_symbol_id, exported, chunk_id \
                 FROM symbol WHERE generation_id = ?1 ORDER BY path, line_start, symbol_id",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let line_start: i64 = r.get(4)?;
                let line_end: i64 = r.get(5)?;
                let exported: i64 = r.get(7)?;
                Ok(SymbolRow {
                    symbol_id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    kind: r.get(3)?,
                    line_start: line_start as u32,
                    line_end: line_end as u32,
                    parent_symbol_id: r.get(6)?,
                    exported: exported != 0,
                    chunk_id: r.get(8)?,
                })
            })
            .map_err(Error::Sqlite)?;
        for row in rows {
            symbols.push(row.map_err(Error::Sqlite)?);
        }
    }
    let mut references = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT ref_id, path, name, kind, confidence, line, chunk_id, \
                 target_symbol_id \
                 FROM reference WHERE generation_id = ?1 ORDER BY path, line, ref_id",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let line: i64 = r.get(5)?;
                Ok(ReferenceRow {
                    ref_id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    kind: r.get(3)?,
                    confidence: r.get(4)?,
                    line: line as u32,
                    chunk_id: r.get(6)?,
                    target_symbol_id: r.get(7)?,
                })
            })
            .map_err(Error::Sqlite)?;
        for row in rows {
            references.push(row.map_err(Error::Sqlite)?);
        }
    }
    let mut history = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT item_id, revision_id, parents, message, author, \
                  committed_at_ms, affected_paths, paths_truncated, hunks, \
                  hunks_truncated, pr_hint, host_metadata \
                  FROM history_item WHERE generation_id = ?1 \
                  ORDER BY committed_at_ms DESC, item_id",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let parents: String = r.get(2)?;
                let committed_at_ms: Option<i64> = r.get(5)?;
                let affected_paths: String = r.get(6)?;
                let paths_truncated: i64 = r.get(7)?;
                let hunks: String = r.get(8)?;
                let hunks_truncated: i64 = r.get(9)?;
                let pr_hint: String = r.get(10)?;
                let host_metadata: String = r.get(11)?;
                Ok(HistoryRow {
                    item_id: r.get(0)?,
                    revision_id: r.get(1)?,
                    parents: serde_json::from_str(&parents).unwrap_or_default(),
                    message: r.get(3)?,
                    author: r.get(4)?,
                    committed_at_ms,
                    affected_paths: serde_json::from_str(&affected_paths).unwrap_or_default(),
                    paths_truncated: paths_truncated as u32,
                    hunks: serde_json::from_str(&hunks).unwrap_or_default(),
                    hunks_truncated: hunks_truncated as u32,
                    pr_hint: serde_json::from_str(&pr_hint).unwrap_or_default(),
                    association: if host_metadata.trim().is_empty() {
                        None
                    } else {
                        serde_json::from_str(&host_metadata).unwrap_or_default()
                    },
                })
            })
            .map_err(Error::Sqlite)?;
        for row in rows {
            history.push(row.map_err(Error::Sqlite)?);
        }
    }
    Ok(Some(PreviousGeneration {
        meta,
        files,
        chunks,
        symbols,
        references,
        history,
    }))
}

/// Reads a `meta` value.
pub fn meta_value(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT value FROM meta WHERE key = ?1")
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(params![key], |r| r.get::<_, String>(0))
        .map_err(Error::Sqlite)?;
    let mut value = None;
    for row in rows {
        value = Some(row.map_err(Error::Sqlite)?);
    }
    Ok(value)
}

/// Records the last build error (surfaced by `status`); the index itself is
/// unaffected by a failed build.
pub fn record_last_error(conn: &Connection, message: &str, at_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('last_error', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![message],
    )
    .map_err(Error::Sqlite)?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('last_error_at', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![at_ms.to_string()],
    )
    .map_err(Error::Sqlite)?;
    Ok(())
}

/// Clears a previously recorded build error after a successful build.
pub fn clear_last_error(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM meta WHERE key IN ('last_error', 'last_error_at')",
        [],
    )
    .map_err(Error::Sqlite)?;
    Ok(())
}

/// The last recorded build error, if any.
pub fn last_error(conn: &Connection) -> Result<Option<String>> {
    meta_value(conn, "last_error")
}

/// (chunk count, vector count) for one generation and profile. A `None`
/// profile (lexical-only generation) yields a vector count of 0.
pub fn vector_coverage(
    conn: &Connection,
    generation_id: i64,
    fingerprint: Option<&str>,
) -> Result<(i64, i64)> {
    let chunks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunk WHERE generation_id = ?1",
            params![generation_id],
            |row| row.get(0),
        )
        .map_err(Error::Sqlite)?;
    let vectors: i64 = match fingerprint {
        Some(fingerprint) => conn
            .query_row(
                "SELECT COUNT(*) FROM chunk_vec WHERE generation_id = ?1 AND fingerprint = ?2",
                params![generation_id, fingerprint],
                |row| row.get(0),
            )
            .map_err(Error::Sqlite)?,
        None => 0,
    };
    Ok((chunks, vectors))
}

/// (docs chunk count, code chunk count) for one generation — the honest
/// basis for embedding-scope coverage (card K4).
pub fn corpus_chunk_counts(conn: &Connection, generation_id: i64) -> Result<(i64, i64)> {
    let (docs, code): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(corpus = 'docs'), 0), COALESCE(SUM(corpus = 'code'), 0) \
             FROM chunk WHERE generation_id = ?1",
            params![generation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(Error::Sqlite)?;
    Ok((docs, code))
}

/// (history item count, enriched item count) for one generation's history
/// lane (0/0 when the lane is off or unavailable).
pub fn history_lane_counts(conn: &Connection, generation_id: i64) -> Result<(i64, i64)> {
    let (items, enriched): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), \
             COALESCE(SUM(host_metadata IS NOT NULL AND host_metadata <> ''), 0) \
             FROM history_item WHERE generation_id = ?1",
            params![generation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(Error::Sqlite)?;
    Ok((items, enriched))
}

/// The newest revision recorded in one generation's history lane.
pub fn history_head_revision(conn: &Connection, generation_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT revision_id FROM history_item WHERE generation_id = ?1 \
         ORDER BY committed_at_ms DESC, item_id LIMIT 1",
        params![generation_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Error::Sqlite)
}

/// Distinct paths of chunks without a vector for one generation's profile
/// (ordered; empty for lexical-only generations).
pub fn pending_vector_paths(
    conn: &Connection,
    generation_id: i64,
    fingerprint: Option<&str>,
) -> Result<Vec<String>> {
    let Some(fingerprint) = fingerprint else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT c.path FROM chunk c \
             LEFT JOIN chunk_vec v \
               ON v.generation_id = c.generation_id AND v.chunk_id = c.chunk_id \
              AND v.fingerprint = ?2 \
             WHERE c.generation_id = ?1 AND v.chunk_id IS NULL \
             ORDER BY c.path",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(params![generation_id, fingerprint], |row| {
            row.get::<_, String>(0)
        })
        .map_err(Error::Sqlite)?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(row.map_err(Error::Sqlite)?);
    }
    Ok(paths)
}

/// Validated vectors of one generation and profile, by chunk id.
pub fn vectors_for_generation(
    conn: &Connection,
    generation_id: i64,
    fingerprint: &str,
) -> Result<std::collections::HashMap<String, Vec<f32>>> {
    let mut stmt = conn
        .prepare(
            "SELECT chunk_id, dimension, vector FROM chunk_vec \
             WHERE generation_id = ?1 AND fingerprint = ?2 \
             ORDER BY chunk_id",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(params![generation_id, fingerprint], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(Error::Sqlite)?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (chunk_id, dimension, bytes) = row.map_err(Error::Sqlite)?;
        match crate::embed::decode_vector(&bytes, dimension as u32) {
            Some(vector) => {
                out.insert(chunk_id, vector);
            }
            None => {
                return Err(Error::IndexState(format!(
                    "corrupt stored vector for chunk {chunk_id}"
                )));
            }
        }
    }
    Ok(out)
}

/// Reads one (chunk id, dimension, vector bytes) row from the vector queries.
fn read_vector_row(row: &rusqlite::Row) -> rusqlite::Result<(String, i64, Vec<u8>)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

/// Validated vectors of one generation and profile, by chunk id, restricted
/// to chunks whose path matches `path_filter` (SQLite GLOB) and/or whose file
/// role matches `role_filter` — the same semantics as the lexical query.
pub fn vectors_for_generation_filtered(
    conn: &Connection,
    generation_id: i64,
    fingerprint: &str,
    path_filter: Option<&str>,
    role_filter: Option<&str>,
) -> Result<std::collections::HashMap<String, Vec<f32>>> {
    let mut sql = String::from(
        "SELECT v.chunk_id, v.dimension, v.vector FROM chunk_vec v \
         JOIN chunk c ON c.generation_id = v.generation_id AND c.chunk_id = v.chunk_id \
         JOIN file fr ON fr.generation_id = v.generation_id AND fr.path = c.path \
         WHERE v.generation_id = ?1 AND v.fingerprint = ?2",
    );
    if path_filter.is_some() {
        sql.push_str(" AND c.path GLOB ?3");
    }
    if role_filter.is_some() {
        sql.push_str(if path_filter.is_some() {
            " AND fr.role = ?4"
        } else {
            " AND fr.role = ?3"
        });
    }
    sql.push_str(" ORDER BY v.chunk_id");
    let mut stmt = conn.prepare(&sql).map_err(Error::Sqlite)?;
    let rows = match (path_filter, role_filter) {
        (Some(path), Some(role)) => stmt
            .query_map(
                params![generation_id, fingerprint, path, role],
                read_vector_row,
            )
            .map_err(Error::Sqlite)?,
        (Some(path), None) => stmt
            .query_map(params![generation_id, fingerprint, path], read_vector_row)
            .map_err(Error::Sqlite)?,
        (None, Some(role)) => stmt
            .query_map(params![generation_id, fingerprint, role], read_vector_row)
            .map_err(Error::Sqlite)?,
        (None, None) => stmt
            .query_map(params![generation_id, fingerprint], read_vector_row)
            .map_err(Error::Sqlite)?,
    };
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (chunk_id, dimension, bytes) = row.map_err(Error::Sqlite)?;
        match crate::embed::decode_vector(&bytes, dimension as u32) {
            Some(vector) => {
                out.insert(chunk_id, vector);
            }
            None => {
                return Err(Error::IndexState(format!(
                    "corrupt stored vector for chunk {chunk_id}"
                )));
            }
        }
    }
    Ok(out)
}

/// The (input hash, fingerprint) pairs referenced by every retained
/// generation of one repository — the reference set for embedding-cache GC.
pub fn referenced_embedding_keys(
    conn: &Connection,
    repo_id: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT input_hash, fingerprint FROM chunk_vec \
             WHERE generation_id IN (SELECT id FROM generation WHERE repo_id = ?1)",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(params![repo_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(Error::Sqlite)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(Error::Sqlite)?);
    }
    Ok(out)
}
