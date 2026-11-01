//! Code chunk bounds: every declaration form goes through the same
//! size-aware emission path, every splittable chunk meets the estimated
//! ceiling, and irreducible single lines are labeled, exact and searchable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use repoise_core::adapter::{SnapshotMode, fake::FakeRevisionAdapter};
use repoise_core::cache::CachePaths;
use repoise_core::chunk::code::parse_code;
use repoise_core::chunk::{MAX_CHUNK_TOKENS, estimate_tokens};
use repoise_core::config::{CliOverrides, EffectiveConfig};
use repoise_core::embed::{
    CancellationToken, EmbeddingBatch, EmbeddingBudgets, EmbeddingClient, EmbeddingProfile,
    EmbeddingProvider, PROFILE_VERSION,
};
use repoise_core::error::Error;
use repoise_core::indexing::{self, IndexRequest};
use repoise_core::search::{self, SearchMode, SearchRequest};
use repoise_core::store::{self, Store};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("repoise-bounds-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A representative multiline routes table: one large exported arrow
/// function binding (the shape the AP-06 pilot had to exclude).
fn route_file() -> String {
    let mut out = String::from("import type { Request, Response } from \"./types\";\n\n");
    out.push_str("export const chatRoutes = (req: Request, res: Response) => {\n");
    for i in 0..400 {
        out.push_str(&format!("  res.append(\"header-{i}\", \"value-{i}\");\n"));
    }
    out.push_str("  return res;\n};\n");
    out
}

/// A large ordinary function declaration.
fn big_function() -> String {
    let mut out = String::from("export function processBatch(items: number[]): number {\n");
    for i in 0..300 {
        out.push_str(&format!("  items[{i}] = items[{i}] + 1;\n"));
    }
    out.push_str("  return items[0];\n}\n");
    out
}

/// A function whose second statement alone exceeds the ceiling.
fn oversized_statement() -> String {
    let mut out = String::from("export function ship(payload: string): string {\n");
    out.push_str("  const warmup = payload.length;\n");
    out.push_str("  const table = {\n");
    for i in 0..200 {
        out.push_str(&format!("    field{i}: \"data-{i}\",\n"));
    }
    out.push_str("  };\n");
    out.push_str("  return warmup + table.field0;\n");
    out.push_str("};\n");
    out
}

/// A large variable-bound class expression.
fn bound_class() -> String {
    let mut out = String::from("export const Widget = class Widget {\n");
    for i in 0..80 {
        out.push_str(&format!(
            "  method{i}(input: string): string {{\n    return \"m{i}\" + input;\n  }}\n"
        ));
    }
    out.push_str("};\n");
    out
}

/// A large variable-bound function expression.
fn function_expression() -> String {
    let mut out =
        String::from("export const heavy = function transform(input: string): string {\n");
    for i in 0..200 {
        out.push_str(&format!("  input = input + \"p{i}\";\n"));
    }
    out.push_str("  return input;\n};\n");
    out
}

/// A small single-chunk declaration with a nested named function inside an
/// arrow binding (the nested chunk must be emitted like for declarations).
fn nested_arrow() -> &'static str {
    "export const outer = (x: number) => {\n  function inner(x: number): number {\n    return x + 1;\n  }\n  return inner(x);\n};\n"
}

/// A small single-chunk variable-bound class with one method.
fn small_bound_class() -> &'static str {
    "export const Tiny = class Tiny {\n  run(x: number): number {\n    return x * 2;\n  }\n};\n"
}

/// A generic (non-function, non-class) declaration larger than the ceiling.
fn big_interface() -> String {
    let mut out = String::from("export interface HugeConfig {\n");
    for i in 0..300 {
        out.push_str(&format!("  field_{i}: string;\n"));
    }
    out.push_str("}\n");
    out
}

/// One line larger than the budget (irreducible), plus ordinary lines.
fn huge_single_line() -> String {
    let filler: String = "a".repeat(4200);
    format!("const data = \"{filler}\";\nconst other = 1;\n")
}

/// A multiline source with wide (multi-byte) unicode content, including a
/// long CJK line that would exceed the ceiling if bytes were counted.
fn unicode_source() -> String {
    let cjk: String = "漢".repeat(1400);
    let mut out = String::from("export function greeter(name: string): string {\n");
    out.push_str("  const note = \"こんにちは世界 🌍 — ünïcödé\";\n");
    out.push_str(&format!("  const big = \"{cjk}\";\n"));
    for i in 0..100 {
        out.push_str(&format!("  const row{i} = `行{i}: {{name}}`;\n"));
    }
    out.push_str("  return note + big;\n}\n");
    out
}

/// A decorated, commented class large enough to split; comments and
/// decorators must stay attached and within the bounds.
fn decorated_class() -> String {
    let mut out = String::from("export class Service {\n");
    for i in 0..100 {
        out.push_str(&format!("  /** Handles case {i}. */\n"));
        out.push_str("  @tracked\n");
        out.push_str(&format!(
            "  handle{i}(input: string): string {{\n    return \"{i}\" + input;\n  }}\n"
        ));
    }
    out.push_str("}\n");
    out
}

/// Repeated declaration names at different scopes.
fn repeated_names() -> &'static str {
    "function same(x: number): number {\n  function same(y: number): number {\n    return y;\n  }\n  return same(x);\n}\nexport function same(x: number): number {\n  return same(x - 1);\n}\n"
}

/// Malformed source: parser errors must not drop or unbound the file.
fn malformed_source() -> &'static str {
    "export function broken( {\n  const x = ;\n  return ;\n}\nconst ok = 1;\n"
}

fn parse_ts(source: &str) -> repoise_core::chunk::code::CodeParse {
    parse_code("ts", source)
}

/// Every unit must be within the ceiling unless irreducible (labeled), and
/// every unit's text must be the exact source lines for its range.
fn assert_bounds_and_exactness(source: &str, parsed: &repoise_core::chunk::code::CodeParse) {
    let lines: Vec<&str> = source.lines().collect();
    for unit in &parsed.units {
        if !unit.oversized {
            assert!(
                estimate_tokens(&unit.text) <= MAX_CHUNK_TOKENS,
                "unit {:?} (lines {}–{}) exceeds the ceiling without the irreducible label",
                unit.symbol,
                unit.line_start,
                unit.line_end,
            );
        } else {
            // Only a single irreducible line may carry the label.
            assert_eq!(
                unit.line_start, unit.line_end,
                "labeled exceptions must be single source lines"
            );
        }
        let expected = lines[(unit.line_start - 1) as usize..unit.line_end as usize].join("\n");
        assert_eq!(
            unit.text, expected,
            "unit text is not the exact source lines for its range"
        );
    }
}

/// Every content line must be covered by at least one unit. Blank lines and
/// pure boundary-punctuation lines (e.g. a lone closing brace) are
/// intentionally outside every code unit's range: ranges never cover
/// synthetic or brace-only content, and no source evidence is lost.
fn assert_full_coverage(source: &str, parsed: &repoise_core::chunk::code::CodeParse) {
    let lines: Vec<&str> = source.lines().collect();
    let mut covered = vec![false; lines.len()];
    for unit in &parsed.units {
        for was_covered in covered
            .iter_mut()
            .take(unit.line_end as usize)
            .skip((unit.line_start - 1) as usize)
        {
            *was_covered = true;
        }
    }
    let missing: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            let boundary_only = line
                .trim_end()
                .chars()
                .all(|c| matches!(c, '{' | '}' | '(' | ')' | ';' | ','));
            !covered[*index] && !(line.trim().is_empty() || boundary_only)
        })
        .map(|(index, _)| index + 1)
        .collect();
    assert!(
        missing.is_empty(),
        "content lines not covered by any chunk: {missing:?}"
    );
}

/// The split children of a large declaration keep symbol, heading and a
/// parent link with a non-empty context, and link within the split.
fn assert_split_children(parsed: &repoise_core::chunk::code::CodeParse, symbol: &str) {
    let units = &parsed.units;
    let hosting = units
        .iter()
        .find(|unit| unit.parent.is_none() && unit.symbol == symbol)
        .unwrap_or_else(|| panic!("no hosting unit for {symbol}"));
    let children: Vec<_> = units
        .iter()
        .filter(|unit| unit.parent.is_some() && unit.symbol == symbol)
        .collect();
    assert!(!children.is_empty(), "no split children for {symbol}");
    for child in &children {
        assert_eq!(child.heading_path, vec![symbol.to_string()]);
        assert!(
            child
                .context
                .as_ref()
                .is_some_and(|context| !context.is_empty()),
            "split child of {symbol} lacks context"
        );
        // Parent links stay inside the split (hosting or a chained part).
        let parent_ordinal = child.parent.expect("child has a parent");
        assert_ne!(parent_ordinal, child.ordinal);
        let parent_unit = units
            .iter()
            .find(|unit| unit.ordinal == parent_ordinal)
            .expect("parent ordinal exists");
        assert_eq!(parent_unit.symbol, symbol);
    }
    let _ = hosting;
}

#[test]
fn exported_arrow_function_splits_within_ceiling() {
    let source = route_file();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(
        parsed.units.iter().all(|unit| !unit.oversized),
        "no single line of the routes fixture is irreducible"
    );
    let symbol = parsed
        .symbols
        .iter()
        .find(|s| s.name == "chatRoutes")
        .expect("chatRoutes symbol");
    assert_eq!(symbol.kind, repoise_core::chunk::code::SymbolKind::Function);
    assert!(symbol.exported);
    assert_split_children(&parsed, "chatRoutes");
    // Deterministic: a second parse is identical.
    assert_eq!(parsed.units, parse_ts(&source).units);
}

#[test]
fn ordinary_function_splits_within_ceiling() {
    let source = big_function();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(parsed.units.iter().all(|unit| !unit.oversized));
    assert_split_children(&parsed, "processBatch");
}

#[test]
fn single_oversized_statement_splits_by_lines() {
    let source = oversized_statement();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(parsed.units.iter().all(|unit| !unit.oversized));
    assert_split_children(&parsed, "ship");
}

#[test]
fn variable_bound_class_splits_within_ceiling() {
    let source = bound_class();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(parsed.units.iter().all(|unit| !unit.oversized));
    let symbol = parsed
        .symbols
        .iter()
        .find(|s| s.name == "Widget")
        .expect("Widget symbol");
    assert_eq!(symbol.kind, repoise_core::chunk::code::SymbolKind::Class);
    assert!(symbol.exported);
    assert_split_children(&parsed, "Widget");
}

#[test]
fn function_expression_splits_within_ceiling() {
    let source = function_expression();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(parsed.units.iter().all(|unit| !unit.oversized));
    assert_split_children(&parsed, "heavy");
}

#[test]
fn generic_declaration_splits_within_ceiling() {
    let source = big_interface();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(parsed.units.iter().all(|unit| !unit.oversized));
    assert!(
        parsed.symbols.iter().any(|s| s.name == "HugeConfig"
            && s.kind == repoise_core::chunk::code::SymbolKind::Interface)
    );
    assert!(parsed.units.iter().any(|u| u.parent.is_some()));
}

#[test]
fn nested_function_chunks_emit_inside_arrow_bindings() {
    for (source, outer, inner) in [
        (nested_arrow(), "outer", "inner"),
        (
            "function outer(x: number): number {\n  function inner(x: number): number {\n    return x + 1;\n  }\n  return inner(x);\n}\n",
            "outer",
            "inner",
        ),
    ] {
        let parsed = parse_ts(source);
        assert_bounds_and_exactness(source, &parsed);
        let inner_symbol = parsed
            .symbols
            .iter()
            .find(|s| s.name == inner)
            .unwrap_or_else(|| panic!("no {inner} symbol"));
        assert!(
            inner_symbol.parent.is_some(),
            "nested symbol lacks parent link"
        );
        let inner_chunk = parsed
            .units
            .iter()
            .find(|u| u.symbol == inner)
            .unwrap_or_else(|| panic!("no {inner} chunk"));
        assert_eq!(
            inner_chunk.heading_path,
            vec![outer.to_string(), inner.to_string()]
        );
    }
}

#[test]
fn single_chunk_bound_class_emits_method_chunks() {
    let parsed = parse_ts(small_bound_class());
    assert_bounds_and_exactness(small_bound_class(), &parsed);
    let method = parsed
        .symbols
        .iter()
        .find(|s| s.name == "run" && s.kind == repoise_core::chunk::code::SymbolKind::Method)
        .expect("method symbol");
    assert!(method.parent.is_some());
    let chunk = parsed
        .units
        .iter()
        .find(|u| u.symbol == "run")
        .expect("method chunk");
    assert_eq!(
        chunk.heading_path,
        vec!["Tiny".to_string(), "run".to_string()]
    );
}

#[test]
fn huge_single_line_is_labeled_exception_with_exact_range() {
    let source = huge_single_line();
    let parsed = parse_ts(&source);
    assert_full_coverage(&source, &parsed);
    let lines: Vec<&str> = source.lines().collect();
    let oversized: Vec<_> = parsed.units.iter().filter(|u| u.oversized).collect();
    assert_eq!(oversized.len(), 1, "exactly one irreducible unit");
    let unit = &oversized[0];
    assert_eq!(unit.line_start, 1);
    assert_eq!(unit.line_end, 1);
    assert_eq!(
        unit.text, lines[0],
        "irreducible chunk keeps the exact line"
    );
    assert!(estimate_tokens(&unit.text) > MAX_CHUNK_TOKENS);
    // The remaining line is an ordinary bounded chunk.
    let other = parsed
        .units
        .iter()
        .find(|u| u.line_start == 2)
        .expect("second line covered");
    assert!(!other.oversized);
    assert!(estimate_tokens(&other.text) <= MAX_CHUNK_TOKENS);
}

#[test]
fn unicode_is_estimated_by_chars_and_ranges_stay_exact() {
    let source = unicode_source();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    // The 1400-character CJK line is 4200 bytes: a byte-based estimate would
    // flag it oversized, the character-based estimate must not.
    assert!(
        parsed.units.iter().all(|unit| !unit.oversized),
        "unicode source must not produce labeled exceptions"
    );
    assert!(
        parsed
            .units
            .iter()
            .any(|unit| unit.text.contains("こんにちは世界 🌍 — ünïcödé"))
    );
}

#[test]
fn decorators_and_comments_stay_attached_and_bounded() {
    let source = decorated_class();
    let parsed = parse_ts(&source);
    assert_bounds_and_exactness(&source, &parsed);
    assert_full_coverage(&source, &parsed);
    assert!(parsed.units.iter().all(|unit| !unit.oversized));
    assert!(parsed.units.iter().any(|u| u.text.contains("@tracked")));
}

#[test]
fn repeated_names_emit_each_declaration() {
    let parsed = parse_ts(repeated_names());
    assert_bounds_and_exactness(repeated_names(), &parsed);
    let same: Vec<_> = parsed.symbols.iter().filter(|s| s.name == "same").collect();
    assert_eq!(same.len(), 3, "module, nested and exported declarations");
    assert!(same.iter().any(|s| s.parent.is_none() && s.exported));
    assert!(same.iter().any(|s| s.parent.is_some()));
}

#[test]
fn malformed_syntax_falls_back_to_line_windows() {
    let parsed = parse_ts(malformed_source());
    assert_bounds_and_exactness(malformed_source(), &parsed);
    assert_full_coverage(malformed_source(), &parsed);
    assert!(
        !parsed.error_ranges.is_empty(),
        "parser errors are recorded"
    );
    assert!(!parsed.units.is_empty());
    assert!(parsed.units.iter().all(|u| !u.oversized));
}

/// A deterministic provider enforcing a documented per-input bound: every
/// input longer than `max_chars` characters is rejected.
struct BoundedProvider {
    max_chars: usize,
    model: String,
    dimension: u32,
}

impl EmbeddingProvider for BoundedProvider {
    fn profile(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            provider: "test-bounded".to_string(),
            model: self.model.clone(),
            dimension: self.dimension,
            profile_version: PROFILE_VERSION,
            input_profile: repoise_core::embed::InputProfile::RAW,
        }
    }

    fn embed(&self, inputs: &[String]) -> repoise_core::Result<EmbeddingBatch> {
        for input in inputs {
            if input.chars().count() > self.max_chars {
                return Err(Error::Provider(format!(
                    "input exceeds the documented {}-character bound",
                    self.max_chars
                )));
            }
        }
        let vectors = inputs
            .iter()
            .map(|input| {
                let mut h: u64 = 0xcbf29ce484222325;
                for byte in input.as_bytes() {
                    h ^= u64::from(*byte);
                    h = h.wrapping_mul(0x100000001b3);
                }
                (0..self.dimension)
                    .map(|i| {
                        let mixed =
                            h ^ (u64::from(i).wrapping_mul(0x9E3779B97F4A7C15)).wrapping_add(1);
                        let scaled = (f64::from((mixed % 1000) as u32) / 1000.0) * 2.0 - 1.0;
                        scaled as f32
                    })
                    .collect::<Vec<f32>>()
            })
            .collect();
        Ok(EmbeddingBatch {
            vectors,
            model_revision: None,
        })
    }
}

/// With a fake provider enforcing a documented input bound, the
/// representative multiline routes file indexes completely: no chunk is
/// oversized for the provider, the file is neither excluded nor relabeled,
/// and every eligible chunk gets a vector.
#[test]
fn routes_file_indexes_without_exclusion_under_provider_bound() {
    const PROVIDER_BOUND_CHARS: usize = 4096; // documented test bound: > 4x the chunk ceiling
    let root = temp_dir("provider-bound");
    std::fs::write(
        root.join("repoise.config.json"),
        "{\"schemaVersion\": 1, \"embedding\": {\"scope\": \"all\"}}\n",
    )
    .unwrap();
    let files: Vec<(&str, String)> = vec![
        ("src/routes/chat.ts", route_file()),
        (
            "docs/guide.md",
            "# Guide\n\nThe route assembly uses a gizmo bearing.\n".to_string(),
        ),
    ];
    let mut tree = BTreeMap::new();
    for (path, content) in &files {
        tree.insert((*path).to_string(), content.as_bytes().to_vec());
    }
    let mut revisions = BTreeMap::new();
    revisions.insert("fake:v1".to_string(), tree);
    let adapter = FakeRevisionAdapter::new(&root, revisions);
    let effective = EffectiveConfig::resolve(&root, &CliOverrides::default()).unwrap();
    assert_eq!(
        effective.embedding_scope,
        repoise_core::embed::EmbeddingScope::All
    );
    let cache = CachePaths::resolve(&root, ".repoise", None).unwrap();
    let (repo_id, worktree_id, _) =
        search::scope_for_search(&adapter, SnapshotMode::PlainDirectory).unwrap();
    let store = Store::new(cache.db_path(&repo_id, &worktree_id));
    let session = EmbeddingClient::new(
        Box::new(BoundedProvider {
            max_chars: PROVIDER_BOUND_CHARS,
            model: "test-bound".to_string(),
            dimension: 8,
        }),
        EmbeddingBudgets::default(),
        CancellationToken::new(),
    );
    let outcome = indexing::index(
        &adapter,
        SnapshotMode::PlainDirectory,
        &effective,
        &store,
        &cache,
        &IndexRequest::default(),
        Some(&session),
        None,
        None,
    )
    .unwrap();
    assert_eq!(outcome.files_indexed, 2, "no file is excluded");
    let stats = outcome.embedding.as_ref().expect("embedding pass ran");
    assert_eq!(
        stats.failed, 0,
        "no chunk input exceeded the provider bound"
    );
    assert_eq!(stats.pending, 0);
    assert!(stats.embedded > 0);

    // The routes file stays in the code corpus with bounded, complete chunks.
    let conn = store.open().unwrap();
    let generation = store::load_generation(&conn, &repo_id, &worktree_id)
        .unwrap()
        .unwrap();
    let routes = generation
        .files
        .iter()
        .find(|f| f.path == "src/routes/chat.ts")
        .expect("routes file is indexed");
    assert_eq!(routes.corpus.as_deref(), Some("code"));
    let profile = repoise_core::embed::EmbeddingProfile {
        provider: "test-bounded".to_string(),
        model: "test-bound".to_string(),
        dimension: 8,
        profile_version: PROFILE_VERSION,
        input_profile: repoise_core::embed::InputProfile::RAW,
    };
    let (chunks, vectors) =
        store::vector_coverage(&conn, outcome.generation_id, Some(&profile.fingerprint())).unwrap();
    assert_eq!(
        chunks, vectors,
        "every eligible chunk has a vector (no silent exclusion)"
    );
    assert!(chunks > 0);
    // The split chunks are lexically searchable.
    let response = search::search(
        &adapter,
        SnapshotMode::PlainDirectory,
        &store,
        &SearchRequest {
            query: "chatRoutes".to_string(),
            mode: SearchMode::Lexical,
            ..SearchRequest::default()
        },
        None,
    )
    .unwrap();
    assert!(
        response
            .results
            .iter()
            .any(|hit| hit.path == "src/routes/chat.ts"),
        "a split routes chunk must stay reachable"
    );
}
