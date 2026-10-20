//! K3 (v1-2): embedding profiles, content-addressed cache, bounded embedding
//! client, vector publication with coverage, hybrid retrieval (RRF fusion),
//! and embedding-cache GC.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use repoise_core::adapter::SnapshotMode;
use repoise_core::adapter::fake::FakeRevisionAdapter;
use repoise_core::cache::CachePaths;
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::embed::{
    CancellationToken, EmbeddingBatch, EmbeddingBudgets, EmbeddingCache, EmbeddingClient,
    EmbeddingProfile, EmbeddingProvider, EmbeddingWork, embed_chunks, gc_repo_embeddings,
    validate_vector,
};
use repoise_core::error::Error;
use repoise_core::hash;
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::search::{self, QueryEmbedder, SearchMode, SearchRequest};
use repoise_core::store::{self, ChunkRow, ChunkVecRow, GenerationInput, Store};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-k3-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Deterministic test provider: vectors are a pure function of the input.
struct HashProvider {
    calls: Arc<AtomicUsize>,
    fail: bool,
    model: String,
    dimension: u32,
    input_profile: repoise_core::embed::InputProfile,
}

impl HashProvider {
    fn new(dimension: u32) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
            model: "v1".to_string(),
            dimension,
            input_profile: repoise_core::embed::InputProfile::RAW,
        }
    }

    fn failing(dimension: u32) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
            model: "v1".to_string(),
            dimension,
            input_profile: repoise_core::embed::InputProfile::RAW,
        }
    }

    fn other_model(dimension: u32) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
            model: "v2".to_string(),
            dimension,
            input_profile: repoise_core::embed::InputProfile::RAW,
        }
    }
}

/// Deterministic pseudo-embedding: stable, finite, one vector per component.
fn vector_for(input: &str, dimension: u32) -> Vec<f32> {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100000001b3);
    }
    (0..dimension)
        .map(|i| {
            let mixed = h ^ (u64::from(i).wrapping_mul(0x9E3779B97F4A7C15)).wrapping_add(1);
            let scaled = (f64::from((mixed % 1000) as u32) / 1000.0) * 2.0 - 1.0;
            scaled as f32
        })
        .collect()
}

impl EmbeddingProvider for HashProvider {
    fn profile(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            provider: "test-hash".to_string(),
            model: self.model.clone(),
            dimension: self.dimension,
            profile_version: repoise_core::embed::PROFILE_VERSION,
            input_profile: self.input_profile,
        }
    }

    fn embed(&self, inputs: &[String]) -> repoise_core::Result<EmbeddingBatch> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(Error::Provider("injected provider failure".to_string()));
        }
        Ok(EmbeddingBatch {
            vectors: inputs
                .iter()
                .map(|input| vector_for(input, self.dimension))
                .collect(),
            model_revision: None,
        })
    }
}

fn client(provider: HashProvider) -> EmbeddingClient {
    EmbeddingClient::new(
        Box::new(provider),
        EmbeddingBudgets::default(),
        CancellationToken::new(),
    )
}

fn fingerprint_for(model: &str) -> String {
    EmbeddingProfile {
        provider: "test-hash".to_string(),
        model: model.to_string(),
        dimension: 8,
        profile_version: repoise_core::embed::PROFILE_VERSION,
        input_profile: repoise_core::embed::InputProfile::RAW,
    }
    .fingerprint()
}
#[test]
fn embedding_cache_reuses_unchanged_input_and_separates_profiles() {
    let dir = temp_dir("cache");
    let cache = EmbeddingCache::open(dir.join("embeddings.sqlite")).unwrap();
    let session = client(HashProvider::new(8));
    let work = vec![
        EmbeddingWork {
            chunk_id: "c1".into(),
            input: "alpha".into(),
            input_hash: hash::sha256_hex("alpha"),
        },
        EmbeddingWork {
            chunk_id: "c2".into(),
            input: "beta".into(),
            input_hash: hash::sha256_hex("beta"),
        },
    ];
    let first = embed_chunks(&cache, &session, &work, 1).unwrap();
    assert_eq!(first.vectors.len(), 2);
    assert_eq!(first.stats.embedded, 2);
    assert_eq!(first.stats.cache_hits, 0);

    // Unchanged inputs are served from the cache without a provider call.
    let second = embed_chunks(&cache, &session, &work, 2).unwrap();
    assert_eq!(second.stats.cache_hits, 2);
    assert_eq!(second.stats.embedded, 0);
    assert_eq!(second.vectors.len(), 2);

    // A different profile (different model) never reuses the entry.
    let other = client(HashProvider::other_model(8));
    let third = embed_chunks(&cache, &other, &work, 3).unwrap();
    assert_eq!(third.stats.embedded, 2);
    assert_eq!(third.stats.cache_hits, 0);

    // Same input, same profile: identical deterministic vector.
    assert_eq!(first.vectors[0].1, vector_for("alpha", 8));
    assert_eq!(second.vectors[0].1, first.vectors[0].1);
}

#[test]
fn failed_vectors_are_never_zero_filled() {
    let dir = temp_dir("cache-fail");
    let cache = EmbeddingCache::open(dir.join("embeddings.sqlite")).unwrap();
    let client = client(HashProvider::failing(8));
    let work = vec![EmbeddingWork {
        chunk_id: "c1".into(),
        input: "alpha".into(),
        input_hash: hash::sha256_hex("alpha"),
    }];
    let outcome = embed_chunks(&cache, &client, &work, 1).unwrap();
    assert!(outcome.vectors.is_empty(), "no vector may be published");
    assert_eq!(outcome.stats.failed, 1);
    assert_eq!(outcome.stats.embedded, 0);
    assert_eq!(outcome.pending_chunk_ids, vec!["c1".to_string()]);
    // Nothing was cached.
    assert_eq!(
        cache
            .get(&hash::sha256_hex("alpha"), &fingerprint_for("v1"), 8)
            .unwrap(),
        None
    );
}

fn fake_adapter(label: &Path, files: &[(&str, &str)]) -> FakeRevisionAdapter {
    let mut tree = BTreeMap::new();
    for (path, content) in files {
        tree.insert((*path).to_string(), content.as_bytes().to_vec());
    }
    let mut revisions = BTreeMap::new();
    revisions.insert("fake:v1".to_string(), tree);
    FakeRevisionAdapter::new(label, revisions)
}

/// One published scope for retrieval tests.
struct Built {
    store: Store,
    adapter: FakeRevisionAdapter,
    generation_id: i64,
    chunks_total: usize,
}

fn build_scope(tag: &str, files: &[(&str, &str)], session: Option<&EmbeddingClient>) -> Built {
    let root = temp_dir(tag);
    let adapter = fake_adapter(&root, files);
    let effective = EffectiveConfig::resolve(&root, &CliOverrides::default()).unwrap();
    let cache = CachePaths::resolve(&root, ".repoise", None).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    let outcome = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        session,
        None,
    )
    .unwrap();
    assert!(outcome.chunks_total > 0);
    Built {
        store,
        adapter,
        generation_id: outcome.generation_id,
        chunks_total: outcome.chunks_total,
    }
}

const GUIDE_MD_V1: &str = "# Guide\n\nThe widget assembly uses a special gizmo bearing.\n\n## Details\n\nWrench the widget until it clicks.\n";

#[test]
fn vectors_publish_with_generation_and_report_coverage() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", GUIDE_MD_V1)];
    let client = client(HashProvider::new(8));
    let built = build_scope("vectors-build", &files, Some(&client));
    let fp = fingerprint_for("v1");
    let conn = built.store.open().unwrap();
    let (chunks, vectors) = store::vector_coverage(&conn, built.generation_id, Some(&fp)).unwrap();
    assert_eq!(chunks, built.chunks_total as i64);
    assert_eq!(
        vectors, built.chunks_total as i64,
        "every chunk has a vector"
    );
    assert!(
        store::pending_vector_paths(&conn, built.generation_id, Some(&fp))
            .unwrap()
            .is_empty()
    );
    let vectors_by_id = store::vectors_for_generation(&conn, built.generation_id, &fp).unwrap();
    assert_eq!(vectors_by_id.len(), built.chunks_total);
    for vector in vectors_by_id.values() {
        assert!(validate_vector(vector, 8));
    }
}

#[test]
fn failed_embedding_stays_lexical_and_publishes_without_vectors() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", GUIDE_MD_V1)];
    let client = client(HashProvider::failing(8));
    let built = build_scope("vectors-fail", &files, Some(&client));
    let conn = built.store.open().unwrap();
    let fp = fingerprint_for("v1");
    let (chunks, vectors) = store::vector_coverage(&conn, built.generation_id, Some(&fp)).unwrap();
    assert_eq!(chunks, built.chunks_total as i64);
    assert_eq!(vectors, 0, "no vectors are invented on failure");
    // All chunk paths are reported pending.
    let pending = store::pending_vector_paths(&conn, built.generation_id, Some(&fp)).unwrap();
    assert!(!pending.is_empty());
}

struct ProviderQueryEmbedder {
    provider: Arc<dyn EmbeddingProvider>,
}

impl QueryEmbedder for ProviderQueryEmbedder {
    fn embed_queries(&self, inputs: &[String]) -> repoise_core::Result<Vec<Option<Vec<f32>>>> {
        let batch = self.provider.embed(inputs)?;
        let dimension = self.provider.profile().dimension;
        Ok(batch
            .vectors
            .into_iter()
            .map(|vector| {
                if validate_vector(&vector, dimension) {
                    Some(vector)
                } else {
                    None
                }
            })
            .collect())
    }

    fn profile_fingerprint(&self) -> String {
        self.provider.profile().fingerprint()
    }

    fn input_profile(&self) -> repoise_core::embed::InputProfile {
        self.provider.profile().input_profile
    }
}

struct NoopQueryEmbedder;

impl QueryEmbedder for NoopQueryEmbedder {
    fn embed_queries(&self, _inputs: &[String]) -> repoise_core::Result<Vec<Option<Vec<f32>>>> {
        Ok(vec![None; _inputs.len()])
    }

    fn profile_fingerprint(&self) -> String {
        // Never matches a stored profile (this embedder never produces vectors).
        String::new()
    }

    fn input_profile(&self) -> repoise_core::embed::InputProfile {
        repoise_core::embed::InputProfile::RAW
    }
}

fn run_search(
    built: &Built,
    query: &str,
    mode: SearchMode,
    embedder: Option<&dyn QueryEmbedder>,
) -> repoise_core::Result<search::SearchResponse> {
    search::search(
        &built.adapter,
        SnapshotMode::PlainDirectory,
        &built.store,
        &SearchRequest {
            query: query.to_string(),
            path_filter: None,
            role_filter: None,
            max_results: None,
            max_output_tokens: None,
            cursor: None,
            mode,
            rrf_k: None,
        },
        embedder,
    )
}

#[test]
fn hybrid_search_fuses_degrades_and_errors_without_vectors() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", GUIDE_MD_V1)];
    let client = client(HashProvider::new(8));
    let built = build_scope("vectors-hybrid", &files, Some(&client));

    // Hybrid with a working query embedder: fused results with RRF explanation.
    let embedder = ProviderQueryEmbedder {
        provider: Arc::new(HashProvider::new(8)),
    };
    let hybrid = run_search(&built, "gizmo", SearchMode::Hybrid, Some(&embedder)).unwrap();
    assert_eq!(hybrid.retrieval_mode, "hybrid");
    assert!(!hybrid.results.is_empty());
    assert!(
        hybrid.results[0].explanation.contains("rrf"),
        "hybrid explanations must attribute fusion: {}",
        hybrid.results[0].explanation
    );

    // Hybrid with an embedder that declines: degrades to lexical.
    let noop = NoopQueryEmbedder;
    let degraded = run_search(&built, "gizmo", SearchMode::Hybrid, Some(&noop)).unwrap();
    assert_eq!(degraded.retrieval_mode, "lexical");
    assert!(!degraded.results.is_empty());

    // Vectors-only with a working embedder.
    let vector_only =
        run_search(&built, "gizmo", SearchMode::VectorsOnly, Some(&embedder)).unwrap();
    assert_eq!(vector_only.retrieval_mode, "vectors-only");
    assert!(!vector_only.results.is_empty());

    // Vectors-only without a query embedder degrades to the lexical baseline.
    let degraded_vector_only = run_search(&built, "gizmo", SearchMode::VectorsOnly, None).unwrap();
    assert_eq!(degraded_vector_only.retrieval_mode, "lexical");

    // A lexical-only generation degrades vectors-only to lexical as well.
    let lexical_built = build_scope("vectors-lexical", &files, None);
    let lexical_degraded = run_search(
        &lexical_built,
        "gizmo",
        SearchMode::VectorsOnly,
        Some(&embedder),
    )
    .unwrap();
    assert_eq!(lexical_degraded.retrieval_mode, "lexical");
}

#[test]
fn gc_reclaims_unreferenced_embeddings_and_keeps_referenced() {
    let root = temp_dir("gc");
    let repo_id = "repo-gc";
    let cache = CachePaths::resolve(&root, ".repoise", None).unwrap();
    let embedding_db = EmbeddingCache::open(cache.embedding_cache_path(repo_id)).unwrap();
    let fp = fingerprint_for("v1");
    let alpha_hash = hash::sha256_hex("alpha");
    let beta_hash = hash::sha256_hex("beta");
    embedding_db
        .put(
            &alpha_hash,
            &fp,
            8,
            "test-hash:v1",
            &vector_for("alpha", 8),
            1,
        )
        .unwrap();
    embedding_db
        .put(
            &beta_hash,
            &fp,
            8,
            "test-hash:v1",
            &vector_for("beta", 8),
            1,
        )
        .unwrap();

    // No retained worktree generation references anything: both reclaimed.
    assert_eq!(gc_repo_embeddings(&cache, repo_id).unwrap(), 2);

    // Retain one entry through a published generation, orphan the other.
    let wt_dir = cache.worktrees_dir(repo_id).join("wt1");
    std::fs::create_dir_all(&wt_dir).unwrap();
    let store = Store::new(wt_dir.join("index.sqlite"));
    let conn = store.open().unwrap();
    store::publish(
        &conn,
        &GenerationInput {
            repo_id: repo_id.to_string(),
            worktree_id: "wt1".into(),
            snapshot_id: "snap-1".into(),
            snapshot_mode: "plain-directory".into(),
            revision_id: Some("fake:v1".into()),
            manifest_hash: "manifest".into(),
            config_fingerprint: "config".into(),
            parser_fingerprint: "parser".into(),
            built_at_ms: 1,
            files: Vec::new(),
            chunks: vec![ChunkRow {
                chunk_id: "c1".into(),
                parent_chunk_id: None,
                path: "docs/a.md".into(),
                heading_path: String::new(),
                corpus: "docs".into(),
                text: "alpha".into(),
                text_hash: alpha_hash.clone(),
                symbol: String::new(),
                context: None,
                line_start: 1,
                line_end: 1,
                byte_start: 0,
                byte_end: 5,
            }],
            vectors: vec![ChunkVecRow {
                chunk_id: "c1".into(),
                fingerprint: fp.clone(),
                input_hash: alpha_hash.clone(),
                dimension: 8,
                vector: vector_for("alpha", 8),
            }],
            vector_profile: Some(fp.clone()),
            embedding_scope: repoise_core::embed::EmbeddingScope::Docs,
            symbols: Vec::new(),
            references: Vec::new(),
            history: Vec::new(),
            history_meta: String::new(),
        },
    )
    .unwrap();
    embedding_db
        .put(
            &alpha_hash,
            &fp,
            8,
            "test-hash:v1",
            &vector_for("alpha", 8),
            2,
        )
        .unwrap();
    embedding_db
        .put(
            &beta_hash,
            &fp,
            8,
            "test-hash:v1",
            &vector_for("beta", 8),
            2,
        )
        .unwrap();
    // The referenced entry survives; the orphan is reclaimed.
    assert_eq!(gc_repo_embeddings(&cache, repo_id).unwrap(), 1);
    assert!(embedding_db.get(&alpha_hash, &fp, 8).unwrap().is_some());
    assert!(embedding_db.get(&beta_hash, &fp, 8).unwrap().is_none());
}

#[test]
fn vector_search_respects_path_and_role_filters() {
    let files: Vec<(&str, &str)> = vec![
        ("docs/a.md", "# Guide A\n\nThe gizmo guide explains setup."),
        ("other/b.md", "# Guide B\n\nThe gizmo lives in guide B."),
        ("conf/app.toml", "[section]\nkey = value\n"),
    ];
    let client = client(HashProvider::new(8));
    let built = build_scope("vector-filters", &files, Some(&client));
    let embedder = ProviderQueryEmbedder {
        provider: Arc::new(HashProvider::new(8)),
    };

    for (mode, label) in [
        (SearchMode::Hybrid, "hybrid"),
        (SearchMode::VectorsOnly, "vectors-only"),
    ] {
        // Path filter: only docs/* may be returned.
        let response = search::search(
            &built.adapter,
            SnapshotMode::PlainDirectory,
            &built.store,
            &SearchRequest {
                query: "gizmo".to_string(),
                path_filter: Some("docs/*".to_string()),
                role_filter: None,
                max_results: None,
                max_output_tokens: None,
                cursor: None,
                mode,
                rrf_k: None,
            },
            Some(&embedder),
        )
        .unwrap();
        assert_eq!(response.retrieval_mode, label);
        assert!(
            response.results.iter().all(|hit| hit.path == "docs/a.md"),
            "{label} path filter leaked results: {:?}",
            response
                .results
                .iter()
                .map(|hit| hit.path.clone())
                .collect::<Vec<_>>()
        );

        // Role filter: only config files may be returned.
        let response = search::search(
            &built.adapter,
            SnapshotMode::PlainDirectory,
            &built.store,
            &SearchRequest {
                query: "gizmo".to_string(),
                path_filter: None,
                role_filter: Some(repoise_core::classify::Role::Config),
                max_results: None,
                max_output_tokens: None,
                cursor: None,
                mode,
                rrf_k: None,
            },
            Some(&embedder),
        )
        .unwrap();
        assert_eq!(response.retrieval_mode, label);
        assert!(
            response
                .results
                .iter()
                .all(|hit| hit.path == "conf/app.toml"),
            "{label} role filter leaked results: {:?}",
            response
                .results
                .iter()
                .map(|hit| hit.path.clone())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn profile_mismatch_degrades_to_lexical() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", GUIDE_MD_V1)];
    let client = client(HashProvider::new(8));
    let built = build_scope("profile-mismatch", &files, Some(&client));

    // Same dimension, different model: an incompatible embedding space.
    let other = ProviderQueryEmbedder {
        provider: Arc::new(HashProvider::other_model(8)),
    };
    let hybrid = run_search(&built, "gizmo", SearchMode::Hybrid, Some(&other)).unwrap();
    assert_eq!(hybrid.retrieval_mode, "lexical");
    assert!(
        hybrid.coverage.vector.contains("differs"),
        "coverage must explain the mismatch: {}",
        hybrid.coverage.vector
    );

    let vectors_only = run_search(&built, "gizmo", SearchMode::VectorsOnly, Some(&other)).unwrap();
    assert_eq!(vectors_only.retrieval_mode, "lexical");
    assert!(vectors_only.coverage.vector.contains("differs"));
}

#[test]
fn character_budget_counts_the_first_input_and_keeps_the_rest_pending() {
    let provider = HashProvider::new(8);
    let budgets = EmbeddingBudgets {
        batch_size: 4,
        max_input_chars_per_build: 10,
        max_requests_per_build: 10,
        ..EmbeddingBudgets::default()
    };
    let session = EmbeddingClient::new(Box::new(provider), budgets, CancellationToken::new());
    let inputs = vec!["a".repeat(100); 40];
    let (outcomes, stats) = session.embed_all(&inputs);
    assert!(
        outcomes.iter().all(|vector| vector.is_none()),
        "an exhausted character budget must not produce vectors"
    );
    assert_eq!(stats.pending, 40);
    assert_eq!(stats.embedded, 0);
    assert_eq!(
        stats.requests, 0,
        "an exhausted budget must not call the provider"
    );
}

#[test]
fn v1_database_migrates_and_reopens() {
    let dir = temp_dir("migration");
    let db_path = dir.join("index.sqlite");
    // Simulate a card-K2 (schema v1) database.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); \
             CREATE TABLE generation ( \
               id INTEGER PRIMARY KEY AUTOINCREMENT, \
               repo_id TEXT NOT NULL, worktree_id TEXT NOT NULL, \
               snapshot_id TEXT NOT NULL, snapshot_mode TEXT NOT NULL, \
               revision_id TEXT, manifest_hash TEXT NOT NULL, \
               config_fingerprint TEXT NOT NULL, parser_fingerprint TEXT NOT NULL, \
               built_at_ms INTEGER NOT NULL, state TEXT NOT NULL, \
               files INTEGER NOT NULL, chunks INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '1')",
            [],
        )
        .unwrap();
    }
    let store = Store::new(db_path);
    let conn = store.open().unwrap();
    let version = store::meta_value(&conn, "schema_version").unwrap().unwrap();
    assert_eq!(version, repoise_core::INDEX_SCHEMA_VERSION.to_string());
    drop(conn);
    // A second open must succeed (no duplicate-column error).
    let conn = store.open().unwrap();
    let version = store::meta_value(&conn, "schema_version").unwrap().unwrap();
    assert_eq!(version, repoise_core::INDEX_SCHEMA_VERSION.to_string());
}

#[test]
fn cache_get_many_handles_more_keys_than_the_sqlite_variable_limit() {
    let dir = temp_dir("cache-many");
    let cache = EmbeddingCache::open(dir.join("embeddings.sqlite")).unwrap();
    let fp = fingerprint_for("v1");
    // Seed one entry so a hit is found among the misses.
    let known = hash::sha256_hex("known");
    cache
        .put(&known, &fp, 8, "test-hash:v1", &vector_for("known", 8), 1)
        .unwrap();
    // 40,000 unique hashes exceed SQLite's bound-variable limit per statement,
    // so the lookup must be split across statements without failing.
    let hashes: Vec<String> = (0..40_000u32)
        .map(|i| hash::sha256_hex(format!("chunk-{i}")))
        .collect();
    assert!(hashes.iter().all(|h| *h != known));
    let mut hashes = hashes;
    hashes.push(known.clone());
    let hits = cache.get_many(&hashes, &fp, 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits.get(&known), Some(&vector_for("known", 8)));
}

// --- Model input profiles (EmbeddingGemma 2) --------------------------------

use std::sync::Mutex;

use repoise_core::classify::Role;
use repoise_core::embed::{
    InputProfile, docs_chunk_title, format_code_input, format_code_query, format_docs_input,
    format_docs_query,
};

const MATH_TS: &str = r#"
/** Multiplies two numbers. */
export function multiply(a: number, b: number): number {
  return a * b;
}
"#;

/// Deterministic provider that records every input it is asked to embed, so
/// tests can assert the exact templated inputs (no model server needed).
#[derive(Clone)]
struct RecordingProvider {
    inputs: Arc<Mutex<Vec<String>>>,
    model: String,
    dimension: u32,
    input_profile: InputProfile,
}

impl RecordingProvider {
    fn new(model: &str, dimension: u32, input_profile: InputProfile) -> Arc<Self> {
        Arc::new(Self {
            inputs: Arc::new(Mutex::new(Vec::new())),
            model: model.to_string(),
            dimension,
            input_profile,
        })
    }

    fn received(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.inputs.lock().unwrap().clear();
    }

    fn profile_for(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            provider: "test-record".to_string(),
            model: self.model.clone(),
            dimension: self.dimension,
            profile_version: repoise_core::embed::PROFILE_VERSION,
            input_profile: self.input_profile,
        }
    }

    fn embed_impl(&self, inputs: &[String]) -> repoise_core::Result<EmbeddingBatch> {
        self.inputs.lock().unwrap().extend(inputs.iter().cloned());
        Ok(EmbeddingBatch {
            vectors: inputs
                .iter()
                .map(|input| vector_for(input, self.dimension))
                .collect(),
            model_revision: None,
        })
    }
}

/// Local trait object box for the recording provider.
struct RecordingBox(RecordingProvider);

impl EmbeddingProvider for RecordingBox {
    fn profile(&self) -> EmbeddingProfile {
        self.0.profile_for()
    }

    fn embed(&self, inputs: &[String]) -> repoise_core::Result<EmbeddingBatch> {
        self.0.embed_impl(inputs)
    }
}

fn client_of(provider: Arc<RecordingProvider>) -> EmbeddingClient {
    EmbeddingClient::new(
        Box::new(RecordingBox((*provider).clone())),
        EmbeddingBudgets::default(),
        CancellationToken::new(),
    )
}

struct RecordingQueryEmbedder {
    provider: Arc<RecordingProvider>,
}

impl QueryEmbedder for RecordingQueryEmbedder {
    fn embed_queries(&self, inputs: &[String]) -> repoise_core::Result<Vec<Option<Vec<f32>>>> {
        let batch = self.provider.embed_impl(inputs)?;
        Ok(batch.vectors.into_iter().map(Some).collect())
    }

    fn profile_fingerprint(&self) -> String {
        self.provider.profile_for().fingerprint()
    }

    fn input_profile(&self) -> InputProfile {
        self.provider.input_profile
    }
}

/// Builds one published scope with an explicit embedding scope setting.
fn build_scope_with_scope(
    tag: &str,
    files: &[(&str, &str)],
    session: Option<&EmbeddingClient>,
    scope: &str,
) -> Built {
    let root = temp_dir(tag);
    std::fs::write(
        root.join("repoise.config.json"),
        format!("{{\"schemaVersion\": 1, \"embedding\": {{\"scope\": \"{scope}\"}}}}\n"),
    )
    .unwrap();
    let adapter = fake_adapter(&root, files);
    let effective = EffectiveConfig::resolve(&root, &CliOverrides::default()).unwrap();
    assert_eq!(effective.embedding_scope.as_str(), scope);
    let cache = CachePaths::resolve(&root, ".repoise", None).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    let outcome = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        session,
        None,
    )
    .unwrap();
    assert!(outcome.chunks_total > 0);
    Built {
        store,
        adapter,
        generation_id: outcome.generation_id,
        chunks_total: outcome.chunks_total,
    }
}

#[test]
fn input_profile_parsing_identity_and_templates_are_exact() {
    // Parsing: explicit names, default and explicit versions, errors.
    assert_eq!(InputProfile::parse("raw").unwrap(), InputProfile::RAW);
    assert_eq!(InputProfile::parse("raw:1").unwrap(), InputProfile::RAW);
    assert_eq!(InputProfile::parse("raw:2").unwrap().identity(), "raw:2");
    let eg2 = InputProfile::parse("embeddinggemma-2").unwrap();
    assert_eq!(eg2.identity(), "embeddinggemma-2:1");
    assert_eq!(InputProfile::parse("embeddinggemma-2:1").unwrap(), eg2);
    assert!(InputProfile::parse("unknown").is_err());
    assert!(InputProfile::parse("raw:0").is_err());
    assert!(InputProfile::parse("raw:x").is_err());

    // Document chunk templates: raw passes through, EmbeddingGemma 2 formats.
    assert_eq!(
        format_docs_input(InputProfile::RAW, "Any Title", "body"),
        "body"
    );
    assert_eq!(
        format_docs_input(eg2, "Guide > Setup", "body"),
        "title: Guide > Setup | text: body"
    );
    assert_eq!(
        format_code_input(
            InputProfile::RAW,
            "src/a.ts",
            "fn main",
            Some("ctx"),
            "let x"
        ),
        "let x"
    );
    assert_eq!(
        format_code_input(
            eg2,
            "src/a.ts",
            "multiply",
            Some("src/math.ts"),
            "return a * b"
        ),
        "title: src/a.ts | text: symbol: multiply | context: src/math.ts | code: return a * b"
    );
    assert_eq!(
        format_code_input(eg2, "src/a.ts", "", None, "let x"),
        "title: src/a.ts | text: code: let x"
    );

    // Query templates.
    assert_eq!(format_docs_query(InputProfile::RAW, "q"), "q");
    assert_eq!(format_code_query(InputProfile::RAW, "q"), "q");
    assert_eq!(
        format_docs_query(eg2, "q"),
        "task: search result | query: q"
    );
    assert_eq!(
        format_code_query(eg2, "q"),
        "task: code retrieval | query: q"
    );

    // Title fallbacks: heading, then file name, then repository path.
    assert_eq!(
        docs_chunk_title("Guide > Setup", "docs/a.md"),
        "Guide > Setup"
    );
    assert_eq!(docs_chunk_title("", "docs/a.md"), "a.md");
    assert_eq!(docs_chunk_title("", "/"), "/");
}

#[test]
fn index_time_embedding_inputs_are_formatted_and_cache_keyed() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", GUIDE_MD_V1), ("src/math.ts", MATH_TS)];
    let eg2 = InputProfile::parse("embeddinggemma-2").unwrap();
    let provider = RecordingProvider::new("v1", 8, eg2);
    let built = build_scope_with_scope(
        "profile-index",
        &files,
        Some(&client_of(provider.clone())),
        "all",
    );
    let inputs = provider.received();
    assert!(!inputs.is_empty());
    assert!(
        inputs
            .iter()
            .any(|input| input.starts_with("title: ") && input.contains("| text: ")),
        "a docs chunk must be embedded with the title template: {inputs:?}"
    );
    assert!(
        inputs
            .iter()
            .any(|input| input.starts_with("title: src/math.ts")),
        "a code chunk must be embedded with the path title template: {inputs:?}"
    );

    // The published vectors (and thus the embedding-cache reference set) key
    // on the exact formatted inputs, never on the raw chunk text.
    let conn = built.store.open().unwrap();
    let (repo_id, _, _) =
        search::scope_for_search(&built.adapter, SnapshotMode::PlainDirectory).unwrap();
    let keys = store::referenced_embedding_keys(&conn, &repo_id).unwrap();
    let expected: std::collections::HashSet<String> = inputs.iter().map(hash::sha256_hex).collect();
    let actual: std::collections::HashSet<String> = keys
        .iter()
        .map(|(input_hash, _)| input_hash.clone())
        .collect();
    assert_eq!(actual, expected, "cache keys must be the formatted inputs");
}

#[test]
fn embedding_cache_never_reuses_vectors_across_templates_or_profiles() {
    let dir = temp_dir("cache-templates");
    let cache = EmbeddingCache::open(dir.join("embeddings.sqlite")).unwrap();
    let raw = RecordingProvider::new("v1", 8, InputProfile::RAW);
    let raw_session = client_of(raw.clone());
    let text = "The widget guide.";
    let raw_work = vec![EmbeddingWork {
        chunk_id: "c1".into(),
        input: text.to_string(),
        input_hash: hash::sha256_hex(text),
    }];
    let first = embed_chunks(&cache, &raw_session, &raw_work, 1).unwrap();
    assert_eq!(first.stats.embedded, 1);

    // Same profile, changed title template: the exact input differs, so the
    // raw entry must not be reused.
    let eg2 = InputProfile::parse("embeddinggemma-2").unwrap();
    let formatted = format_docs_input(eg2, "Guide", text);
    let formatted_work = vec![EmbeddingWork {
        chunk_id: "c1".into(),
        input: formatted.clone(),
        input_hash: hash::sha256_hex(&formatted),
    }];
    let second = embed_chunks(&cache, &raw_session, &formatted_work, 2).unwrap();
    assert_eq!(
        second.stats.embedded, 1,
        "a template change must miss the cache"
    );
    assert_eq!(second.stats.cache_hits, 0);

    // Same template again: cache hit without a provider call.
    let calls_before = raw.received().len();
    let third = embed_chunks(&cache, &raw_session, &formatted_work, 3).unwrap();
    assert_eq!(third.stats.cache_hits, 1);
    assert_eq!(third.stats.embedded, 0);
    assert_eq!(raw.received().len(), calls_before);

    // Same exact input, different profile identity (input profile in the
    // fingerprint): must not compare or reuse vectors.
    let eg2_session = client_of(RecordingProvider::new("v1", 8, eg2));
    let fourth = embed_chunks(&cache, &eg2_session, &formatted_work, 4).unwrap();
    assert_eq!(
        fourth.stats.embedded, 1,
        "a different input profile must miss the cache"
    );
    assert_eq!(fourth.stats.cache_hits, 0);

    // And a different model revision with the same template: also a miss.
    let other_session = client_of(RecordingProvider::new("v2", 8, eg2));
    let fifth = embed_chunks(&cache, &other_session, &formatted_work, 5).unwrap();
    assert_eq!(fifth.stats.embedded, 1);
    assert_eq!(fifth.stats.cache_hits, 0);
}

#[test]
fn hybrid_search_templates_and_splits_query_vectors_by_corpus() {
    let files: Vec<(&str, &str)> = vec![
        (
            "docs/a.md",
            "# Guide\n\nMultiply two numbers with the gizmo bearing.\n",
        ),
        ("src/math.ts", MATH_TS),
    ];
    let eg2 = InputProfile::parse("embeddinggemma-2").unwrap();
    let built = build_scope_with_scope(
        "mixed-hybrid",
        &files,
        Some(&client_of(RecordingProvider::new("v1", 8, eg2))),
        "all",
    );
    let provider = RecordingProvider::new("v1", 8, eg2);
    let embedder = RecordingQueryEmbedder {
        provider: provider.clone(),
    };

    // Unfiltered mixed corpus: both vectors in one provider batch, docs first.
    let response = run_search(
        &built,
        "gizmo multiply",
        SearchMode::Hybrid,
        Some(&embedder),
    )
    .unwrap();
    assert_eq!(response.retrieval_mode, "hybrid");
    assert_eq!(
        provider.received(),
        vec![
            format_docs_query(eg2, "gizmo multiply"),
            format_code_query(eg2, "gizmo multiply"),
        ]
    );
    let explanations: Vec<String> = response
        .results
        .iter()
        .map(|hit| hit.explanation.clone())
        .collect();
    assert!(
        explanations.iter().any(|e| e.contains("docs vector rank")),
        "docs chunks must be ranked by the document-search vector: {explanations:?}"
    );
    assert!(
        explanations.iter().any(|e| e.contains("code vector rank")),
        "code chunks must be ranked by the code-retrieval vector: {explanations:?}"
    );

    // Code role filter: only the code-retrieval vector is computed.
    provider.clear();
    let code_only = search::search(
        &built.adapter,
        SnapshotMode::PlainDirectory,
        &built.store,
        &SearchRequest {
            query: "multiply".to_string(),
            path_filter: None,
            role_filter: Some(Role::Code),
            max_results: None,
            max_output_tokens: None,
            cursor: None,
            mode: SearchMode::Hybrid,
            rrf_k: None,
        },
        Some(&embedder),
    )
    .unwrap();
    assert_eq!(code_only.retrieval_mode, "hybrid");
    assert_eq!(
        provider.received(),
        vec![format_code_query(eg2, "multiply")]
    );

    // A non-code role filter: only the document-search vector is computed.
    provider.clear();
    search::search(
        &built.adapter,
        SnapshotMode::PlainDirectory,
        &built.store,
        &SearchRequest {
            query: "multiply".to_string(),
            path_filter: None,
            role_filter: Some(Role::CurrentDoc),
            max_results: None,
            max_output_tokens: None,
            cursor: None,
            mode: SearchMode::Hybrid,
            rrf_k: None,
        },
        Some(&embedder),
    )
    .unwrap();
    assert_eq!(
        provider.received(),
        vec![format_docs_query(eg2, "multiply")]
    );
}

#[test]
fn query_and_index_profiles_must_share_the_input_profile() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", GUIDE_MD_V1)];
    let eg2 = InputProfile::parse("embeddinggemma-2").unwrap();
    let built = build_scope_with_scope(
        "profile-mismatch",
        &files,
        Some(&client_of(RecordingProvider::new("v1", 8, eg2))),
        "all",
    );

    // Same model and dimension, but a different input profile: the
    // fingerprints differ, so hybrid degrades to lexical.
    let raw_embedder = RecordingQueryEmbedder {
        provider: RecordingProvider::new("v1", 8, InputProfile::RAW),
    };
    let degraded = run_search(&built, "gizmo", SearchMode::Hybrid, Some(&raw_embedder)).unwrap();
    assert_eq!(degraded.retrieval_mode, "lexical");

    // The matching profile fuses hybrid results.
    let eg2_embedder = RecordingQueryEmbedder {
        provider: RecordingProvider::new("v1", 8, eg2),
    };
    let hybrid = run_search(&built, "gizmo", SearchMode::Hybrid, Some(&eg2_embedder)).unwrap();
    assert_eq!(hybrid.retrieval_mode, "hybrid");
}

#[test]
fn raw_profile_mixed_corpus_keeps_the_global_cosine_rank() {
    let files: Vec<(&str, &str)> = vec![
        (
            "docs/a.md",
            "# Guide\n\nMultiply two numbers with the gizmo bearing.\n",
        ),
        ("src/math.ts", MATH_TS),
    ];
    let built = build_scope_with_scope(
        "raw-global-rank",
        &files,
        Some(&client_of(RecordingProvider::new(
            "v1",
            8,
            InputProfile::RAW,
        ))),
        "all",
    );
    let query = "gizmo multiply";
    let provider = RecordingProvider::new("v1", 8, InputProfile::RAW);
    let embedder = RecordingQueryEmbedder {
        provider: provider.clone(),
    };

    // The raw templates are identity: one shared query input in one batch.
    let response = run_search(&built, query, SearchMode::VectorsOnly, Some(&embedder)).unwrap();
    assert_eq!(response.retrieval_mode, "vectors-only");
    assert_eq!(provider.received(), vec![query.to_string()]);

    // Expected pre-profile behavior: one global cosine rank over every stored
    // vector, both corpora together.
    let conn = built.store.open().unwrap();
    let fingerprint = embedder.profile_fingerprint();
    let rows = store::vectors_for_generation_with_corpus(
        &conn,
        built.generation_id,
        &fingerprint,
        None,
        None,
    )
    .unwrap();
    let query_vector = vector_for(query, 8);
    let mut expected: Vec<(String, f64)> = rows
        .iter()
        .map(|(chunk_id, _corpus, vector)| {
            (
                chunk_id.clone(),
                repoise_core::embed::cosine(&query_vector, vector),
            )
        })
        .collect();
    expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    assert!(!expected.is_empty());

    // The vectors-only page starts at the global cosine maximum and stays in
    // global cosine order (the per-file cap may skip later chunks).
    let page_ids: Vec<&str> = response
        .results
        .iter()
        .map(|hit| hit.source_id.as_str())
        .collect();
    assert_eq!(page_ids.first().copied(), Some(expected[0].0.as_str()));
    let mut last = 0usize;
    for id in page_ids {
        let position = expected
            .iter()
            .position(|(chunk_id, _score)| chunk_id == id)
            .expect("vector result must be a stored chunk");
        assert!(
            position >= last,
            "vectors-only order must follow the global cosine rank: {id:?} at {position}, after {last}"
        );
        last = position + 1;
    }

    // Hybrid keeps the single-space fusion and the original global-rank
    // attribution (no per-corpus split for raw).
    let hybrid = run_search(&built, query, SearchMode::Hybrid, Some(&embedder)).unwrap();
    assert_eq!(hybrid.retrieval_mode, "hybrid");
    for hit in &hybrid.results {
        assert!(
            !hit.explanation.contains("docs vector rank")
                && !hit.explanation.contains("code vector rank"),
            "raw hybrid results must use the global rank attribution: {:?}",
            hit.explanation
        );
    }
    assert!(
        hybrid
            .results
            .iter()
            .any(|hit| hit.explanation.contains("vector rank ")),
        "a hybrid result must carry the global vector rank attribution: {:?}",
        hybrid
            .results
            .iter()
            .map(|hit| hit.explanation.clone())
            .collect::<Vec<_>>()
    );
}

/// Query embedder whose second batch slot returns no valid vector
/// (simulating a per-slot `validate_vector` failure) — no provider needed.
struct PartialQueryEmbedder {
    inputs: Arc<Mutex<Vec<String>>>,
    fail_index: usize,
    dimension: u32,
    input_profile: InputProfile,
}

impl PartialQueryEmbedder {
    fn new(fail_index: usize, dimension: u32, input_profile: InputProfile) -> Self {
        Self {
            inputs: Arc::new(Mutex::new(Vec::new())),
            fail_index,
            dimension,
            input_profile,
        }
    }

    fn received(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }

    fn profile_for(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            provider: "test-record".to_string(),
            model: "v1".to_string(),
            dimension: self.dimension,
            profile_version: repoise_core::embed::PROFILE_VERSION,
            input_profile: self.input_profile,
        }
    }
}

impl QueryEmbedder for PartialQueryEmbedder {
    fn embed_queries(&self, inputs: &[String]) -> repoise_core::Result<Vec<Option<Vec<f32>>>> {
        self.inputs.lock().unwrap().extend(inputs.iter().cloned());
        Ok(inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                if index == self.fail_index {
                    None
                } else {
                    Some(vector_for(input, self.dimension))
                }
            })
            .collect())
    }

    fn profile_fingerprint(&self) -> String {
        self.profile_for().fingerprint()
    }

    fn input_profile(&self) -> InputProfile {
        self.input_profile
    }
}

#[test]
fn partially_failed_mixed_query_keeps_the_corpus_split() {
    let files: Vec<(&str, &str)> = vec![
        (
            "docs/a.md",
            "# Guide\n\nMultiply two numbers with the gizmo bearing.\n",
        ),
        ("src/math.ts", MATH_TS),
    ];
    let eg2 = InputProfile::parse("embeddinggemma-2").unwrap();
    let built = build_scope_with_scope(
        "partial-failure",
        &files,
        Some(&client_of(RecordingProvider::new("v1", 8, eg2))),
        "all",
    );
    let query = "multiply";
    // The code-retrieval slot (second batch input) fails validation.
    let embedder = PartialQueryEmbedder::new(1, 8, eg2);
    let response = run_search(&built, query, SearchMode::Hybrid, Some(&embedder)).unwrap();
    assert_eq!(response.retrieval_mode, "hybrid");
    assert_eq!(
        embedder.received(),
        vec![format_docs_query(eg2, query), format_code_query(eg2, query)]
    );

    // Both corpora reach the page lexically, so the check is not vacuous.
    assert!(response.results.iter().any(|hit| hit.path == "src/math.ts"));
    assert!(response.results.iter().any(|hit| hit.path == "docs/a.md"));

    // The surviving document-search vector ranks only docs chunks: code
    // chunks must not be compared against it (and the missing code vector
    // contributes no rank of its own).
    for hit in &response.results {
        assert!(
            !hit.explanation.contains("code vector rank"),
            "a missing code vector must not rank code chunks: {:?}",
            hit.explanation
        );
        if hit.path == "src/math.ts" {
            assert!(
                !hit.explanation.contains("docs vector rank"),
                "code chunks must not be compared against the docs vector: {:?}",
                hit.explanation
            );
        }
    }
    assert!(
        response
            .results
            .iter()
            .any(|hit| hit.explanation.contains("docs vector rank")),
        "docs chunks must still be ranked by the surviving docs vector: {:?}",
        response
            .results
            .iter()
            .map(|hit| hit.explanation.clone())
            .collect::<Vec<_>>()
    );
}
