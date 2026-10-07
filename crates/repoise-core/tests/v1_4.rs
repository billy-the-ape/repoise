//! K4: structural TypeScript/JavaScript chunks, symbol and reference
//! publication, FTS symbol/context columns, embedding scope and incremental
//! code reparse.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use repoise_core::adapter::SnapshotMode;
use repoise_core::adapter::fake::FakeRevisionAdapter;
use repoise_core::cache::CachePaths;
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::search::{self, SearchMode, SearchRequest};
use repoise_core::store::Store;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-k4-{tag}-{}-{n}", std::process::id()));
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

struct Harness {
    root: PathBuf,
    effective: EffectiveConfig,
    cache: CachePaths,
    store: Store,
}

fn harness(tag: &str) -> Harness {
    let root = temp_dir(tag);
    let effective = EffectiveConfig::resolve(&root, &CliOverrides::default()).unwrap();
    let cache = CachePaths::resolve(&root, ".repoise", None).unwrap();
    let adapter = fake_adapter(&root, &[]);
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let path = cache.db_path(&repo_id, &worktree_id);
    Harness {
        root,
        effective,
        cache,
        store: Store::new(path),
    }
}

const MATH_TS: &str = r#"
/** Multiplies two numbers. */
export function multiply(a: number, b: number): number {
  return a * b;
}
function add(a: number, b: number): number {
  return a + b;
}
export const result = multiply(2, 3);
"#;

const FORMAT_TS: &str = r#"
import { multiply } from "./math";
export function render(value: number): string {
  return String(multiply(value, 2));
}
"#;

const LEGACY_PY: &str = "def helper():\n    return 1\n";

fn corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("src/math.ts", MATH_TS),
        ("src/format.ts", FORMAT_TS),
        ("src/legacy.py", LEGACY_PY),
        ("docs/guide.md", "# Guide\n\nSee the API.\n"),
    ]
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

fn scope_of(h: &Harness) -> (String, String) {
    let adapter = fake_adapter(&h.root, &[]);
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    (repo_id, worktree_id)
}

fn current(h: &Harness) -> repoise_core::store::PreviousGeneration {
    let (repo_id, worktree_id) = scope_of(h);
    let conn = h.store.open().unwrap();
    repoise_core::store::load_generation(&conn, &repo_id, &worktree_id)
        .unwrap()
        .unwrap()
}

#[test]
fn code_files_chunk_with_symbols_and_reference_edges() {
    let h = harness("code");
    let outcome = build(&corpus(), &h);
    assert!(outcome.chunks_total > 0);
    let generation = current(&h);

    // File rows: code corpus with the pinned grammar versions.
    let math = generation
        .files
        .iter()
        .find(|f| f.path == "src/math.ts")
        .unwrap();
    assert_eq!(math.corpus.as_deref(), Some("code"));
    assert_eq!(
        math.parser_version.as_deref(),
        Some("ts/tree-sitter-0.23.2/1")
    );
    let format = generation
        .files
        .iter()
        .find(|f| f.path == "src/format.ts")
        .unwrap();
    assert_eq!(format.corpus.as_deref(), Some("code"));
    let legacy = generation
        .files
        .iter()
        .find(|f| f.path == "src/legacy.py")
        .unwrap();
    assert_eq!(legacy.corpus.as_deref(), Some("code"));
    assert_eq!(legacy.parser_version.as_deref(), Some("code-line/1"));

    // Symbols: declared functions, exported and local.
    let math_symbols: Vec<_> = generation
        .symbols
        .iter()
        .filter(|s| s.path == "src/math.ts")
        .collect();
    let multiply = math_symbols
        .iter()
        .find(|s| s.name == "multiply")
        .expect("multiply symbol");
    assert_eq!(multiply.kind, "function");
    assert!(multiply.exported);
    assert!(multiply.chunk_id.is_some());
    let add = math_symbols
        .iter()
        .find(|s| s.name == "add")
        .expect("add symbol");
    assert_eq!(add.kind, "function");
    assert!(!add.exported);
    // Line fallback files produce no symbols.
    assert!(generation.symbols.iter().all(|s| s.path != "src/legacy.py"));

    // Import edge: resolved to math.ts with certain confidence.
    let import = generation
        .references
        .iter()
        .find(|r| r.path == "src/format.ts" && r.kind == "import" && r.name == "multiply")
        .expect("import edge");
    assert_eq!(import.confidence, "certain");
    assert_eq!(
        import.target_symbol_id.as_deref(),
        Some(multiply.symbol_id.as_str())
    );

    // Call reference: unqualified call resolves through the import binding.
    let call = generation
        .references
        .iter()
        .find(|r| r.path == "src/format.ts" && r.kind == "call" && r.name == "multiply")
        .expect("call edge");
    assert_eq!(call.confidence, "uncertain");
    assert_eq!(
        call.target_symbol_id.as_deref(),
        Some(multiply.symbol_id.as_str())
    );

    // The multiply chunk carries its primary symbol for lexical matching.
    let multiply_chunk = generation
        .chunks
        .iter()
        .find(|c| c.chunk_id == multiply.chunk_id.clone().unwrap())
        .unwrap();
    assert_eq!(multiply_chunk.symbol, "multiply");
    assert!(multiply_chunk.text.contains("export function multiply"));
    assert_eq!(multiply_chunk.corpus, "code");
}

#[test]
fn lexical_search_reaches_code_chunks_by_symbol() {
    let h = harness("search");
    build(&corpus(), &h);
    let adapter = fake_adapter(&h.root, &[]);
    let response = search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &SearchRequest {
            query: "multiply".to_string(),
            mode: SearchMode::Lexical,
            max_results: Some(5),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(
        response
            .results
            .iter()
            .any(|hit| hit.path == "src/math.ts" && !hit.excerpt.is_empty())
    );
    // Docs remain reachable in the same index.
    let doc_response = search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &h.store,
        &SearchRequest {
            query: "Guide".to_string(),
            mode: SearchMode::Lexical,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(
        doc_response
            .results
            .iter()
            .any(|hit| hit.path == "docs/guide.md")
    );
}

// Unchanged code files are reused and changed files reparse. An identical
// rebuild reuses everything; a changed file reparses and its dependents'
// edges re-resolve against the new symbol set.
#[test]
fn unchanged_code_files_are_reused_and_changed_files_reparse() {
    let h = harness("incremental");
    let first = build(&corpus(), &h);
    assert!(first.files_reparsed > 0);

    // Identical rebuild: everything is reused, no reparsing.
    let second = build(&corpus(), &h);
    assert_eq!(second.files_reparsed, 0);
    assert_eq!(second.files_reused, second.files_indexed);
    assert_eq!(second.chunks_added, 0);
    assert_eq!(second.chunks_removed, 0);

    // Change math.ts: only that file reparses; the format.ts import edge is
    // re-resolved against the new symbol set.
    let changed_files: Vec<(&str, String)> = corpus()
        .into_iter()
        .map(|(path, content)| {
            if path == "src/math.ts" {
                (path, format!("{MATH_TS}\n// touched\n"))
            } else {
                (path, content.to_string())
            }
        })
        .collect();
    let changed: Vec<(&str, &str)> = changed_files
        .iter()
        .map(|(path, content)| (*path, content.as_str()))
        .collect();
    let third = build(&changed, &h);
    assert_eq!(third.files_reparsed, 1);

    let generation = current(&h);
    let import = generation
        .references
        .iter()
        .find(|r| r.path == "src/format.ts" && r.kind == "import" && r.name == "multiply")
        .expect("import edge after reparse");
    assert_eq!(import.confidence, "certain");
}
