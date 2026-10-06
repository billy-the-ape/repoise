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
  vector_profile TEXT
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
  body,
  tokenize='unicode61'
);
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

/// Additive migration from schema version 1 (card K2) to version 2 (card K3):
/// adds the per-generation `chunk_vec` vector table and the generation
/// vector columns. Existing databases are upgraded in place; only
/// brand-new databases are created with the full schema above.
const MIGRATION_SQL_V1_TO_V2: &str = r#"
ALTER TABLE generation ADD COLUMN vectors INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generation ADD COLUMN vector_profile TEXT;
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
            "1" => {
                // Additive card-K3 upgrade: vector table and columns.
                conn.execute_batch(MIGRATION_SQL_V1_TO_V2)?;
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
    /// 1-based inclusive source line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Inclusive byte range in the snapshot file.
    pub byte_start: u64,
    pub byte_end: u64,
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
             vectors, vector_profile \
             FROM generation WHERE repo_id = ?1 AND worktree_id = ?2 AND id = ?3",
        )
        .map_err(Error::Sqlite)?;
    let Some(meta) = row
        .query_row(params![repo_id, worktree_id, current], |r| {
            Ok(GenerationMeta {
                generation_id: r.get(0)?,
                snapshot_id: r.get(1)?,
                snapshot_mode: r.get(2)?,
                revision_id: r.get(3)?,
                manifest_hash: r.get(4)?,
                config_fingerprint: r.get(5)?,
                parser_fingerprint: r.get(6)?,
                built_at_ms: r.get(7)?,
                files: r.get(8)?,
                chunks: r.get(9)?,
                vectors: r.get(10)?,
                vector_profile: r.get(11)?,
            })
        })
        .optional()
        .map_err(Error::Sqlite)?
    else {
        return Err(Error::IndexState(
            "current_generation points at a missing generation".into(),
        ));
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
             built_at_ms, state, files, chunks, vectors, vector_profile) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'building', ?10, ?11, ?12, ?13)",
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
            ],
        )
        .map_err(Error::Sqlite)?;
        let generation_id = conn.last_insert_rowid();
        for file in &input.files {
            conn.execute(
                "INSERT INTO file (generation_id, path, content_hash, role, lifecycle, \
                 classification_source, language, parser_version, corpus, size) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    file.size as i64,
                ],
            )
            .map_err(Error::Sqlite)?;
        }
        for chunk in &input.chunks {
            conn.execute(
                "INSERT INTO chunk (generation_id, chunk_id, parent_chunk_id, path, \
                 heading_path, corpus, text, text_hash, line_start, line_end, \
                 byte_start, byte_end) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    generation_id,
                    chunk.chunk_id,
                    chunk.parent_chunk_id,
                    chunk.path,
                    chunk.heading_path,
                    chunk.corpus,
                    chunk.text,
                    chunk.text_hash,
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
                "INSERT INTO chunk_fts (chunk_id, path, heading, symbol, body) \
                 VALUES (?1, ?2, ?3, '', ?4)",
                params![chunk.chunk_id, chunk.path, chunk.heading_path, chunk.text],
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
                 language, parser_version, corpus, size \
                 FROM file WHERE generation_id = ?1 ORDER BY path",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let size: i64 = r.get(8)?;
                Ok(FileRow {
                    path: r.get(0)?,
                    content_hash: r.get(1)?,
                    role: r.get(2)?,
                    lifecycle: r.get(3)?,
                    classification_source: r.get(4)?,
                    language: r.get(5)?,
                    parser_version: r.get(6)?,
                    corpus: r.get(7)?,
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
                 text, text_hash, line_start, line_end, byte_start, byte_end \
                 FROM chunk WHERE generation_id = ?1 ORDER BY path, line_start, chunk_id",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(params![meta.generation_id], |r| {
                let line_start: i64 = r.get(7)?;
                let line_end: i64 = r.get(8)?;
                let byte_start: i64 = r.get(9)?;
                let byte_end: i64 = r.get(10)?;
                Ok(StoredChunk {
                    chunk_id: r.get(0)?,
                    parent_chunk_id: r.get(1)?,
                    path: r.get(2)?,
                    heading_path: r.get(3)?,
                    corpus: r.get(4)?,
                    text: r.get(5)?,
                    text_hash: r.get(6)?,
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
    Ok(Some(PreviousGeneration {
        meta,
        files,
        chunks,
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
