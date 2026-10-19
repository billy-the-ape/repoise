//! Cross-worktree cache reuse for linked worktrees of one local Git
//! repository.
//!
//! Real temporary Git repositories/worktrees verify identity and
//! end-to-end behavior; a deterministic fake embedding provider with
//! instrumentation verifies vector reuse. Counters (never wall-clock time)
//! establish the parsing/provider work avoided.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use repoise_core::adapter::{
    SnapshotMode, SourceAdapter, filesystem::FilesystemAdapter, git::GitAdapter,
};
use repoise_core::cache::CachePaths;
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::embed::{
    CancellationToken, EmbeddingBatch, EmbeddingBudgets, EmbeddingCache, EmbeddingClient,
    EmbeddingProfile, EmbeddingProvider, gc_family_embeddings, validate_vector,
};
use repoise_core::hash;
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::purge::{self, PurgeRequest};
use repoise_core::read::{self, ReadRequest};
use repoise_core::search::{self, SearchRequest};
use repoise_core::shared::{self, ArtifactCache, ArtifactRef, ParseArtifact, SharingState};
use repoise_core::store::{self, Store};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-xw-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonicalize so checkout-relative store resolution matches across
    // platforms (macOS /var -> /private/var, Windows short paths).
    dir.canonicalize().unwrap()
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes `content` to `rel`, stages and commits it with `message`.
fn commit_file(root: &Path, rel: &str, content: &str, message: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    run_git(root, &["add", rel]);
    run_git(
        root,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            message,
        ],
    );
}

/// Creates a detached linked worktree of `root` at HEAD as a sibling
/// directory named `name` (relative to `root`), and returns its path.
fn linked_worktree(root: &Path, name: &str) -> PathBuf {
    let path = root
        .parent()
        .expect("worktree root has a parent")
        .join(name);
    let path_str = path.to_string_lossy().into_owned();
    run_git(root, &["worktree", "add", "--detach", &path_str, "HEAD"]);
    path.canonicalize().expect("worktree exists")
}

const GUIDE: &str = "# Guide\n\nThe assembly uses a unique gizmo bearing.\n\n## Steps\n\nWrench the widget until it clicks.\n";
const UTIL_TS: &str = "export function calculateTotal(items: number[]): number {\n  return items.reduce((sum, item) => sum + item, 0);\n}\n";
const MAIN_TS: &str = "import { calculateTotal } from './util.ts';\nexport const total = calculateTotal([1, 2, 3]);\n";
const EXTRA_MD: &str = "# Extra\n\nThe quixotic annex describes the side panel.\n";

/// The shared fixture corpus: docs, TS code with symbols/references, and a
/// minimal committed configuration.
fn write_corpus(root: &Path, config: &str) {
    commit_file(root, "repoise.config.json", config, "Add repoise config");
    commit_file(root, "docs/guide.md", GUIDE, "Add guide");
    commit_file(root, "src/util.ts", UTIL_TS, "Add util");
    commit_file(root, "src/main.ts", MAIN_TS, "Add main");
}

struct Harness {
    root: PathBuf,
    effective: EffectiveConfig,
    cache: CachePaths,
    store: Store,
    repo_id: String,
    worktree_id: String,
}

/// Resolves a working-tree harness against `root` (using the repository's
/// configured cache root, no environment override).
fn harness(root: &Path, cache_override: Option<&Path>) -> Harness {
    let effective = EffectiveConfig::resolve(root, &CliOverrides::default()).unwrap();
    let cache = match cache_override {
        Some(override_root) => {
            CachePaths::resolve(root, override_root.to_str().unwrap(), None).unwrap()
        }
        None => CachePaths::resolve(root, &effective.cache_dir, None).unwrap(),
    };
    let adapter = GitAdapter::new(root).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::WorkingTree).unwrap();
    Harness {
        root: root.to_path_buf(),
        effective,
        store: Store::new(cache.db_path(&repo_id, &worktree_id)),
        cache,
        repo_id,
        worktree_id,
    }
}

fn build(
    h: &Harness,
    mode: SnapshotMode,
    session: Option<&EmbeddingClient>,
) -> indexing::IndexOutcome {
    let adapter = GitAdapter::new(&h.root).unwrap();
    indexing::index(
        &adapter,
        mode,
        &h.effective,
        &h.store,
        &h.cache,
        &IndexRequest::default(),
        session,
        None,
    )
    .unwrap()
}

fn search_hits(h: &Harness, query: &str) -> Vec<search::SearchHit> {
    let adapter = GitAdapter::new(&h.root).unwrap();
    let request = SearchRequest {
        query: query.to_string(),
        max_results: Some(10),
        ..SearchRequest::default()
    };
    search::search(
        &adapter,
        SnapshotMode::WorkingTree,
        &h.store,
        &request,
        None,
    )
    .unwrap()
    .results
}

fn family_id(h: &Harness) -> Option<String> {
    let adapter = GitAdapter::new(&h.root).unwrap();
    match shared::resolve_sharing(&adapter, &h.effective, &h.cache).unwrap() {
        SharingState::Shared(store) => Some(store.family_id),
        _ => None,
    }
}

fn shared_root(h: &Harness) -> PathBuf {
    let adapter = GitAdapter::new(&h.root).unwrap();
    match shared::resolve_sharing(&adapter, &h.effective, &h.cache).unwrap() {
        SharingState::Shared(store) => store.root,
        other => panic!("expected shared state, got {other:?}"),
    }
}

/// Deterministic test provider: vectors are a pure function of the input.
struct HashProvider {
    calls: Arc<AtomicUsize>,
    model: String,
    dimension: u32,
}

impl HashProvider {
    fn new(model: &str, dimension: u32) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            model: model.to_string(),
            dimension,
        }
    }
}

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
        }
    }

    fn embed(&self, inputs: &[String]) -> repoise_core::Result<EmbeddingBatch> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(EmbeddingBatch {
            vectors: inputs
                .iter()
                .map(|input| vector_for(input, self.dimension))
                .collect(),
            model_revision: None,
        })
    }
}

fn client(model: &str) -> EmbeddingClient {
    EmbeddingClient::new(
        Box::new(HashProvider::new(model, 8)),
        EmbeddingBudgets::default(),
        CancellationToken::new(),
    )
}

/// Client plus a handle on the provider's call counter (document-embedding
/// calls during indexing; query embedding uses a separate embedder).
fn client_with_counter(model: &str) -> (EmbeddingClient, Arc<AtomicUsize>) {
    let provider = HashProvider::new(model, 8);
    let calls = provider.calls.clone();
    (
        EmbeddingClient::new(
            Box::new(provider),
            EmbeddingBudgets::default(),
            CancellationToken::new(),
        ),
        calls,
    )
}

fn fingerprint_for(model: &str) -> String {
    EmbeddingProfile {
        provider: "test-hash".to_string(),
        model: model.to_string(),
        dimension: 8,
        profile_version: repoise_core::embed::PROFILE_VERSION,
    }
    .fingerprint()
}
/// Behavior 1: a warm clean linked worktree reuses every parse artifact and
/// makes zero document-embedding provider calls, while still publishing its
/// own correct scope-bound index.
#[test]
fn warm_clean_worktree_reuses_artifacts_and_vectors() {
    let base = temp_dir("warm");
    let root_a = base.join("repo a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);

    let (client_a, calls_a) = client_with_counter("v1");
    let out_a = build(&a, SnapshotMode::WorkingTree, Some(&client_a));
    assert!(out_a.files_indexed >= 4);
    assert_eq!(out_a.files_family_reused, 0);
    assert_eq!(out_a.files_reparsed, out_a.files_indexed);
    let stats_a = out_a.embedding.as_ref().expect("docs embedding pass ran");
    assert!(stats_a.embedded > 0);
    // Document provider calls are batched; the counter proves work done.
    assert_eq!(calls_a.load(Ordering::SeqCst), stats_a.requests);
    assert!(stats_a.requests >= 1);
    assert!(out_a.family_id.is_some());

    // Detached linked worktree B at the same commit (sibling path with a
    // space, exercising path handling).
    let root_b = linked_worktree(&root_a, "repo b");
    let b = harness(&root_b, None);
    assert_ne!(
        a.repo_id, b.repo_id,
        "root-bound repository ids stay separate"
    );
    assert_eq!(family_id(&a), family_id(&b));

    let (client_b, calls_b) = client_with_counter("v1");
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client_b));
    assert_eq!(out_b.repo_id, b.repo_id);
    assert_eq!(
        out_b.files_family_reused, out_b.files_indexed,
        "every eligible file is rematerialized from a shared artifact"
    );
    assert_eq!(out_b.files_reparsed, 0, "no file required reparsing");
    assert_eq!(out_b.files_reused, 0, "B had no prior generation");
    let stats_b = out_b.embedding.as_ref().expect("docs embedding pass ran");
    assert_eq!(stats_b.embedded, 0, "no new document embeddings");
    assert!(stats_b.cache_hits > 0, "cached vectors are reused");
    assert_eq!(calls_b.load(Ordering::SeqCst), 0);
    assert_eq!(out_b.family_id, out_a.family_id);
    assert!(out_b.sharing_fallback.is_none());

    // The default shared store lives under the canonical Git common
    // directory, versioned and outside the corpus.
    let common = root_a.join(".git").canonicalize().unwrap();
    let store_root = common
        .join("repoise")
        .join(shared::SHARED_STORE_VERSION.to_string());
    assert!(store_root.join("parse-cache").is_dir());
    assert!(store_root.join("embeddings.sqlite").exists());
    assert!(store_root.join("members").is_dir());

    // Destination behavior matches a cold build: search and exact read in B
    // see the destination's own scope and content.
    let hits_b = search_hits(&b, "gizmo bearing");
    let hit = hits_b
        .iter()
        .find(|h| h.path == "docs/guide.md")
        .expect("guide hit");
    let read = read::read(
        &GitAdapter::new(&root_b).unwrap(),
        SnapshotMode::WorkingTree,
        &b.store,
        &ReadRequest {
            source_id: hit.source_id.clone(),
            max_output_tokens: None,
        },
    )
    .unwrap();
    assert_eq!(read.scope.worktree_id, b.worktree_id);
    assert!(read.text.contains("gizmo"));
    // A's index is unaffected and still serves its own scope.
    assert!(
        search_hits(&a, "gizmo bearing")
            .iter()
            .any(|h| h.path == "docs/guide.md")
    );
    // B's code edges resolve against its own file set.
    assert!(
        search_hits(&b, "calculateTotal")
            .iter()
            .any(|h| h.path == "src/util.ts")
    );
}

/// Behavior 2: a small delta (modify/add/delete/rename/untracked) in the
/// destination reparses only the affected files; unchanged compatible files
/// still reuse, and only the new/changed inputs are embedded.
#[test]
fn small_delta_reparses_only_affected_files() {
    let base = temp_dir("delta");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));

    let root_b = linked_worktree(&root_a, "b");
    let b = harness(&root_b, None);
    // Destination delta: modify guide, rename main.ts -> app.ts (new
    // untracked path, old path gone), delete util.ts, add untracked extra.md.
    std::fs::write(
        root_b.join("docs/guide.md"),
        format!("{GUIDE}\nNew step: the zorgon valve.\n"),
    )
    .unwrap();
    std::fs::rename(root_b.join("src/main.ts"), root_b.join("src/app.ts")).unwrap();
    std::fs::remove_file(root_b.join("src/util.ts")).unwrap();
    std::fs::write(root_b.join("docs/extra.md"), EXTRA_MD).unwrap();

    let (client_b, calls_b) = client_with_counter("v1");
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client_b));
    // Eligible in B: config, guide (modified), app.ts (renamed), extra.md
    // (untracked). Only config can be reused from the family.
    assert_eq!(out_b.files_family_reused, 1);
    assert_eq!(out_b.files_reparsed, 3);
    let stats_b = out_b.embedding.as_ref().expect("docs embedding pass ran");
    // Changed/new docs chunks miss the cache; at most an unchanged heading
    // chunk of the modified guide can hit.
    assert!(stats_b.cache_hits <= 1);
    assert!(stats_b.embedded >= 2);
    assert!(calls_b.load(Ordering::SeqCst) >= 1);

    // Destination search/read see only B's state.
    assert!(
        search_hits(&b, "quixotic")
            .iter()
            .any(|h| h.path == "docs/extra.md")
    );
    assert!(
        search_hits(&b, "zorgon")
            .iter()
            .any(|h| h.path == "docs/guide.md")
    );
    assert!(
        search_hits(&b, "calculateTotal")
            .iter()
            .all(|h| h.path != "src/util.ts"),
        "deleted util.ts is not in B"
    );
    // A is untouched: its index still contains util.ts and the old guide.
    assert!(
        search_hits(&a, "calculateTotal")
            .iter()
            .any(|h| h.path == "src/util.ts")
    );
    assert!(search_hits(&a, "zorgon").is_empty());
    assert!(search_hits(&a, "quixotic").is_empty());
}

/// Behavior 3: dirty twins with different content at the same relative path
/// each see their own state; donor source ids are never substitutable.
#[test]
fn dirty_twins_keep_isolated_destinations() {
    let base = temp_dir("twins");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));
    let root_b = linked_worktree(&root_a, "b");
    let b = harness(&root_b, None);
    build(&b, SnapshotMode::WorkingTree, Some(&client("v1")));

    // Both worktrees edit the same path to different content.
    std::fs::write(
        root_a.join("docs/guide.md"),
        format!("{GUIDE}\nOnly in A: the alpha bolt.\n"),
    )
    .unwrap();
    std::fs::write(
        root_b.join("docs/guide.md"),
        format!("{GUIDE}\nOnly in B: the beta bolt.\n"),
    )
    .unwrap();
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));
    build(&b, SnapshotMode::WorkingTree, Some(&client("v1")));

    // Each destination serves only its own content at docs/guide.md.
    let hits_a = search_hits(&a, "bolt");
    assert!(
        hits_a.iter().any(|h| h.excerpt.contains("alpha")),
        "A sees its own edit"
    );
    assert!(
        !search_hits(&b, "alpha bolt")
            .iter()
            .any(|h| h.path == "docs/guide.md"),
        "B does not see A's edit"
    );
    assert!(
        !search_hits(&a, "beta bolt")
            .iter()
            .any(|h| h.path == "docs/guide.md"),
        "A does not see B's edit"
    );

    // Exact reads validate against each destination's own snapshot.
    let hits_beta = search_hits(&b, "beta bolt");
    let hit_b = hits_beta
        .iter()
        .find(|h| h.path == "docs/guide.md")
        .expect("B guide hit");
    let read_b = read::read(
        &GitAdapter::new(&root_b).unwrap(),
        SnapshotMode::WorkingTree,
        &b.store,
        &ReadRequest {
            source_id: hit_b.source_id.clone(),
            max_output_tokens: None,
        },
    )
    .unwrap();
    assert!(read_b.text.contains("beta bolt"));
    // A's source id is not accepted for B's destination.
    let hits_alpha = search_hits(&a, "alpha bolt");
    let hit_a = hits_alpha
        .iter()
        .find(|h| h.path == "docs/guide.md")
        .expect("A guide hit");
    assert!(
        read::read(
            &GitAdapter::new(&root_b).unwrap(),
            SnapshotMode::WorkingTree,
            &b.store,
            &ReadRequest {
                source_id: hit_a.source_id.clone(),
                max_output_tokens: None,
            },
        )
        .is_err(),
        "donor source id must not be substituted"
    );
    // An edit after publication keeps the existing stale-read semantics.
    std::fs::write(
        root_b.join("docs/guide.md"),
        format!("{GUIDE}\nOnly in B: the beta bolt.\nPost-publication: gamma.\n"),
    )
    .unwrap();
    assert!(search_hits(&b, "gamma").is_empty());
    assert!(!search_hits(&b, "beta bolt").is_empty());
}

/// Behavior 4: destination policy is fully authoritative — a destination
/// exclude removes the file even though a donor indexed it, and a
/// destination with sharing disabled reparses everything.
#[test]
fn destination_policy_never_leaks_donor_content() {
    let base = temp_dir("policy");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));

    // B writes extra.md but excludes it locally.
    let root_b = linked_worktree(&root_a, "b");
    std::fs::write(root_b.join("docs/extra.md"), EXTRA_MD).unwrap();
    std::fs::write(
        root_b.join("repoise.config.json"),
        r#"{"schemaVersion": 1, "exclude": ["docs/extra.md"]}"#,
    )
    .unwrap();
    let b = harness(&root_b, None);
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert!(
        search_hits(&b, "quixotic").is_empty(),
        "destination exclude removes donor-eligible content"
    );
    // B's own config file changed (the new exclude), so only it reparses;
    // the remaining eligible files are still reused from the family.
    assert_eq!(out_b.files_family_reused, out_b.files_indexed - 1);
    assert_eq!(out_b.files_reparsed, 1);

    // Opt-out worktree D shares nothing: everything is reparsed locally.
    let root_d = linked_worktree(&root_a, "d");
    std::fs::write(
        root_d.join("repoise.config.json"),
        r#"{"schemaVersion": 1, "sharedCache": false}"#,
    )
    .unwrap();
    let d = harness(&root_d, None);
    let out_d = build(&d, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert!(out_d.family_id.is_none(), "opt-out never claims a family");
    assert_eq!(out_d.files_family_reused, 0);
    assert_eq!(out_d.files_reparsed, out_d.files_indexed);
    let adapter_d = GitAdapter::new(&root_d).unwrap();
    assert!(matches!(
        shared::resolve_sharing(&adapter_d, &d.effective, &d.cache).unwrap(),
        SharingState::Disabled
    ));
}

/// Behavior 5: embedding reuse follows the exact prepared input plus the
/// full effective profile; a changed profile misses, a changed input misses,
/// lexical-only builds never invoke a provider, and corrupt cache state is
/// rejected rather than trusted.
#[test]
fn embedding_reuse_respects_input_and_profile() {
    let base = temp_dir("embed");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    let client_a = client("v1");
    build(&a, SnapshotMode::WorkingTree, Some(&client_a));

    // B with a different model: every prepared input misses, even though
    // the raw source text is identical.
    let root_b = linked_worktree(&root_a, "b");
    let b = harness(&root_b, None);
    let (client_b, calls_b) = client_with_counter("v2");
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client_b));
    let stats_b = out_b.embedding.as_ref().unwrap();
    assert_eq!(stats_b.cache_hits, 0);
    assert!(stats_b.embedded > 0);
    assert!(calls_b.load(Ordering::SeqCst) > 0);

    // B reindexed with the original profile: everything hits the family
    // cache again (v1 vectors written by A are still valid for B).
    let (client_b2, calls_b2) = client_with_counter("v1");
    let out_b2 = build(&b, SnapshotMode::WorkingTree, Some(&client_b2));
    let stats_b2 = out_b2.embedding.as_ref().unwrap();
    assert_eq!(stats_b2.embedded, 0);
    assert!(stats_b2.cache_hits > 0);
    assert_eq!(calls_b2.load(Ordering::SeqCst), 0);

    // A changed prepared input (different chunk text) is a miss even with
    // the same profile.
    std::fs::write(
        root_b.join("docs/guide.md"),
        format!("{GUIDE}\nNew section: the delta joint.\n"),
    )
    .unwrap();
    let (client_b3, calls_b3) = client_with_counter("v1");
    let out_b3 = build(&b, SnapshotMode::WorkingTree, Some(&client_b3));
    assert!(out_b3.embedding.as_ref().unwrap().embedded >= 1);
    assert!(calls_b3.load(Ordering::SeqCst) >= 1);

    // Lexical-only worktree E: no client, no provider, no vectors, even
    // though vectors are already cached in the family.
    let root_e = linked_worktree(&root_a, "e");
    let e = harness(&root_e, None);
    let out_e = build(&e, SnapshotMode::WorkingTree, None);
    assert_eq!(out_e.vectors_total, 0);
    assert!(out_e.embedding.is_none());
    assert!(
        !search_hits(&e, "gizmo bearing").is_empty(),
        "lexical search works"
    );

    // A dimension mismatch is never a hit (wrong-dimension vectors are
    // rejected at lookup).
    let root = shared_root(&a);
    let cache = EmbeddingCache::open(root.join("embeddings.sqlite")).unwrap();
    let fp = fingerprint_for("v1");
    let conn = a.store.open().unwrap();
    let keys = store::referenced_embedding_keys(&conn, &a.repo_id).unwrap();
    let (some_hash, _) = keys
        .iter()
        .find(|(_, fingerprint)| *fingerprint == fp)
        .expect("a v1 vector is referenced");
    assert!(
        cache.get(some_hash, &fp, 8).unwrap().is_some(),
        "sanity: v1 vector"
    );
    assert!(
        cache.get(some_hash, &fp, 16).unwrap().is_none(),
        "dimension mismatch"
    );

    // Corrupt shared embedding state fails closed (no deletion, no hit): a
    // member-registered store whose embedding database is unreadable makes
    // GC refuse to run.
    let corrupt = temp_dir("embed-corrupt");
    std::fs::write(corrupt.join("embeddings.sqlite"), b"not a sqlite database").unwrap();
    shared::upsert_member(
        &corrupt,
        &a.repo_id,
        &a.worktree_id,
        &a.cache.db_path(&a.repo_id, &a.worktree_id),
    )
    .unwrap();
    assert!(gc_family_embeddings(&corrupt).is_err());
    // And the validity gate itself rejects non-finite vectors.
    assert!(validate_vector(&vector_for("x", 8), 8));
    assert!(!validate_vector(&[f32::NAN; 8], 8));
}

/// Behavior 6: scope separation — independent clones stay isolated (even
/// under one explicit external cache root), plain directories have no
/// family, submodules form their own families, and committed-mode builds
/// reuse the same family data.
#[test]
fn scope_separation_clones_plain_dirs_submodules() {
    let base = temp_dir("scopes");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));
    let family_a = family_id(&a).expect("A has a family");

    // Independent clones of A under one shared external cache root.
    let external = base.join("external cache");
    std::fs::create_dir_all(&external).unwrap();
    let root_c1 = base.join("clone1");
    let a_str = root_a.to_string_lossy().into_owned();
    let c1_str = root_c1.to_string_lossy().into_owned();
    let c2_str = base.join("clone2").to_string_lossy().into_owned();
    run_git(&root_a, &["clone", &a_str, &c1_str]);
    let root_c2 = base.join("clone2");
    run_git(&root_a, &["clone", &a_str, &c2_str]);
    let c1 = harness(&root_c1, Some(&external));
    let c2 = harness(&root_c2, Some(&external));
    assert_ne!(
        family_id(&c1),
        family_id(&c2),
        "clones are separate families"
    );
    assert_ne!(family_id(&c1).as_deref(), Some(family_a.as_str()));
    // Both build without cross-family reuse, even though they share a root.
    let out_c1 = build(&c1, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert_eq!(out_c1.files_family_reused, 0);
    let out_c2 = build(&c2, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert_eq!(out_c2.files_family_reused, 0);
    // Each family is namespaced under the explicit root.
    let fam1 = family_id(&c1).unwrap();
    let fam2 = family_id(&c2).unwrap();
    assert!(
        external
            .join("families")
            .join(&fam1)
            .join(shared::SHARED_STORE_VERSION.to_string())
            .join("parse-cache")
            .is_dir()
    );
    assert!(
        external
            .join("families")
            .join(&fam2)
            .join(shared::SHARED_STORE_VERSION.to_string())
            .join("parse-cache")
            .is_dir()
    );

    // Plain directories have no reuse family and index correctly isolated.
    let plain = temp_dir("plain");
    std::fs::create_dir_all(plain.join("docs")).unwrap();
    std::fs::write(plain.join("docs/guide.md"), GUIDE).unwrap();
    let plain_adapter = FilesystemAdapter::new(&plain).unwrap();
    let plain_eff = EffectiveConfig::resolve(&plain, &CliOverrides::default()).unwrap();
    let plain_cache = CachePaths::resolve(&plain, &plain_eff.cache_dir, None).unwrap();
    let (plain_repo, plain_wt, _) =
        search::scope_for_search(&plain_adapter, SnapshotMode::PlainDirectory).unwrap();
    let plain_store = Store::new(plain_cache.db_path(&plain_repo, &plain_wt));
    let out_plain = indexing::index(
        &plain_adapter,
        SnapshotMode::PlainDirectory,
        &plain_eff,
        &plain_store,
        &plain_cache,
        &IndexRequest::default(),
        Some(&client("v1")),
        None,
    )
    .unwrap();
    assert_eq!(out_plain.family_id, None);
    assert!(out_plain.files_family_reused == 0);
    assert!(out_plain.files_indexed > 0);
    assert!(matches!(
        shared::resolve_sharing(&plain_adapter, &plain_eff, &plain_cache).unwrap(),
        SharingState::Unavailable
    ));

    // Committed-mode build of a clean linked worktree reuses the family.
    let root_m = linked_worktree(&root_a, "m");
    let m = harness(&root_m, None);
    let out_m = build(&m, SnapshotMode::Committed, Some(&client("v1")));
    assert_eq!(out_m.files_family_reused, out_m.files_indexed);
    assert_eq!(out_m.files_reparsed, 0);
    assert_eq!(family_id(&m).as_deref(), Some(family_a.as_str()));
}

/// Behavior 6 (continued): a submodule with its own Git repository forms its
/// own reuse family and never joins the superproject's family.
#[test]
fn submodules_form_their_own_family() {
    let base = temp_dir("submodule");
    let root = base.join("super");
    std::fs::create_dir_all(&root).unwrap();
    run_git(&root, &["init", "-b", "main"]);
    write_corpus(&root, "{}");

    // A standalone nested repository to be added as a submodule.
    let sub_src = base.join("sub-source");
    std::fs::create_dir_all(&sub_src).unwrap();
    run_git(&sub_src, &["init", "-b", "main"]);
    commit_file(
        &sub_src,
        "docs/sub.md",
        "Submodule-specific zephyr text.\n",
        "sub",
    );
    let sub_src_str = sub_src.to_string_lossy().into_owned();
    run_git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--name",
            "sub",
            &sub_src_str,
            "sub",
        ],
    );

    let super_adapter = GitAdapter::new(&root).unwrap();
    let sub_adapter = GitAdapter::new(&root.join("sub")).unwrap();
    let super_family = super_adapter.reuse_family().unwrap().expect("super family");
    let sub_family = sub_adapter
        .reuse_family()
        .unwrap()
        .expect("submodule family");
    assert_ne!(
        super_family.id, sub_family.id,
        "submodule repositories must not join the superproject family"
    );
    // The submodule anchor is its own metadata directory under the
    // superproject's common directory, never the common directory itself.
    let common = root.join(".git").canonicalize().unwrap();
    assert_ne!(sub_family.anchor, common);
    assert!(sub_family.anchor.starts_with(&common));
}

/// Behavior 7: explicit cache locations stay family-namespaced; an
/// explicit `--all` purge removes the family store under an explicit
/// cache root; old or corrupt artifacts are safe misses.
#[test]
fn storage_controls_and_legacy_artifacts() {
    let base = temp_dir("storage");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    // Relative cacheDir keeps its documented resolution (against the root);
    // both worktrees resolve to the same external root.
    write_corpus(
        &root_a,
        r#"{"schemaVersion": 1, "cacheDir": "../shared-cache"}"#,
    );
    let shared_external = base.join("shared-cache");
    let a = harness(&root_a, None);
    // Relative paths keep their literal resolution (root-relative).
    assert_eq!(a.cache.root, root_a.join("..").join("shared-cache"));
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert!(shared_external.is_dir(), "external root was created");

    let root_b = linked_worktree(&root_a, "b");
    let b = harness(&root_b, None);
    assert_eq!(b.cache.root, root_b.join("..").join("shared-cache"));
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert_eq!(out_b.files_family_reused, out_b.files_indexed);
    let families = base.join("shared-cache").join("families");
    let family = family_id(&b).unwrap();
    assert!(families.join(&family).is_dir());

    // `--all` under an explicit cache root removes the family store there.
    purge::purge(
        &b.cache,
        &PurgeRequest {
            all: true,
            repo_id: None,
            worktree_id: None,
        },
    )
    .unwrap();
    assert!(!families.exists());

    // Legacy/corrupt/incompatible artifacts are safe misses, never trusted.
    let dir = temp_dir("legacy-artifacts");
    let artifacts = ArtifactCache::open(&dir);
    let artifact = ParseArtifact {
        version: shared::ARTIFACT_VERSION,
        content_hash: "h1".into(),
        language: "markdown".into(),
        parser_version: "p1".into(),
        corpus: "docs".into(),
        path: "docs/a.md".into(),
        chunks: vec![],
        symbols: vec![],
        imports: vec![],
        refs: vec![],
    };
    artifacts.put(&artifact).unwrap();
    assert!(
        artifacts
            .get("h1", "markdown", "p1", "docs", "docs/a.md")
            .unwrap()
            .is_some()
    );
    // Unknown version: miss (and the entry is dropped, rebuilt on demand).
    let stale = ParseArtifact {
        version: shared::ARTIFACT_VERSION + 1,
        ..artifact.clone()
    };
    artifacts.put(&stale).unwrap();
    assert!(
        artifacts
            .get("h1", "markdown", "p1", "docs", "docs/a.md")
            .unwrap()
            .is_none()
    );
    // Truncated / malformed artifact: miss.
    let key = hash::sha256_hex("h1|markdown|p1|docs|docs/a.md");
    let file = artifacts.dir().join(format!("{key}.json"));
    std::fs::write(&file, b"{ truncated").unwrap();
    assert!(
        artifacts
            .get("h1", "markdown", "p1", "docs", "docs/a.md")
            .unwrap()
            .is_none()
    );
    // Unknown reference kinds mark the artifact incompatible.
    assert!(
        !ParseArtifact {
            version: shared::ARTIFACT_VERSION,
            content_hash: "h1".into(),
            language: "typescript".into(),
            parser_version: "p1".into(),
            corpus: "code".into(),
            path: "src/a.ts".into(),
            chunks: vec![],
            symbols: vec![],
            imports: vec![],
            refs: vec![ArtifactRef {
                line: 1,
                name: "x".into(),
                kind: "brand-new-kind".into(),
            }],
        }
        .refs_have_known_kinds()
    );
}

/// Behavior 8: two processes building two worktrees at once coordinate on
/// the shared store (atomic artifact publication, SQLite-serialized
/// embedding writes); no partial generation or corrupt cache results.
#[test]
fn concurrent_worktree_builds_coordinate_safely() {
    let base = temp_dir("concurrent");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));

    let root_b = linked_worktree(&root_a, "b");
    let root_c = linked_worktree(&root_a, "c");
    // Child process builds c with a fresh profile (forces real concurrent
    // embedding writes into the shared store).
    let exe = std::env::current_exe().unwrap();
    let root_c_str = root_c.to_string_lossy().into_owned();
    let mut child = Command::new(exe)
        .args(["cross_worktree_child"])
        .env("XW_ROOT", &root_c_str)
        .env("XW_PROFILE", "v2")
        .spawn()
        .unwrap();
    // Parent builds b with the same fresh profile concurrently: the two
    // processes race on identical shared artifact keys and cache rows.
    let b = harness(&root_b, None);
    let client_b = client("v2");
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client_b));
    let status = child.wait().unwrap();
    assert!(status.success(), "child process build failed");
    assert_eq!(out_b.files_family_reused, out_b.files_indexed);

    // Both destination generations are complete and queryable.
    let conn_b = b.store.open().unwrap();
    let gen_b = store::load_generation(&conn_b, &b.repo_id, &b.worktree_id)
        .unwrap()
        .expect("b published");
    let c = harness(&root_c, None);
    let conn_c = c.store.open().unwrap();
    let gen_c = store::load_generation(&conn_c, &c.repo_id, &c.worktree_id)
        .unwrap()
        .expect("c published");
    assert!(!gen_b.chunks.is_empty() && !gen_c.chunks.is_empty());
    assert!(
        search_hits(&b, "gizmo bearing")
            .iter()
            .any(|h| h.path == "docs/guide.md")
    );
    assert!(
        search_hits(&c, "gizmo bearing")
            .iter()
            .any(|h| h.path == "docs/guide.md")
    );
    // Both profiles are published for both generations (no lost vectors).
    let fp2 = fingerprint_for("v2");
    // Docs-scope vectors: every docs chunk of both generations has a v2
    // vector, with nothing pending (no lost vectors under the race).
    for (previous, conn, generation_id) in [
        (&gen_b, &conn_b, gen_b.meta.generation_id),
        (&gen_c, &conn_c, gen_c.meta.generation_id),
    ] {
        let docs_paths: Vec<String> = previous
            .chunks
            .iter()
            .filter(|c| c.corpus == "docs")
            .map(|c| c.path.clone())
            .collect();
        let (_, vectors) = store::vector_coverage(conn, generation_id, Some(&fp2)).unwrap();
        assert_eq!(
            vectors,
            docs_paths.len() as i64,
            "every docs chunk has a v2 vector"
        );
        let pending = store::pending_vector_paths(conn, generation_id, Some(&fp2)).unwrap();
        assert!(
            pending.iter().all(|path| !docs_paths.contains(path)),
            "no docs chunk is pending a v2 vector"
        );
    }
    // The shared store remains consistent after the race.
    let root = shared_root(&b);
    assert!(gc_family_embeddings(&root).is_ok());
}

/// Worker executed in a spawned child process by the concurrency test.
/// During a normal suite run the environment is absent and this no-ops.
#[test]
fn cross_worktree_child() {
    let Some(root_str) = std::env::var_os("XW_ROOT") else {
        return;
    };
    let root = PathBuf::from(root_str);
    let profile = std::env::var("XW_PROFILE").unwrap_or_else(|_| "v1".into());
    let h = harness(&root, None);
    let client = client(&profile);
    let out = build(&h, SnapshotMode::WorkingTree, Some(&client));
    assert_eq!(out.files_family_reused, out.files_indexed);
    let conn = h.store.open().unwrap();
    assert!(
        store::load_generation(&conn, &h.repo_id, &h.worktree_id)
            .unwrap()
            .is_some()
    );
}

/// Behavior 9: purging or removing one worktree never invalidates siblings;
/// shared GC respects retained references; `--all` keeps its documented
/// reach (explicit roots include `families`; default roots do not touch the
/// common-directory family store).
#[test]
fn purge_and_gc_preserve_siblings() {
    let base = temp_dir("purge");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));
    let root_b = linked_worktree(&root_a, "b");
    let b = harness(&root_b, None);
    build(&b, SnapshotMode::WorkingTree, Some(&client("v1")));

    let root_shared = shared_root(&a);
    assert_eq!(shared::load_members(&root_shared).unwrap().len(), 2);

    // Worktree-scoped purge of B: A's generation and shared data survive.
    let report = purge::purge(
        &b.cache,
        &PurgeRequest {
            all: false,
            repo_id: Some(b.repo_id.clone()),
            worktree_id: Some(b.worktree_id.clone()),
        },
    )
    .unwrap();
    assert!(!report.removed.is_empty());
    let conn_a = a.store.open().unwrap();
    assert!(
        store::load_generation(&conn_a, &a.repo_id, &a.worktree_id)
            .unwrap()
            .is_some()
    );
    // Unregister B; GC still runs and retains A's vectors.
    shared::remove_member(&root_shared, &b.repo_id, &b.worktree_id).unwrap();
    assert_eq!(shared::load_members(&root_shared).unwrap().len(), 1);
    assert!(gc_family_embeddings(&root_shared).is_ok());
    let fp = fingerprint_for("v1");
    let conn_a = a.store.open().unwrap();
    let keys = store::referenced_embedding_keys(&conn_a, &a.repo_id).unwrap();
    let (hash, _) = keys
        .iter()
        .find(|(_, fingerprint)| *fingerprint == fp)
        .expect("A references v1 vectors");
    let cache = EmbeddingCache::open(root_shared.join("embeddings.sqlite")).unwrap();
    assert!(cache.get(hash, &fp, 8).unwrap().is_some());

    // Removing the worktree entirely (git) leaves a stale member for B's
    // sibling A only if A had one — B's registration was removed above, and
    // GC skips members whose database is gone, so retained vectors stay.
    let root_b_str = root_b.to_string_lossy().into_owned();
    run_git(&root_a, &["worktree", "remove", "--force", &root_b_str]);
    assert!(gc_family_embeddings(&root_shared).is_ok());
    assert!(cache.get(hash, &fp, 8).unwrap().is_some());

    // `--all` with the default checkout-local root removes the selected
    // cache root but not the common-directory family store (sibling data).
    let report = purge::purge(
        &a.cache,
        &PurgeRequest {
            all: true,
            repo_id: None,
            worktree_id: None,
        },
    )
    .unwrap();
    assert!(
        report
            .removed
            .iter()
            .any(|path| path == &a.cache.root.join("repos"))
    );
    assert!(!a.cache.root.join("repos").exists());
    assert!(
        root_shared.join("parse-cache").is_dir(),
        "the common-directory family store is outside the default cache root"
    );
}

/// Regression: a non-canonical checkout spelling (symlinked root) must
/// select the same family store as its canonical siblings, so linked
/// worktrees still share artifacts. (macOS /var -> /private/var and
/// Windows short paths produce the same mismatch for plain temp dirs.)
#[cfg(unix)]
#[test]
fn non_canonical_checkout_spelling_shares_the_family_store() {
    let base = temp_dir("symlink");
    let root_a = base.join("a");
    std::fs::create_dir_all(&root_a).unwrap();
    run_git(&root_a, &["init", "-b", "main"]);
    write_corpus(&root_a, "{}");
    let a = harness(&root_a, None);
    build(&a, SnapshotMode::WorkingTree, Some(&client("v1")));
    let store_a = shared_root(&a);

    // Rebuild the same checkout through a symlinked (non-canonical) root.
    let link = base.join("a-link");
    std::os::unix::fs::symlink(&root_a, &link).unwrap();
    let a_link = harness(&link, None);
    assert_eq!(a_link.repo_id, a.repo_id, "same scope through the symlink");
    let store_link = shared_root(&a_link);
    assert_eq!(store_link, store_a, "same family store through the symlink");
    let out_link = build(&a_link, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert_eq!(
        out_link.files_reused, out_link.files_indexed,
        "same-worktree reuse"
    );

    // A linked worktree created from the canonical root shares with both.
    let root_b = linked_worktree(&root_a, "b");
    let b = harness(&root_b, None);
    assert_eq!(shared_root(&b), store_a, "linked worktree joins the family");
    let out_b = build(&b, SnapshotMode::WorkingTree, Some(&client("v1")));
    assert_eq!(out_b.files_family_reused, out_b.files_indexed);
    assert_eq!(out_b.files_reparsed, 0);
}
