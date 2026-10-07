//! K2: structural chunkers, generation storage, incremental indexing,
//! offline lexical search, exact reads, cache purge and operator status.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use repoise_core::adapter::SnapshotMode;
use repoise_core::adapter::fake::FakeRevisionAdapter;
use repoise_core::cache::CachePaths;
use repoise_core::chunk::{config_fmt::ConfigChunker, markdown::MarkdownChunker};
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::purge::{self, PurgeRequest};
use repoise_core::read::{self, ReadRequest};
use repoise_core::search::{self, SearchRequest};
use repoise_core::store::Store;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-k2-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake (non-Git) adapter holding one revision of `files` under `label`.
fn fake_adapter(label: &Path, files: &[(&str, &str)]) -> FakeRevisionAdapter {
    let mut tree = BTreeMap::new();
    for (path, content) in files {
        tree.insert((*path).to_string(), content.as_bytes().to_vec());
    }
    let mut revisions = BTreeMap::new();
    revisions.insert("fake:v1".to_string(), tree);
    FakeRevisionAdapter::new(label, revisions)
}

fn effective(root: &Path) -> EffectiveConfig {
    EffectiveConfig::resolve(root, &CliOverrides::default()).unwrap()
}

fn cache_for(root: &Path) -> CachePaths {
    CachePaths::resolve(root, ".repoise", None).unwrap()
}

/// Resolves the scope db path for a fake adapter under `root`.
fn store_for(root: &Path) -> (Store, PathBuf) {
    let adapter = fake_adapter(root, &[]);
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let cache = cache_for(root);
    let path = cache.db_path(&repo_id, &worktree_id);
    (Store::new(path.clone()), path)
}

#[test]
fn markdown_chunker_sections_headings_fences_and_oversized_splits() {
    use repoise_core::chunk::{Chunk as Parsed, Chunker, MAX_CHUNK_TOKENS, estimate_tokens};
    let chunker = MarkdownChunker;
    let big: String = (0..400)
        .map(|i| format!("alpha line {i} widget word"))
        .collect::<Vec<_>>()
        .join("\n"); // ~2400 estimated tokens => oversized
    let md = format!(
        "# Title\n\nIntro about the widget assembly.\n\n## Alpha\n\n{big}\n\n## Beta\n\n```\ncode with a fence\n```\n\n| a | b |\n| - | - |\n| 1 | 2 |\n"
    );
    let chunks: Vec<Parsed> = chunker.parse("docs/a.md", &md);
    assert!(!chunks.is_empty());
    // The oversized Alpha body produces a hosting chunk plus labeled
    // continuation chunks that reference the parent.
    let alpha: Vec<&Parsed> = chunks
        .iter()
        .filter(|c| c.heading_path.iter().any(|h| h == "Alpha"))
        .collect();
    assert!(
        alpha.len() >= 2,
        "oversized section should split: {}",
        alpha.len()
    );
    assert!(
        alpha.iter().any(|c| c.parent.is_none()),
        "a hosting chunk must exist"
    );
    assert!(
        alpha.iter().any(|c| c.parent.is_some()),
        "a labeled continuation must exist"
    );
    // No chunk may exceed the hard token ceiling.
    assert!(
        chunks
            .iter()
            .all(|c| estimate_tokens(&c.text) <= MAX_CHUNK_TOKENS),
        "a chunk exceeded the hard ceiling"
    );
    // Ranges are always inside the file; synthetic label lines are never
    // inside a range, and the range's content is present in the chunk text.
    let lines: Vec<&str> = md.lines().collect();
    for chunk in &chunks {
        assert!(
            (1..=lines.len() as u32).contains(&chunk.line_start)
                && chunk.line_end >= chunk.line_start,
            "bad range {:?}-{:?}",
            chunk.line_start,
            chunk.line_end
        );
        let src: Vec<&str> = lines
            .get((chunk.line_start as usize - 1)..chunk.line_end as usize)
            .unwrap()
            .iter()
            .map(|s| s.trim_end_matches('\r'))
            .filter(|s| !s.trim().is_empty())
            .collect();
        assert!(!src.is_empty(), "empty source range");
        assert!(
            src.iter().all(|line| chunk.text.contains(line)),
            "chunk text must contain its range lines"
        );
        for text_line in chunk.text.lines() {
            if text_line.starts_with("> context:") {
                continue; // synthetic label
            }
            assert!(
                lines
                    .iter()
                    .map(|s| s.trim_end_matches('\r'))
                    .any(|s| s == text_line),
                "non-source line in chunk text: {text_line:?}"
            );
        }
    }
    // Fence content is kept inside one chunk (not split inside the fence).
    assert!(
        chunks.iter().any(|c| c.text.contains("code with a fence")),
        "fenced block must survive intact"
    );
    assert!(
        chunks.iter().any(|c| c.text.contains("| 1 | 2 |")),
        "table rows must survive intact"
    );
    assert!(
        chunks.iter().any(|c| c.text.contains("widget assembly")),
        "intro must survive"
    );
}

#[test]
fn config_chunker_groups_toml_sections_and_keeps_exact_lines() {
    use repoise_core::chunk::{Chunk as Parsed, Chunker};
    let toml = "\
[server]
host = \"localhost\"
port = 8080

[server.tls]
enabled = true

[clients]
";
    let chunker = ConfigChunker { sectioned: true };
    let chunks: Vec<Parsed> = chunker.parse("config.toml", toml);
    assert!(!chunks.is_empty());
    // TOML dotted sections are joined with '.' (not ' > ').
    let sections: Vec<String> = chunks.iter().map(|c| c.heading_path.join(" > ")).collect();
    assert!(sections.iter().any(|s| s == "server"), "{sections:?}");
    assert!(sections.iter().any(|s| s == "server.tls"), "{sections:?}");
    // The empty [clients] section has no units and produces no chunk.
    assert!(!sections.iter().any(|s| s == "clients"), "{sections:?}");
    // Exact lines: the tls section's chunk contains its key and nothing of
    // the host line.
    let tls = chunks
        .iter()
        .find(|c| c.heading_path.join(" > ") == "server.tls")
        .unwrap();
    assert!(tls.text.contains("enabled = true"));
    assert!(!tls.text.contains("host ="));
    let lines: Vec<&str> = toml.lines().collect();
    for chunk in &chunks {
        let src: Vec<&str> = lines
            .get((chunk.line_start as usize - 1)..chunk.line_end as usize)
            .unwrap()
            .to_vec();
        assert!(
            src.iter().all(|l| chunk.text.contains(l)),
            "config chunk text must contain its range lines"
        );
    }
}

#[test]
fn store_publishes_generations_and_retains_current_plus_previous() {
    let root = temp_dir("store");
    let (store, path) = store_for(&root);
    let conn = store.open().unwrap();
    for generation in 1..=3i64 {
        let file = repoise_core::store::FileRow {
            path: "docs/a.md".to_string(),
            content_hash: format!("hash{generation}"),
            role: "current-doc".to_string(),
            lifecycle: "accepted".to_string(),
            classification_source: "role-rule".to_string(),
            language: "markdown".to_string(),
            parser_version: Some("markdown/1".to_string()),
            corpus: Some("docs".to_string()),
            parser_errors: 0,
            size: 10,
        };
        let chunk = repoise_core::store::ChunkRow {
            chunk_id: format!("chunk{generation}"),
            parent_chunk_id: None,
            path: "docs/a.md".to_string(),
            heading_path: "Title".to_string(),
            corpus: "docs".to_string(),
            text: format!("body {generation}"),
            text_hash: format!("th{generation}"),
            symbol: String::new(),
            context: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            byte_end: 9,
        };
        repoise_core::store::publish(
            &conn,
            &repoise_core::store::GenerationInput {
                repo_id: "repo".to_string(),
                worktree_id: "wt".to_string(),
                snapshot_id: format!("snap{generation}"),
                snapshot_mode: "PlainDirectory".to_string(),
                revision_id: Some(format!("fake:v{generation}")),
                manifest_hash: format!("manifest{generation}"),
                config_fingerprint: "cfg".to_string(),
                parser_fingerprint: "parsers".to_string(),
                built_at_ms: generation,
                files: vec![file],
                chunks: vec![chunk],
                vectors: Vec::new(),
                vector_profile: None,
                embedding_scope: repoise_core::embed::EmbeddingScope::Docs,
                symbols: Vec::new(),
                references: Vec::new(),
            },
        )
        .unwrap();
    }
    let current = repoise_core::store::current_generation(&conn, "repo", "wt")
        .unwrap()
        .unwrap();
    assert_eq!(current.generation_id, 3);
    assert_eq!(current.snapshot_id, "snap3");
    // load_generation serves the records of the current generation (this is
    // what an in-progress build reconciles against before publishing).
    let loaded = repoise_core::store::load_generation(&conn, "repo", "wt")
        .unwrap()
        .unwrap();
    assert_eq!(loaded.meta.generation_id, 3);
    assert_eq!(loaded.files.len(), 1);
    assert_eq!(loaded.files[0].content_hash, "hash3");
    assert_eq!(loaded.chunks.len(), 1);
    // Retention: only the current plus the previous generation survive.
    let count: i64 = {
        let mut stmt = conn
            .prepare(
                "SELECT COUNT(*) FROM generation WHERE repo_id = 'repo' AND worktree_id = 'wt'",
            )
            .unwrap();
        stmt.query_row([], |r| r.get(0)).unwrap()
    };
    assert_eq!(count, 2, "retention must keep exactly two generations");
    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
    let _ = path;
}

/// End-to-end engine harness over a fake (non-Git) adapter.
struct Harness {
    root: PathBuf,
    effective: EffectiveConfig,
    cache: CachePaths,
    store: Store,
}

fn harness(tag: &str, files: &[(&str, &str)]) -> Harness {
    let root = temp_dir(tag);
    let adapter = fake_adapter(&root, files);
    let effective = effective(&root);
    let cache = cache_for(&root);
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    Harness {
        root,
        effective,
        cache,
        store,
    }
}

fn build(files: &[(&str, &str)], h: &Harness) -> indexing::IndexOutcome {
    let adapter = fake_adapter(&h.root, files);
    indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.effective,
        &h.store,
        &h.cache,
        &IndexRequest::default(),
        None,
    )
    .unwrap()
}

fn search_files(files: &[(&str, &str)], h: &Harness, query: &str) -> search::SearchResponse {
    let adapter = fake_adapter(&h.root, files);
    search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &SearchRequest {
            query: query.to_string(),
            path_filter: None,
            role_filter: None,
            max_results: None,
            max_output_tokens: None,
            cursor: None,
            mode: search::SearchMode::Lexical,
            rrf_k: None,
        },
        None,
    )
    .unwrap()
}

const A_MD_V1: &str = "# Guide\n\nThe widget assembly uses a special gizmo bearing.\n\n## Details\n\nWrench the widget until it clicks.\n";
const TOML_V1: &str = "[server]\nhost = \"localhost\"\nport = 8080\n";

#[test]
fn indexing_full_build_search_and_exact_read() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", A_MD_V1), ("config.toml", TOML_V1)];
    let h = harness("e2e-build", &files);
    let outcome = build(&files, &h);
    assert_eq!(outcome.files_indexed, 2);
    assert_eq!(outcome.files_reused, 0);
    assert_eq!(outcome.files_reparsed, 2);
    assert_eq!(outcome.chunks_added, outcome.chunks_total);
    assert!(outcome.chunks_total > 0);
    assert!(!outcome.repo_id.is_empty());
    assert!(!outcome.worktree_id.is_empty());
    // state.json is written and matches the published generation.
    let state_path = h.cache.state_path(&outcome.repo_id, &outcome.worktree_id);
    assert!(state_path.exists());
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["generation_id"], outcome.generation_id);
    assert_eq!(state["repo_id"], outcome.repo_id);
    assert_eq!(state["snapshot_id"], outcome.snapshot.snapshot_id);

    let response = search_files(&files, &h, "gizmo bearing");
    assert_eq!(response.results.len(), 1);
    let hit = &response.results[0];
    assert_eq!(hit.path, "docs/a.md");
    assert!(hit.excerpt.contains("gizmo bearing"));
    assert_eq!(hit.role, "current-doc");
    assert!(!hit.source_id.is_empty());
    // Exact read-back: same text, lines and hashes as the chunk.
    let adapter = fake_adapter(&h.root, &files);
    let result = read::read(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &ReadRequest {
            source_id: hit.source_id.clone(),
        },
    )
    .unwrap();
    assert_eq!(result.path, "docs/a.md");
    assert_eq!(result.line_start, hit.line_start);
    assert_eq!(result.line_end, hit.line_end);
    assert!(result.text.contains("gizmo bearing"));
    let lines: Vec<&str> = A_MD_V1.lines().collect();
    let expected: Vec<&str> = lines[(hit.line_start as usize - 1)..hit.line_end as usize].to_vec();
    assert_eq!(result.text, expected.join("\n"));
    assert!(!result.revision_hash.is_empty());
}

#[test]
fn indexing_incremental_reuses_unchanged_and_reparses_changed() {
    let files_v1: Vec<(&str, &str)> = vec![("docs/a.md", A_MD_V1), ("config.toml", TOML_V1)];
    let h = harness("e2e-incremental", &files_v1);
    let first = build(&files_v1, &h);
    let a_md_v2 = "# Guide\n\nThe widget assembly uses a special sprocket bearing.\n\n## Details\n\nWrench the widget until it clicks.\n";
    let files_v2: Vec<(&str, &str)> = vec![("docs/a.md", a_md_v2), ("config.toml", TOML_V1)];
    let second = build(&files_v2, &h);
    assert_eq!(
        second.files_reused, 1,
        "unchanged config file reuses records"
    );
    assert_eq!(
        second.files_reparsed, 1,
        "changed markdown file is reparsed"
    );
    assert!(second.chunks_reused > 0, "unchanged sections keep ids");
    assert!(second.chunks_added > 0, "changed section produces new ids");
    assert_ne!(second.generation_id, first.generation_id);
    // Search serves only the new generation.
    let response = search_files(&files_v2, &h, "sprocket");
    assert_eq!(response.results.len(), 1);
    let stale_response = search_files(&files_v2, &h, "gizmo");
    assert!(
        stale_response.results.is_empty(),
        "old text must not be served"
    );
}

#[test]
fn read_rejects_stale_sources_and_missing_files_are_dropped() {
    let files_v1: Vec<(&str, &str)> = vec![
        ("docs/a.md", A_MD_V1),
        ("docs/b.md", "another gizmo note here\n"),
    ];
    let h = harness("e2e-stale", &files_v1);
    build(&files_v1, &h);
    let page = search_files(&files_v1, &h, "gizmo bearing");
    let hit = page.results.first().unwrap();
    let source_id = hit.source_id.clone();
    // The file then changes OUTSIDE the chunk's range, with no rebuild: the
    // chunk id (and its text) survives, but the file content hash differs.
    let a_md_v2 = "# Guide\n\nThe widget assembly uses a special gizmo bearing.\n\n## Details\n\nWrench the sprocket until it clicks.\n";
    let files_v2: Vec<(&str, &str)> = vec![
        ("docs/a.md", a_md_v2),
        ("docs/b.md", "another gizmo note here\n"),
    ];
    let adapter = fake_adapter(&h.root, &files_v2);
    // Reading that chunk fails: its file content hash changed.
    let err = read::read(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &ReadRequest { source_id },
    )
    .unwrap_err();
    assert!(
        matches!(err, repoise_core::error::Error::Stale { .. }),
        "{err:?}"
    );
    // A file deleted between builds loses its chunks from the current
    // generation.
    let files_v3: Vec<(&str, &str)> = vec![("docs/a.md", a_md_v2)];
    build(&files_v3, &h);
    let response = search_files(&files_v3, &h, "note");
    assert!(response.results.is_empty());
}

#[test]
fn search_pagination_per_file_cap_and_cursor_binding() {
    // One file with eight separate gizmo sections (per-file cap is 3) plus
    // three other files: 6 capped candidates total, so a page overflows.
    let many = "# One\n\ngizmo alpha\n\n## Two\n\ngizmo beta\n\n## Three\n\ngizmo gamma\n\n## Four\n\ngizmo delta\n\n## Five\n\ngizmo epsilon\n\n## Six\n\ngizmo zeta\n\n## Seven\n\ngizmo eta\n\n## Eight\n\ngizmo theta\n";
    let other1 = "gizmo in the other file one\n";
    let other2 = "gizmo in the other file two\n";
    let other3 = "gizmo in the other file three\n";
    let files: Vec<(&str, &str)> = vec![
        ("docs/many.md", many),
        ("docs/o1.md", other1),
        ("docs/o2.md", other2),
        ("docs/o3.md", other3),
    ];
    let h = harness("e2e-search", &files);
    // Search before any index: a clean IndexState error, not a panic.
    let adapter = fake_adapter(&h.root, &files);
    let err = search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &SearchRequest {
            query: "gizmo".to_string(),
            path_filter: None,
            role_filter: None,
            max_results: None,
            max_output_tokens: None,
            cursor: None,
            mode: search::SearchMode::Lexical,
            rrf_k: None,
        },
        None,
    )
    .unwrap_err();
    match err {
        repoise_core::error::Error::IndexState(message) => {
            assert!(message.starts_with("no published index"), "{message}");
        }
        other => panic!("expected IndexState, got {other:?}"),
    }
    build(&files, &h);
    let first_page = search_files(&files, &h, "gizmo");
    assert_eq!(first_page.results.len(), 5, "default page size is 5");
    let per_file: std::collections::HashMap<&str, usize> =
        first_page
            .results
            .iter()
            .fold(std::collections::HashMap::new(), |mut map, hit| {
                *map.entry(hit.path.as_str()).or_insert(0) += 1;
                map
            });
    assert!(
        per_file.get("docs/many.md").copied().unwrap_or(0) <= 3,
        "per-file cap is 3: {per_file:?}"
    );
    assert!(first_page.truncated);
    let cursor = first_page.next_cursor.clone().unwrap();
    // The cursor is bound to this exact query and filters.
    let bad = SearchRequest {
        query: "other".to_string(),
        path_filter: None,
        role_filter: None,
        max_results: None,
        max_output_tokens: None,
        cursor: Some(cursor.clone()),
        mode: search::SearchMode::Lexical,
        rrf_k: None,
    };
    let err =
        search::search(&adapter, SnapshotMode::PlainDirectory, &h.store, &bad, None).unwrap_err();
    assert!(
        matches!(err, repoise_core::error::Error::IndexState(_)),
        "{err:?}"
    );
    // Next page: distinct from the first page, ordered stably.
    let second = SearchRequest {
        query: "gizmo".to_string(),
        path_filter: None,
        role_filter: None,
        max_results: None,
        max_output_tokens: None,
        cursor: Some(cursor),
        mode: search::SearchMode::Lexical,
        rrf_k: None,
    };
    let second_page = search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &second,
        None,
    )
    .unwrap();
    assert!(!second_page.results.is_empty());
    let first_ids: Vec<&str> = first_page
        .results
        .iter()
        .map(|r| r.source_id.as_str())
        .collect();
    for hit in &second_page.results {
        assert!(
            !first_ids.contains(&hit.source_id.as_str()),
            "pages overlap"
        );
    }
    // A cursor from the previous generation is rejected after a reindex.
    let other1_v2 = "gizmo in the other file one, revised";
    let files_v2: Vec<(&str, &str)> = vec![
        ("docs/many.md", many),
        ("docs/o1.md", other1_v2),
        ("docs/o2.md", other2),
    ];
    build(&files_v2, &h);
    let stale_cursor = SearchRequest {
        query: "gizmo".to_string(),
        path_filter: None,
        role_filter: None,
        max_results: None,
        max_output_tokens: None,
        cursor: Some(first_page.next_cursor.clone().unwrap()),
        mode: search::SearchMode::Lexical,
        rrf_k: None,
    };
    let err = search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &stale_cursor,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, repoise_core::error::Error::IndexState(_)),
        "{err:?}"
    );
}

#[test]
fn secret_content_is_never_persisted() {
    // AKIA followed by 16 uppercase/digit characters trips the content scan.
    let secret = "AKIAABCDEFGHIJKLMNOP";
    let md = format!("# Keys\n\nAccess key {secret} for the sandbox.\n");
    let files: Vec<(&str, &str)> = vec![("docs/keys.md", &md)];
    let h = harness("e2e-secret", &files);
    let outcome = build(&files, &h);
    // Discovery skips secret-bearing files outright; nothing is persisted.
    assert_eq!(outcome.files_indexed, 0);
    assert_eq!(outcome.files_skipped, 1);
    assert_eq!(outcome.chunks_total, 0);
    // The redactor itself (defense-in-depth) replaces each shape with a
    // marker and leaves clean text untouched.
    let redacted = repoise_core::ignore::redact_secret_content(&md);
    assert!(!redacted.contains(secret));
    assert!(redacted.contains("[REDACTED]"));
    let token = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
    let redacted = repoise_core::ignore::redact_secret_content(&format!("tok {token}"));
    assert!(!redacted.contains(&token));
    assert!(redacted.contains("[REDACTED]"));
    let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow==\n-----END RSA PRIVATE KEY-----";
    let redacted = repoise_core::ignore::redact_secret_content(pem);
    assert!(!redacted.contains("MIIEow=="));
    assert!(redacted.contains("[REDACTED]"));
    let clean = "just a normal sentence";
    assert_eq!(repoise_core::ignore::redact_secret_content(clean), clean);
}

#[test]
fn status_reports_fresh_stale_and_unknown() {
    let files_v1: Vec<(&str, &str)> = vec![("docs/a.md", A_MD_V1)];
    let h = harness("e2e-status", &files_v1);
    // Unknown: fresh cache with no index yet.
    let adapter = fake_adapter(&h.root, &files_v1);
    let view = repoise_core::status::status(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.effective,
        None,
        &h.store,
        &h.cache,
    )
    .unwrap();
    assert_eq!(view.freshness.status, "unknown");
    assert!(view.index.is_none());
    // Fresh: same content as the published generation.
    build(&files_v1, &h);
    let adapter = fake_adapter(&h.root, &files_v1);
    let view = repoise_core::status::status(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.effective,
        None,
        &h.store,
        &h.cache,
    )
    .unwrap();
    assert_eq!(view.freshness.status, "fresh");
    assert!(view.index.is_some());
    assert!(view.config.problems.is_empty());
    assert!(view.cache.db_bytes > 0);
    // Stale: content changed after the build.
    let files_v2: Vec<(&str, &str)> = vec![("docs/a.md", "# Guide\n\nChanged text.\n")];
    let adapter = fake_adapter(&h.root, &files_v2);
    let view = repoise_core::status::status(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.effective,
        None,
        &h.store,
        &h.cache,
    )
    .unwrap();
    assert_eq!(view.freshness.status, "stale");
    assert!(!view.freshness.reasons.is_empty());
}

#[test]
fn purge_removes_scope_directories_and_rejects_unsafe_ids() {
    let files: Vec<(&str, &str)> = vec![("docs/a.md", A_MD_V1)];
    let h = harness("e2e-purge", &files);
    let outcome = build(&files, &h);
    let scope_dir = h.cache.worktree_dir(&outcome.repo_id, &outcome.worktree_id);
    assert!(scope_dir.exists());
    // Rejects traversal before touching anything.
    let bad = purge::purge(
        &h.cache,
        &PurgeRequest {
            all: false,
            repo_id: Some("../..".to_string()),
            worktree_id: Some("wt".to_string()),
        },
    )
    .unwrap_err();
    assert!(
        matches!(bad, repoise_core::error::Error::Policy(_)),
        "{bad:?}"
    );
    assert!(scope_dir.exists(), "bad purge must remove nothing");
    // Scope purge removes exactly the scope directory.
    let report = purge::purge(
        &h.cache,
        &PurgeRequest {
            all: false,
            repo_id: Some(outcome.repo_id.clone()),
            worktree_id: Some(outcome.worktree_id.clone()),
        },
    )
    .unwrap();
    assert_eq!(report.removed.len(), 1);
    assert!(!scope_dir.exists());
    // --all removes the generated layout, never anything above the root.
    let report = purge::purge(
        &h.cache,
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
            .any(|p| p == &h.cache.root.join("repos"))
    );
    assert!(!h.cache.root.join("repos").exists());
}
