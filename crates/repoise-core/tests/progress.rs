//! Index progress contract (pre-v1 reliability fix): phase events flow in
//! execution order with reliable denominators, incremental runs keep reuse
//! accounting honest, embedding progress covers cache and provider work,
//! and enrichment reports per-item progress. No terminal rendering, no
//! timing dependence: these tests observe the structured events only.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use repoise_core::adapter::SnapshotMode;
use repoise_core::adapter::fake::FakeRevisionAdapter;
use repoise_core::cache::CachePaths;
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::embed::{
    CancellationToken, EmbeddingBatch, EmbeddingBudgets, EmbeddingCache, EmbeddingClient,
    EmbeddingProfile, EmbeddingProvider, EmbeddingWork, embed_chunks,
};
use repoise_core::hash;
use repoise_core::history::{
    EnrichmentBudgets, EnrichmentError, EnrichmentOutcome, EnrichmentProvider, EnrichmentSession,
    HistoryItem, HistorySet, HostCache, RequestBudget,
};
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::progress::{IndexPhase, IndexProgressReporter};
use repoise_core::search;
use repoise_core::store::Store;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("repoise-progress-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Started(IndexPhase, Option<u64>),
    Progress(IndexPhase, u64, u64),
    Finished(IndexPhase),
}

#[derive(Clone, Default)]
struct Recording {
    events: Vec<Event>,
}

/// Records every event for assertions.
struct Reporter(Arc<Mutex<Recording>>);

impl IndexProgressReporter for Reporter {
    fn phase_started(&self, phase: IndexPhase, total: Option<u64>) {
        self.0
            .lock()
            .unwrap()
            .events
            .push(Event::Started(phase, total));
    }
    fn progress(&self, phase: IndexPhase, done: u64, total: u64) {
        self.0
            .lock()
            .unwrap()
            .events
            .push(Event::Progress(phase, done, total));
    }
    fn phase_finished(&self, phase: IndexPhase) {
        self.0.lock().unwrap().events.push(Event::Finished(phase));
    }
}

/// Three files that exercise all three chunkers.
fn corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("docs/guide.md", "# Guide\n\nBody text.\n"),
        ("src/app.ts", "export const a = 1;\n"),
        ("src/legacy.py", "def helper():\n    return 1\n"),
    ]
}

fn build_scope(tag: &str) -> (FakeRevisionAdapter, EffectiveConfig, CachePaths, Store) {
    let root = temp_dir(tag);
    let mut tree = BTreeMap::new();
    for (path, content) in corpus() {
        tree.insert(path.to_string(), content.as_bytes().to_vec());
    }
    let revisions: BTreeMap<String, BTreeMap<String, Vec<u8>>> =
        BTreeMap::from([("fake:v1".to_string(), tree)]);
    let adapter = FakeRevisionAdapter::new(&root, revisions);
    let effective = EffectiveConfig::resolve(&root, &CliOverrides::default()).unwrap();
    let cache = CachePaths::resolve(&root, ".repoise", None).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    (adapter, effective, cache, store)
}

#[test]
fn index_reports_phase_events_in_order_with_reliable_denominators() {
    let (adapter, effective, cache, store) = build_scope("phases");
    let recording = Arc::new(Mutex::new(Recording::default()));
    let reporter = Reporter(recording.clone());
    let outcome = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        None,
        None,
        Some(&reporter),
    )
    .unwrap();

    let events = recording.lock().unwrap().events.clone();
    let position = |event: Event| events.iter().position(|e| *e == event);
    let discovery_start = position(Event::Started(IndexPhase::Discovery, Some(3)))
        .expect("discovery starts with the enumeration as its denominator");
    let discovery_end =
        position(Event::Finished(IndexPhase::Discovery)).expect("discovery completes");
    let reconcile_start = position(Event::Started(IndexPhase::Reconcile, Some(3)))
        .expect("reconcile starts with the file count as its denominator");
    let reconcile_end =
        position(Event::Finished(IndexPhase::Reconcile)).expect("reconcile completes");
    let publication_start =
        position(Event::Started(IndexPhase::Publication, None)).expect("publication starts");
    let publication_end =
        position(Event::Finished(IndexPhase::Publication)).expect("publication completes");
    for pair in [
        (discovery_start, discovery_end),
        (discovery_end, reconcile_start),
        (reconcile_start, reconcile_end),
        (reconcile_end, publication_start),
        (publication_start, publication_end),
    ] {
        assert!(
            pair.0 < pair.1,
            "expected phase order {:?} < {:?}",
            pair.0,
            pair.1
        );
    }

    // Discovery and reconcile progress is monotonic and ends exactly at the
    // denominator; the denominator never changes mid-phase.
    for phase in [IndexPhase::Discovery, IndexPhase::Reconcile] {
        let counts: Vec<Event> = events
            .iter()
            .filter(|e| matches!(e, Event::Progress(p, _, _) if *p == phase))
            .cloned()
            .collect();
        assert!(!counts.is_empty(), "phase {phase:?} must report progress");
        assert!(
            counts.iter().all(|e| matches!(e, Event::Progress(_, _, 3))),
            "the denominator must stay the enumeration/file count"
        );
        let last = counts.last().unwrap();
        assert_eq!(last, &Event::Progress(phase, 3, 3));
        let mut done = 0u64;
        for e in &counts {
            if let Event::Progress(_, value, _) = e {
                assert!(*value >= done, "progress must be monotonic");
                done = *value;
            }
        }
    }

    // Lexical-only, history-off build: no embedding or history events.
    assert!(
        events.iter().all(|e| !matches!(
            e,
            Event::Started(IndexPhase::Embedding, _)
                | Event::Progress(IndexPhase::Embedding, _, _)
                | Event::Finished(IndexPhase::Embedding)
                | Event::Started(IndexPhase::History, _)
                | Event::Progress(IndexPhase::History, _, _)
                | Event::Finished(IndexPhase::History)
        )),
        "embedding and history events are opt-in"
    );

    // The first full build reparses everything.
    assert_eq!(outcome.files_indexed, 3);
    assert_eq!(outcome.files_reparsed, 3);
    assert_eq!(outcome.files_reused, 0);
}

#[test]
fn incremental_index_reports_reuse_not_reparse() {
    let (adapter, effective, cache, store) = build_scope("incremental");
    let first = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(first.files_reparsed, 3);
    assert_eq!(first.files_reused, 0);

    let recording = Arc::new(Mutex::new(Recording::default()));
    let reporter = Reporter(recording.clone());
    let second = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        None,
        None,
        Some(&reporter),
    )
    .unwrap();

    // Unchanged files are reused, never reparsed.
    assert_eq!(second.files_reparsed, 0);
    assert_eq!(second.files_reused, 3);
    // Reconcile still covers every inventoried file: its denominator is the
    // file count, not the reparsed-file count, so a fast incremental run
    // still reports a full, honest sweep.
    let events = recording.lock().unwrap().events.clone();
    let reconcile: Vec<Event> = events
        .iter()
        .filter(|e| matches!(e, Event::Progress(IndexPhase::Reconcile, _, _)))
        .cloned()
        .collect();
    assert!(
        reconcile
            .iter()
            .all(|e| matches!(e, Event::Progress(_, _, 3))),
        "reconcile denominator stays the file count"
    );
    assert_eq!(
        reconcile.last(),
        Some(&Event::Progress(IndexPhase::Reconcile, 3, 3))
    );
}

/// Deterministic fake provider: dimension-many pseudo-random floats per input.
struct HashProvider {
    model: String,
    dimension: u32,
}

impl EmbeddingProvider for HashProvider {
    fn profile(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            provider: "test-hash".to_string(),
            model: self.model.clone(),
            dimension: self.dimension,
            profile_version: repoise_core::embed::PROFILE_VERSION,
        }
    }
    fn embed(&self, inputs: &[String]) -> repoise_core::Result<EmbeddingBatch> {
        Ok(EmbeddingBatch {
            vectors: inputs
                .iter()
                .map(|input| deterministic_vector(input, self.dimension))
                .collect(),
            model_revision: None,
        })
    }
}

fn deterministic_vector(input: &str, dimension: u32) -> Vec<f32> {
    let mut hash = 0u64;
    for byte in input.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u64);
    }
    (0..dimension)
        .map(|slot| {
            let value = (hash ^ ((slot as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))) & 0xFF;
            value as f32 / 255.0
        })
        .collect()
}

fn embedding_work(texts: &[&str]) -> Vec<EmbeddingWork> {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| EmbeddingWork {
            chunk_id: format!("c{i}"),
            input: (*text).to_string(),
            input_hash: hash::sha256_hex(text),
        })
        .collect()
}

#[test]
fn embed_chunks_reports_cache_then_provider_progress() {
    let dir = temp_dir("embed-progress");
    let cache = EmbeddingCache::open(dir.join("embeddings.sqlite")).unwrap();
    let client = EmbeddingClient::new(
        Box::new(HashProvider {
            model: "v1".into(),
            dimension: 8,
        }),
        EmbeddingBudgets::default(),
        CancellationToken::new(),
    );
    let work = embedding_work(&["alpha", "beta", "gamma"]);

    // Cold cache: the first report comes from provider work, per batch.
    let seen = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let first = {
        let updates = seen.clone();
        embed_chunks(
            &cache,
            &client,
            &work,
            1,
            Some(&mut |done, total| {
                updates.lock().unwrap().push((done, total));
            }),
        )
        .unwrap()
    };
    let updates = seen.lock().unwrap().clone();
    assert_eq!(updates.first(), Some(&(0, 3)));
    assert_eq!(updates.last(), Some(&(3, 3)));
    assert!(
        updates
            .windows(2)
            .all(|w| w[0].1 == w[1].1 && w[0].0 <= w[1].0),
        "progress must be monotonic with a stable denominator"
    );
    assert_eq!(first.vectors.len(), 3);

    // Warm cache: the total is reached entirely from cache, with no
    // provider call.
    let seen = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let second = {
        let updates = seen.clone();
        embed_chunks(
            &cache,
            &client,
            &work,
            2,
            Some(&mut |done, total| {
                updates.lock().unwrap().push((done, total));
            }),
        )
        .unwrap()
    };
    assert_eq!(seen.lock().unwrap().as_slice(), &[(3, 3)]);
    assert_eq!(second.stats.cache_hits, 3);
    assert_eq!(second.stats.embedded, 0);
    assert_eq!(second.vectors.len(), 3);
}

#[test]
fn embed_chunks_mixed_cache_keeps_stable_denominator() {
    let dir = temp_dir("embed-mixed");
    let cache = EmbeddingCache::open(dir.join("embeddings.sqlite")).unwrap();
    let client = EmbeddingClient::new(
        Box::new(HashProvider {
            model: "v1".into(),
            dimension: 8,
        }),
        EmbeddingBudgets::default(),
        CancellationToken::new(),
    );
    let work = embedding_work(&["alpha", "beta", "gamma"]);

    // Warm exactly one input so the next run is mixed hit/miss.
    embed_chunks(&cache, &client, &work[..1], 1, None).unwrap();

    // Mixed run: 1 cache hit, 2 misses. The denominator must stay the full
    // work size, never the miss count.
    let seen = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let outcome = {
        let updates = seen.clone();
        embed_chunks(
            &cache,
            &client,
            &work,
            2,
            Some(&mut |done, total| {
                updates.lock().unwrap().push((done, total));
            }),
        )
        .unwrap()
    };
    let updates = seen.lock().unwrap().clone();
    assert!(
        updates.iter().all(|(_, total)| *total == 3),
        "denominator must stay the work size: {updates:?}"
    );
    assert_eq!(
        updates.first(),
        Some(&(1, 3)),
        "cache hits report before any provider work"
    );
    assert_eq!(updates.last(), Some(&(3, 3)));
    assert!(
        updates
            .windows(2)
            .all(|w| w[0].1 == w[1].1 && w[0].0 <= w[1].0)
    );
    assert_eq!(outcome.stats.cache_hits, 1);
    assert_eq!(outcome.stats.embedded, 2);
    assert_eq!(outcome.vectors.len(), 3);
}

#[test]
fn embed_all_reports_per_batch_and_stops_at_budget() {
    let budgets = EmbeddingBudgets {
        batch_size: 2,
        max_requests_per_build: 2,
        ..EmbeddingBudgets::default()
    };
    let client = EmbeddingClient::new(
        Box::new(HashProvider {
            model: "v1".into(),
            dimension: 8,
        }),
        budgets,
        CancellationToken::new(),
    );
    let inputs: Vec<String> = (0..5).map(|i| format!("input-{i}")).collect();
    let updates = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let (outcomes, stats) = {
        let seen = updates.clone();
        client.embed_all(
            &inputs,
            Some(&mut |done, total| {
                seen.lock().unwrap().push((done, total));
            }),
        )
    };
    assert_eq!(stats.embedded, 4);
    assert_eq!(stats.pending, 1);
    // Two batches of two land, then the budget stops the session; the final
    // report covers the early stop.
    assert_eq!(
        updates.lock().unwrap().as_slice(),
        &[(2, 5), (4, 5), (4, 5)]
    );
    assert_eq!(outcomes.iter().filter(|v| v.is_some()).count(), 4);
    assert!(
        outcomes
            .iter()
            .all(|v| v.as_ref().is_none_or(|vec| vec.len() == 8))
    );
}

/// A fake host provider: every hint resolves to "no association".
struct FakeEnrichment;

impl EnrichmentProvider for FakeEnrichment {
    fn host(&self) -> &'static str {
        "github"
    }
    fn remote_host(&self) -> &'static str {
        "github.com"
    }
    fn enrich(
        &self,
        _remote: &str,
        _revision_id: &str,
        _hints: &[u32],
        _cache: &HostCache,
        _budget: &mut RequestBudget,
    ) -> Result<EnrichmentOutcome, EnrichmentError> {
        Ok(EnrichmentOutcome::NoAssociation)
    }
}

fn history_item(revision: &str, hint: u32) -> HistoryItem {
    HistoryItem {
        revision_id: revision.to_string(),
        parents: Vec::new(),
        message: "commit message".to_string(),
        author: None,
        committed_at_ms: None,
        affected_paths: Vec::new(),
        paths_truncated: 0,
        hunks: Vec::new(),
        hunks_truncated: 0,
        pr_hint: Some(vec![hint]),
        association: None,
    }
}

#[test]
fn enrichment_reports_per_item_progress() {
    let dir = temp_dir("enrichment-progress");
    let cache = HostCache::open(dir.join("host-cache.json"), "github");
    let session = EnrichmentSession::new(
        Box::new(FakeEnrichment),
        cache,
        EnrichmentBudgets::default(),
    );
    let mut set = HistorySet {
        count: 2,
        items: vec![history_item("fake:r2", 7), history_item("fake:r1", 9)],
        ..HistorySet::default()
    };
    let updates = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let report = {
        let seen = updates.clone();
        session.run(
            Some("github.com/owner/repo"),
            &mut set,
            Some(&mut |done, total| {
                seen.lock().unwrap().push((done, total));
            }),
        )
    };
    assert_eq!(report.attempted, 2);
    assert_eq!(report.no_association, 2);
    // One report per item plus the final report.
    assert_eq!(
        updates.lock().unwrap().as_slice(),
        &[(1, 2), (2, 2), (2, 2)]
    );
}

#[test]
fn history_phase_brackets_collection_and_enrichment() {
    let (adapter, mut effective, cache, store) = build_scope("history-phase");
    effective.history.enabled = true;
    let recording = Arc::new(Mutex::new(Recording::default()));
    let reporter = Reporter(recording.clone());
    let outcome = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        None,
        None,
        Some(&reporter),
    )
    .unwrap();

    let events = recording.lock().unwrap().events.clone();
    let position = |event: Event| events.iter().position(|e| *e == event);
    let reconcile_end = position(Event::Finished(IndexPhase::Reconcile)).unwrap();
    let history_start = position(Event::Started(IndexPhase::History, None))
        .expect("history starts without a reliable denominator");
    let history_end = position(Event::Finished(IndexPhase::History)).unwrap();
    let publication_start = position(Event::Started(IndexPhase::Publication, None)).unwrap();
    assert!(reconcile_end < history_start);
    assert!(history_start < history_end);
    assert!(history_end < publication_start);

    // The fake adapter has no history capability: the build records an
    // explicit gap instead of failing.
    let summary = outcome.history.as_ref().expect("history lane summary");
    assert!(
        summary.gaps.iter().any(|gap| gap.contains("unavailable")),
        "no-capability adapter must record an explicit gap"
    );
}
