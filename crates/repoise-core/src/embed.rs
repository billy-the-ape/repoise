//! Versioned embedding contracts, provider policy, and the shared
//! content-addressed embedding cache (master plan sections 3, 6, 7, 11).
//!
//! The offline lexical baseline does not depend on this module succeeding:
//! providers are operator-selected and optional, secrets resolve only via
//! environment references, and every failure mode (missing credential,
//! timeout, partial batch, budget exhaustion, cancellation) degrades to
//! lexical operation with an explicit coverage report. Pending or failed
//! chunks never serve vectors for replaced text, and successful vectors are
//! never replaced with zeros.
//!
//! Cache identity is content-addressed: normalized embedding input plus the
//! full embedding fingerprint (provider/model revision or operator artifact
//! id, dimension, profile/tokenizer version). Embeddings are indivisible per
//! chunk input — only chunks whose complete input hash or profile
//! fingerprint changed are re-embedded; a rename reuses a vector only when
//! the normalized input (which includes the chunk's context label) is
//! unchanged.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::Result;
use crate::hash;

/// Version of the embedding provider protocol (the versioned interface).
pub const EMBEDDING_PROTOCOL_VERSION: u32 = 1;
/// Version of the profile/tokenizer contract folded into the fingerprint.
pub const PROFILE_VERSION: u32 = 1;
/// Default reciprocal-rank-fusion `k` for hybrid search (configurable).
pub const DEFAULT_RRF_K: u32 = 60;
/// Bounded exact-cosine candidate cap per query (small indexes only).
pub const VECTOR_SCAN_TOP: usize = 200;

/// A versioned identity of an embedding space. Vectors from different
/// profiles live in separate collections and are never compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EmbeddingProfile {
    /// Provider name (operator-selected, e.g. `openai-compatible`).
    pub provider: String,
    /// Provider/model revision, or an operator artifact id for local models.
    pub model: String,
    /// Vector dimension this profile produces.
    pub dimension: u32,
    /// Profile/tokenizer contract version.
    pub profile_version: u32,
}

impl EmbeddingProfile {
    /// Full embedding fingerprint: provider/model revision, dimension and
    /// profile version. Cache keys and vector collections key on this.
    pub fn fingerprint(&self) -> String {
        let key = format!(
            "emb-v{}|{}|{}|dim={}",
            self.profile_version, self.provider, self.model, self.dimension
        );
        hash::sha256_hex(key)
    }

    /// Short human-readable name for reports.
    pub fn name(&self) -> String {
        format!("{}:{} (dim {})", self.provider, self.model, self.dimension)
    }
}

/// Which chunks an enabled embedding profile applies to.
///
/// `docs` keeps K3 behavior exactly (code stays lexical-only); `all` also
/// embeds code chunks (card K4). The scope is stored per generation so
/// vector coverage and pending counts stay honest when it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingScope {
    /// Docs corpus only (default).
    #[default]
    Docs,
    /// Docs and code corpora.
    All,
}

impl EmbeddingScope {
    /// Whether code chunks are embedded under this scope.
    pub fn includes_code(self) -> bool {
        matches!(self, Self::All)
    }

    /// Stable stored form of the scope.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docs => "docs",
            Self::All => "all",
        }
    }

    /// Parses the stored form (rejects unknown values).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "docs" => Some(Self::Docs),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Cooperative cancellation for embedding work. Checked between batches and
/// during backoff sleeps; an in-flight batch's result is discarded.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// A fresh, uncanceled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_canceled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Operator-configurable request/input budgets for one embedding session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EmbeddingBudgets {
    /// Maximum inputs per provider batch.
    pub batch_size: u32,
    /// Maximum provider batches per build.
    pub max_requests_per_build: u32,
    /// Maximum input characters accepted across one build.
    pub max_input_chars_per_build: u64,
    /// Per-batch request timeout.
    pub timeout: Duration,
    /// Retries after the first attempt for a failed batch.
    pub max_retries: u32,
    /// Initial backoff between retries.
    pub backoff_initial: Duration,
    /// Upper bound of the backoff.
    pub backoff_max: Duration,
}

impl Default for EmbeddingBudgets {
    fn default() -> Self {
        Self {
            batch_size: 32,
            max_requests_per_build: 500,
            max_input_chars_per_build: 1_000_000,
            timeout: Duration::from_millis(30_000),
            max_retries: 2,
            backoff_initial: Duration::from_millis(250),
            backoff_max: Duration::from_millis(5_000),
        }
    }
}

/// One batch result from a provider.
#[derive(Clone, Debug)]
pub struct EmbeddingBatch {
    /// One vector per input, in input order.
    pub vectors: Vec<Vec<f32>>,
    /// Model revision reported by the provider (recorded in cache records;
    /// falls back to the profile's model).
    pub model_revision: Option<String>,
}

/// One embedding provider transport (the versioned interface).
///
/// Implementations are synchronous and may block on I/O (network or local
/// inference); the engine runs batches on worker threads only where a
/// request timeout is enforced. `Send + Sync` is required for that seam.
pub trait EmbeddingProvider: Send + Sync {
    /// Protocol version this implementation speaks.
    fn protocol_version(&self) -> u32 {
        EMBEDDING_PROTOCOL_VERSION
    }

    /// The embedding space this provider produces.
    fn profile(&self) -> EmbeddingProfile;

    /// Embeds one batch of normalized inputs, in order. Returned vectors are
    /// validated for dimension and finiteness by the client before use.
    fn embed(&self, inputs: &[String]) -> Result<EmbeddingBatch>;
}

/// Counters for one embedding session (diagnostics only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EmbeddingStats {
    /// Inputs served from the content-addressed cache (no provider call).
    pub cache_hits: usize,
    /// Inputs embedded by the provider in this session.
    pub embedded: usize,
    /// Inputs rejected (batch failure after retries, or validation failure).
    pub failed: usize,
    /// Inputs left pending by budget exhaustion or cancellation.
    pub pending: usize,
    /// Provider batch attempts made (including retries).
    pub requests: usize,
    /// Batch attempts beyond the first.
    pub retries: usize,
}

/// Validates a vector before it is cached or published: exact dimension and
/// all components finite. A failing vector is rejected, never zero-filled.
pub fn validate_vector(vector: &[f32], dimension: u32) -> bool {
    (vector.len() as u32) == dimension && vector.iter().all(|component| component.is_finite())
}

/// Encodes a vector as little-endian f32 bytes (little-endian on all
/// supported targets).
pub fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for component in vector {
        out.extend_from_slice(&component.to_bits().to_le_bytes());
    }
    out
}

/// Decodes a stored vector, re-validating dimension and finiteness.
pub fn decode_vector(bytes: &[u8], dimension: u32) -> Option<Vec<f32>> {
    if bytes.len() / 4 != dimension as usize {
        return None;
    }
    let mut out = Vec::with_capacity(dimension as usize);
    for chunk in bytes.as_chunks::<4>().0 {
        let value = f32::from_bits(u32::from_le_bytes(*chunk));
        if !value.is_finite() {
            return None;
        }
        out.push(value);
    }
    Some(out)
}

/// Cosine similarity of two finite vectors (0.0 on length mismatch).
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += f64::from(*x) * f64::from(*y);
        norm_a += f64::from(*x) * f64::from(*x);
        norm_b += f64::from(*y) * f64::from(*y);
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Outcome of one timed batch attempt.
enum BatchOutcome {
    /// All vectors returned; `attempts` counts tries.
    Success {
        vectors: Vec<Vec<f32>>,
        attempts: usize,
    },
    /// Failed after exhausting retries, or canceled; `attempts` counts tries.
    Failed { attempts: usize },
}

/// Policy wrapper over a provider: bounded batching, per-batch timeout,
/// retry/backoff, cancellation, and request/input budgets.
pub struct EmbeddingClient {
    provider: Arc<Box<dyn EmbeddingProvider>>,
    budgets: EmbeddingBudgets,
    cancellation: CancellationToken,
}

impl EmbeddingClient {
    /// Wraps a provider with budgets and a cancellation token.
    pub fn new(
        provider: Box<dyn EmbeddingProvider>,
        budgets: EmbeddingBudgets,
        cancellation: CancellationToken,
    ) -> Self {
        debug_assert_eq!(provider.protocol_version(), EMBEDDING_PROTOCOL_VERSION);
        Self {
            provider: Arc::new(provider),
            budgets,
            cancellation,
        }
    }

    /// The provider's embedding space.
    pub fn profile(&self) -> EmbeddingProfile {
        self.provider.profile()
    }

    /// The budgets in effect.
    pub fn budgets(&self) -> &EmbeddingBudgets {
        &self.budgets
    }

    /// Embeds each input, in order, returning per-input vectors. Inputs that
    /// could not be embedded (batch failure after retries, timeout,
    /// validation failure, budget exhaustion, cancellation) come back as
    /// `None` — never a zero vector. `stats` describes the session.
    pub fn embed_all(&self, inputs: &[String]) -> (Vec<Option<Vec<f32>>>, EmbeddingStats) {
        let dimension = self.provider.profile().dimension;
        let mut out: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
        let mut stats = EmbeddingStats::default();
        let mut char_budget = self.budgets.max_input_chars_per_build;
        let mut batches = 0usize;
        let mut i = 0usize;
        while i < inputs.len() {
            if self.cancellation.is_canceled() {
                stats.pending += inputs.len() - i;
                break;
            }
            if batches >= self.budgets.max_requests_per_build as usize {
                stats.pending += inputs.len() - i;
                break;
            }
            // The first input counts against the remaining character budget
            // too; once it is exhausted the rest stay pending (never sent).
            let first_chars = inputs[i].chars().count() as u64;
            if char_budget < first_chars {
                stats.pending += inputs.len() - i;
                break;
            }
            char_budget -= first_chars;
            // Grow the batch as far as the per-batch size and the remaining
            // character budget allow.
            let max_end = (i + self.budgets.batch_size as usize).min(inputs.len());
            let mut end = i + 1;
            while end < max_end {
                let next_chars = inputs[end].chars().count() as u64;
                if char_budget < next_chars {
                    break;
                }
                char_budget -= next_chars;
                end += 1;
            }
            let batch: Vec<String> = inputs[i..end].to_vec();
            batches += 1;
            match self.run_batch(&batch) {
                BatchOutcome::Success { vectors, attempts } => {
                    stats.requests += attempts;
                    stats.retries += attempts.saturating_sub(1);
                    for (index, vector) in vectors.into_iter().enumerate() {
                        if validate_vector(&vector, dimension) {
                            stats.embedded += 1;
                            out[i + index] = Some(vector);
                        } else {
                            stats.failed += 1;
                        }
                    }
                }
                BatchOutcome::Failed { attempts } => {
                    stats.requests += attempts;
                    stats.retries += attempts.saturating_sub(1);
                    stats.failed += batch.len();
                }
            }
            i = end;
        }
        (out, stats)
    }

    /// Runs one batch with timeout and retry/backoff until success, retry
    /// exhaustion, or cancellation.
    fn run_batch(&self, batch: &[String]) -> BatchOutcome {
        let mut backoff = self.budgets.backoff_initial;
        let mut attempts = 0usize;
        loop {
            if self.cancellation.is_canceled() {
                return BatchOutcome::Failed { attempts };
            }
            attempts += 1;
            match self.call_with_timeout(batch) {
                Some(result) => {
                    return BatchOutcome::Success {
                        vectors: result.vectors,
                        attempts,
                    };
                }
                None => {
                    if attempts > self.budgets.max_retries as usize {
                        return BatchOutcome::Failed { attempts };
                    }
                    if !interruptible_sleep(backoff, &self.cancellation) {
                        return BatchOutcome::Failed { attempts };
                    }
                    backoff = Duration::from_millis(
                        backoff
                            .as_millis()
                            .saturating_mul(2)
                            .min(self.budgets.backoff_max.as_millis())
                            as u64,
                    );
                }
            }
        }
    }

    /// Calls the provider on a worker thread so the per-batch timeout can be
    /// enforced without an async executor. On timeout the worker is
    /// abandoned (its result is discarded; the provider's own transport
    /// timeout bounds it) and the batch counts as failed.
    fn call_with_timeout(&self, batch: &[String]) -> Option<EmbeddingBatch> {
        let provider = Arc::clone(&self.provider);
        let inputs = batch.to_vec();
        let (tx, rx) = mpsc::channel();
        let _worker = std::thread::spawn(move || {
            let result = provider.embed(&inputs);
            let _ = tx.send(result);
        });
        match rx.recv_timeout(self.budgets.timeout) {
            Ok(Ok(batch)) => Some(batch),
            Ok(Err(_)) | Err(_) => None,
        }
    }
}

/// Sleeps up to `duration`, returning false when canceled mid-sleep.
fn interruptible_sleep(duration: Duration, cancellation: &CancellationToken) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if cancellation.is_canceled() {
            return false;
        }
        std::thread::sleep(duration.min(Duration::from_millis(25)));
    }
    true
}

/// Content-addressed shared embedding cache for one repository:
/// `<cache>/repos/<repoId>/embedding-cache/embeddings.sqlite`.
///
/// Entries are keyed by (normalized input hash, full profile fingerprint),
/// so worktrees of the same repository reuse compatible vectors. A rename
/// reuses a vector only when the normalized input — which includes the
/// chunk's context label — did not change.
#[derive(Clone, Debug)]
pub struct EmbeddingCache {
    path: PathBuf,
}

impl EmbeddingCache {
    const SCHEMA: &str = r#"
        CREATE TABLE IF NOT EXISTS embedding (
          cache_key TEXT PRIMARY KEY,
          input_hash TEXT NOT NULL,
          fingerprint TEXT NOT NULL,
          dimension INTEGER NOT NULL,
          model_revision TEXT NOT NULL,
          vector BLOB NOT NULL,
          created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_embedding_lookup
          ON embedding(input_hash, fingerprint);
    "#;

    /// Opens (creating if needed) the cache database at `path`.
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(Self::SCHEMA)?;
        Ok(Self { path })
    }

    fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(conn)
    }

    /// The cache key for one normalized input and profile.
    pub fn cache_key(input_hash: &str, fingerprint: &str) -> String {
        hash::sha256_hex(format!("{input_hash}\n{fingerprint}"))
    }

    /// Reads many cached vectors in one connection.
    ///
    /// Returns `input_hash -> vector` for hits; corrupt or dimension-mismatched
    /// entries are dropped (and deleted) and read as misses. Keys are queried in
    /// bounded chunks (500 per statement) so a single query never exceeds
    /// SQLite's bound-variable limit on large indexes.
    pub fn get_many(
        &self,
        input_hashes: &[String],
        fingerprint: &str,
        dimension: u32,
    ) -> Result<std::collections::HashMap<String, Vec<f32>>> {
        const KEYS_PER_QUERY: usize = 500;
        let mut hits: std::collections::HashMap<String, Vec<f32>> =
            std::collections::HashMap::new();
        let mut keys: Vec<(String, String)> = input_hashes
            .iter()
            .map(|input_hash| (input_hash.clone(), Self::cache_key(input_hash, fingerprint)))
            .collect();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return Ok(hits);
        }
        let conn = self.conn()?;
        for chunk in keys.chunks(KEYS_PER_QUERY) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT input_hash, vector, dimension FROM embedding \
                 WHERE cache_key IN ({})",
                placeholders.join(", ")
            );
            let mut stmt = conn.prepare(&sql)?;
            let params = rusqlite::params_from_iter(chunk.iter().map(|(_, key)| key.as_str()));
            let rows = stmt
                .query_map(params, |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
            for (input_hash, bytes, stored_dim) in rows {
                match decode_vector(&bytes, dimension) {
                    Some(vector) if stored_dim == dimension as i64 => {
                        hits.insert(input_hash, vector);
                    }
                    _ => {
                        let key = Self::cache_key(&input_hash, fingerprint);
                        let _ = conn
                            .execute("DELETE FROM embedding WHERE cache_key = ?1", params![key]);
                    }
                }
            }
        }
        Ok(hits)
    }

    /// Stores many validated vectors in one transaction (one prepared
    /// statement). `items` are (input hash, vector).
    pub fn put_many(
        &self,
        items: &[(String, Vec<f32>)],
        fingerprint: &str,
        dimension: u32,
        model_revision: &str,
        now_ms: i64,
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO embedding \
                 (cache_key, input_hash, fingerprint, dimension, model_revision, vector, created_at_ms) \
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
              ON CONFLICT(cache_key) DO UPDATE SET \
                  vector = excluded.vector, model_revision = excluded.model_revision",
        )?;
        for (input_hash, vector) in items {
            stmt.execute(params![
                Self::cache_key(input_hash, fingerprint),
                input_hash,
                fingerprint,
                dimension as i64,
                model_revision,
                encode_vector(vector),
                now_ms,
            ])?;
        }
        drop(stmt);
        tx.commit()?;
        Ok(())
    }

    /// Reads a cached vector for one input and profile. Corrupt entries are
    /// dropped and read as a miss.
    pub fn get(
        &self,
        input_hash: &str,
        fingerprint: &str,
        dimension: u32,
    ) -> Result<Option<Vec<f32>>> {
        let conn = self.conn()?;
        let key = Self::cache_key(input_hash, fingerprint);
        let mut stmt =
            conn.prepare("SELECT vector, dimension FROM embedding WHERE cache_key = ?1")?;
        let row = stmt
            .query_row(params![key], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?;
        let Some((bytes, stored_dim)) = row else {
            return Ok(None);
        };
        match decode_vector(&bytes, dimension) {
            Some(vector) if stored_dim == dimension as i64 => Ok(Some(vector)),
            _ => {
                let _ = conn.execute("DELETE FROM embedding WHERE cache_key = ?1", params![key]);
                Ok(None)
            }
        }
    }

    /// Stores one validated vector (upsert on the cache key).
    pub fn put(
        &self,
        input_hash: &str,
        fingerprint: &str,
        dimension: u32,
        model_revision: &str,
        vector: &[f32],
        now_ms: i64,
    ) -> Result<()> {
        let conn = self.conn()?;
        let key = Self::cache_key(input_hash, fingerprint);
        conn.execute(
            "INSERT INTO embedding \
                 (cache_key, input_hash, fingerprint, dimension, model_revision, vector, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(cache_key) DO UPDATE SET \
                 vector = excluded.vector, model_revision = excluded.model_revision",
            params![
                key,
                input_hash,
                fingerprint,
                dimension as i64,
                model_revision,
                encode_vector(vector),
                now_ms
            ],
        )?;
        Ok(())
    }

    /// Number of cache entries (for status/diagnostics).
    pub fn count(&self) -> Result<i64> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM embedding")?;
        Ok(stmt.query_row([], |row| row.get(0))?)
    }

    /// Deletes entries not referenced by any retained generation.
    /// `referenced` holds the (input hash, fingerprint) pairs still in use.
    /// Returns the number of entries reclaimed.
    pub fn gc(&self, referenced: &HashSet<(String, String)>) -> Result<usize> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT cache_key, input_hash, fingerprint FROM embedding")?;
        let rows: Vec<(String, String, String)> = {
            let iter = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            iter.collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?
        };
        let mut reclaimed = 0usize;
        for (key, input_hash, fingerprint) in rows {
            if !referenced.contains(&(input_hash, fingerprint)) {
                conn.execute("DELETE FROM embedding WHERE cache_key = ?1", params![key])?;
                reclaimed += 1;
            }
        }
        Ok(reclaimed)
    }
}

/// One chunk input for one embedding pass.
///
/// `input` is the normalized embedding input: the stored chunk text exactly
/// (secret redaction already applied); `input_hash` is its SHA-256.
#[derive(Clone, Debug)]
pub struct EmbeddingWork {
    /// Opaque chunk id of the generation being built.
    pub chunk_id: String,
    /// Normalized embedding input.
    pub input: String,
    /// SHA-256 of the normalized input.
    pub input_hash: String,
}

/// Result of one embedding pass over a generation's chunk inputs.
#[derive(Clone, Debug, Default)]
pub struct EmbeddingOutcome {
    /// chunk id -> validated vector (cache- or provider-sourced).
    pub vectors: Vec<(String, Vec<f32>)>,
    /// Session counters (cache hits, embedded, failed, pending, requests).
    pub stats: EmbeddingStats,
    /// Chunk ids left without a vector (failed or pending).
    pub pending_chunk_ids: Vec<String>,
}

/// Runs one embedding pass over a generation's chunk inputs: cache lookup
/// first (no re-embedding of unchanged input), then bounded provider calls
/// for the misses. Every returned vector is dimension- and finiteness-
/// validated; rejected vectors are reported as failures, never zero-filled.
pub fn embed_chunks(
    cache: &EmbeddingCache,
    client: &EmbeddingClient,
    work: &[EmbeddingWork],
    now_ms: i64,
) -> Result<EmbeddingOutcome> {
    let profile = client.profile();
    let fingerprint = profile.fingerprint();
    let mut stats = EmbeddingStats::default();
    let mut vectors: Vec<(String, Vec<f32>)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut misses: Vec<&EmbeddingWork> = Vec::new();

    // Phase 1: content-addressed cache (one lookup pass; unchanged input is
    // never re-embedded).
    let cache_hits = cache.get_many(
        &work
            .iter()
            .map(|item| item.input_hash.clone())
            .collect::<Vec<_>>(),
        &fingerprint,
        profile.dimension,
    )?;
    for item in work {
        match cache_hits.get(&item.input_hash) {
            Some(vector) => {
                stats.cache_hits += 1;
                vectors.push((item.chunk_id.clone(), vector.clone()));
            }
            None => misses.push(item),
        }
    }

    // Phase 2: bounded provider calls for the misses.
    if !misses.is_empty() {
        let miss_inputs: Vec<String> = misses.iter().map(|item| item.input.clone()).collect();
        let (results, session) = client.embed_all(&miss_inputs);
        stats.embedded += session.embedded;
        stats.failed += session.failed;
        stats.pending += session.pending;
        stats.requests += session.requests;
        stats.retries += session.retries;
        let mut to_store: Vec<(String, Vec<f32>)> = Vec::new();
        for (position, item) in misses.iter().enumerate() {
            let Some(vector) = results.get(position).cloned().flatten() else {
                pending.push(item.chunk_id.clone());
                continue;
            };
            vectors.push((item.chunk_id.clone(), vector.clone()));
            to_store.push((item.input_hash.clone(), vector));
        }
        cache.put_many(
            &to_store,
            &fingerprint,
            profile.dimension,
            &profile.model,
            now_ms,
        )?;
    }

    Ok(EmbeddingOutcome {
        vectors,
        stats,
        pending_chunk_ids: pending,
    })
}

/// Collects the (input hash, fingerprint) pairs still referenced by any
/// retained generation of `repo_id`, then reclaims unreferenced shared
/// embeddings. Active readers only read vectors from their own retained
/// generation, so GC never invalidates served data. If any sibling
/// worktree database cannot be read, GC is skipped entirely (fail closed:
/// unknown references are never deleted around).
pub fn gc_repo_embeddings(cache_paths: &crate::cache::CachePaths, repo_id: &str) -> Result<usize> {
    let cache = EmbeddingCache::open(cache_paths.embedding_cache_path(repo_id))?;
    let worktrees = cache_paths.worktrees_dir(repo_id);
    let mut referenced: HashSet<(String, String)> = HashSet::new();
    if worktrees.exists() {
        for entry in fs::read_dir(&worktrees)? {
            let entry = entry?;
            let db = entry.path().join("index.sqlite");
            if !db.exists() {
                continue;
            }
            // Fail closed (skip GC entirely) when any sibling database cannot
            // be opened or its references cannot be read.
            let conn = match crate::store::Store::new(db).open() {
                Ok(conn) => conn,
                Err(_) => return Ok(0),
            };
            match crate::store::referenced_embedding_keys(&conn, repo_id) {
                Ok(keys) => referenced.extend(keys),
                Err(_) => return Ok(0),
            }
        }
    }
    cache.gc(&referenced)
}

/// Vector storage interface: exact cosine candidates for one generation and
/// profile. The first (and only, for now) implementation lives in the scope
/// index SQLite; an ANN adapter may be added behind this seam later only if
/// measured latency/memory justify it.
pub trait VectorStore {
    /// (chunk count, vector count) for one generation and profile
    /// (`None` profile yields a vector count of 0).
    fn coverage(&self, generation_id: i64, fingerprint: Option<&str>) -> Result<(i64, i64)>;

    /// Validated vectors for one generation and profile, by chunk id.
    fn vectors(
        &self,
        generation_id: i64,
        fingerprint: &str,
    ) -> Result<std::collections::HashMap<String, Vec<f32>>>;
}

/// SQLite vector store over a scope index connection.
pub struct SqliteVectorStore<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteVectorStore<'a> {
    /// Wraps an open scope index connection.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl VectorStore for SqliteVectorStore<'_> {
    fn coverage(&self, generation_id: i64, fingerprint: Option<&str>) -> Result<(i64, i64)> {
        crate::store::vector_coverage(self.conn, generation_id, fingerprint)
    }

    fn vectors(
        &self,
        generation_id: i64,
        fingerprint: &str,
    ) -> Result<std::collections::HashMap<String, Vec<f32>>> {
        crate::store::vectors_for_generation(self.conn, generation_id, fingerprint)
    }
}
