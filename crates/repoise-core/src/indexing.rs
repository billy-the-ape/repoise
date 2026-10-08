//! Incremental index building (master plan sections 5 and 7).
//!
//! Flow: resolve the snapshot, run the deterministic inventory, reconcile
//! against the previous published generation for the scope, reparse only
//! changed (or newly eligible) files, reuse unchanged chunk inputs with
//! updated location metadata, tombstone deleted/excluded records, and
//! publish one complete generation transactionally. A failed build leaves
//! the previous generation served and records its error for `status`.
//!
//! Affected paths are scheduling hints only: every build reconciles the full
//! corpus and validates manifest consistency before publication.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::Result;
use crate::adapter::SnapshotMode;
use crate::cache::{CachePaths, worktree_id};
use crate::chunk::code;
use crate::chunk::config_fmt::ConfigChunker;
use crate::chunk::markdown::MarkdownChunker;
use crate::chunk::text::{TextChunker, TextFlavor};
use crate::chunk::{
    Chunker, Corpus, PARSER_VERSION_CODE_LINE, PARSER_VERSION_CONFIG, PARSER_VERSION_JS,
    PARSER_VERSION_MARKDOWN, PARSER_VERSION_TEXT, PARSER_VERSION_TS,
};
use crate::classify::Role;
use crate::config::EffectiveConfig;
use crate::discovery;
use crate::error::Error;
use crate::hash;
use crate::provenance::SnapshotRecord;
use crate::store::{self, ChunkRow, FileRow, GenerationInput, Store};

/// Request inputs for one index build.
#[derive(Clone, Debug, Default)]
pub struct IndexRequest {
    /// Scheduling hints (affected paths); they never limit reconciliation.
    pub affected_paths: Option<Vec<PathBuf>>,
    /// Force a full reparse even when file hashes are unchanged.
    pub force_full: bool,
}

/// Outcome of one successful publication.
#[derive(Clone, Debug, Serialize)]
pub struct IndexOutcome {
    /// Stable repository scope id.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Snapshot mode.
    pub snapshot_mode: String,
    /// Snapshot-level provenance.
    pub snapshot: SnapshotRecord,
    /// Published generation id.
    pub generation_id: i64,
    /// Content manifest hash of the inventory.
    pub manifest_hash: String,
    /// Total files in the new generation.
    pub files_indexed: usize,
    /// Files carried over unchanged (no reparse).
    pub files_reused: usize,
    /// Files reparsed (new, changed, or forced).
    pub files_reparsed: usize,
    /// Total chunks in the new generation.
    pub chunks_total: usize,
    /// Chunks whose input was unchanged (location metadata refreshed).
    pub chunks_reused: usize,
    /// Chunks newly added.
    pub chunks_added: usize,
    /// Chunks tombstoned (deleted/changed/excluded).
    pub chunks_removed: usize,
    /// Symbols published in the new generation (code corpus only).
    pub symbols_total: usize,
    /// Reference edges published in the new generation (code corpus only).
    pub references_total: usize,
    /// Inventory skip count (diagnostics, never content).
    pub files_skipped: usize,
    /// Vector records published in the new generation (0 when lexical-only).
    pub vectors_total: usize,
    /// Embedding session stats, when embedding ran for this build.
    pub embedding: Option<crate::embed::EmbeddingStats>,
    /// History lane summary (card K5; `None` when the lane is disabled).
    pub history: Option<crate::history::HistorySummary>,
}

/// The versioned `state.json` integration manifest (non-canonical; the
/// database remains canonical).
#[derive(Clone, Debug, Serialize)]
pub struct StateFile {
    /// Manifest format version.
    pub version: u64,
    /// Stable repository scope id.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Snapshot mode.
    pub snapshot_mode: String,
    /// Current generation id.
    pub generation_id: i64,
    /// Snapshot id of the current generation.
    pub snapshot_id: String,
    /// Content manifest hash.
    pub manifest_hash: String,
    /// Effective config fingerprint.
    pub config_fingerprint: String,
    /// Parser fingerprint.
    pub parser_fingerprint: String,
    /// Build time in milliseconds since the Unix epoch.
    pub built_at_ms: i64,
    /// Last build error, if the most recent build failed.
    pub last_error: Option<String>,
}

/// The combined parser fingerprint for the chunkers and code grammars
/// shipped with this build.
pub fn parser_fingerprint() -> String {
    let capabilities = code::grammar_capabilities();
    format!(
        "{}|{}|{}|{}|{}|{}",
        PARSER_VERSION_MARKDOWN,
        PARSER_VERSION_TEXT,
        PARSER_VERSION_CONFIG,
        PARSER_VERSION_CODE_LINE,
        crate::INDEX_SCHEMA_VERSION,
        capabilities.join("|")
    )
}

/// The shipped grammar for a detected code language: parser version and
/// whether the TSX superset grammar is used. `None` when the file falls back
/// to line windows (see [`code::parse_code`]).
pub fn code_grammar_for(language: &str) -> Option<(&'static str, bool)> {
    match language {
        "ts" => Some((PARSER_VERSION_TS, false)),
        "tsx" => Some((PARSER_VERSION_TS, true)),
        "js" | "jsx" | "mjs" | "cjs" => Some((PARSER_VERSION_JS, false)),
        _ => None,
    }
}

/// The parser version recorded for a code file (grammar or line fallback).
pub fn code_parser_version(language: &str) -> &'static str {
    match code_grammar_for(language) {
        Some((version, _)) => version,
        None => PARSER_VERSION_CODE_LINE,
    }
}

/// Selects the chunker for a detected language: corpus, parser version and
/// chunker. `None` for code/unknown languages (no chunks in this PR; code
/// symbols arrive in PR 4).
pub fn chunker_for(language: &str, role: Role) -> Option<(Corpus, &'static str, Box<dyn Chunker>)> {
    let corpus = match role {
        Role::Config => Corpus::Config,
        Role::Code => return None,
        _ => Corpus::Docs,
    };
    match language {
        "markdown" => Some((corpus, PARSER_VERSION_MARKDOWN, Box::new(MarkdownChunker))),
        "restructuredtext" => Some((
            corpus,
            PARSER_VERSION_TEXT,
            Box::new(TextChunker {
                flavor: TextFlavor::Rst,
            }),
        )),
        "asciidoc" => Some((
            corpus,
            PARSER_VERSION_TEXT,
            Box::new(TextChunker {
                flavor: TextFlavor::Adoc,
            }),
        )),
        "text" => Some((
            corpus,
            PARSER_VERSION_TEXT,
            Box::new(TextChunker {
                flavor: TextFlavor::Plain,
            }),
        )),
        "json" | "yaml" | "yml" => Some((
            corpus,
            PARSER_VERSION_CONFIG,
            Box::new(ConfigChunker { sectioned: false }),
        )),
        "toml" | "ini" | "conf" => Some((
            corpus,
            PARSER_VERSION_CONFIG,
            Box::new(ConfigChunker { sectioned: true }),
        )),
        "proto" => Some((
            corpus,
            PARSER_VERSION_CONFIG,
            Box::new(ConfigChunker { sectioned: false }),
        )),
        _ => None,
    }
}
/// Derives the (repo_id, worktree_id) scope for an inventory.
pub fn scope_for(
    inventory: &discovery::Inventory,
    canonical_root: &std::path::Path,
    mode: SnapshotMode,
) -> (String, String) {
    let repo_id = inventory.repository.repo_id.clone();
    (repo_id.clone(), worktree_id(&repo_id, canonical_root, mode))
}

/// Inclusive byte range of 1-based lines `[line_start, line_end]` in the
/// original snapshot bytes (CRLF-safe; synthetic label lines are never
/// covered because ranges come from source line numbers only).
pub fn line_byte_range(bytes: &[u8], line_start: u32, line_end: u32) -> (u64, u64) {
    let mut offsets = vec![0u64];
    for line in bytes.split(|&b| b == b'\n') {
        offsets.push(offsets.last().unwrap() + line.len() as u64 + 1);
    }
    let total = bytes.len() as u64;
    let start = offsets
        .get(line_start as usize - 1)
        .copied()
        .unwrap_or(total)
        .min(total);
    let end_exclusive = offsets
        .get(line_end as usize)
        .copied()
        .unwrap_or(total)
        .min(total);
    let end = if end_exclusive > start {
        end_exclusive - 1
    } else {
        start
    };
    (start, end)
}

/// Opaque chunk id: scope + parser version + relative path + structural
/// address + chunk text hash. Position counters alone never identify a
/// chunk, so line moves that preserve the chunk input keep the identity.
pub fn chunk_id(
    scope_key: &str,
    parser_version: &str,
    path: &str,
    address: &str,
    text: &str,
) -> String {
    let text_hash = hash::sha256_hex(text);
    let key = format!("{scope_key}\n{parser_version}\n{path}\n{address}\n{text_hash}");
    format!("chunk-{}", hash::sha256_hex(&key))
}

/// The structural address for a chunk ordinal (position among siblings, with
/// the parent ordinal when it is a labeled split).
pub fn chunk_address(ordinal: u32, parent: Option<u32>) -> String {
    match parent {
        Some(parent) => format!("{parent}/{ordinal}"),
        None => ordinal.to_string(),
    }
}

/// Serializes an enum to its canonical JSON name (kebab-case/lowercase).
fn enum_name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .expect("serializable")
        .as_str()
        .expect("string")
        .to_string()
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Builds and atomically publishes one index generation (cards K1, K3, K4
/// and K5). File-level reuse keeps unchanged records stable; embeddings are
/// optional (lexical-only always publishes); history is collected locally
/// and optionally enriched against the configured host before publication.
#[allow(clippy::too_many_arguments)]
pub fn index(
    adapter: &dyn crate::adapter::SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
    store: &Store,
    cache: &CachePaths,
    request: &IndexRequest,
    embed_session: Option<&crate::embed::EmbeddingClient>,
    enrichment: Option<&crate::history::EnrichmentSession>,
) -> Result<IndexOutcome> {
    let inventory = discovery::inventory(adapter, mode, eff)?;
    let canonical_root = adapter.canonical_root()?;
    let (repo_id, worktree_id) = scope_for(&inventory, &canonical_root, mode);
    let scope_key = format!("{repo_id}|{worktree_id}");
    let config_fingerprint = eff.fingerprint();
    let built_at_ms = now_ms();

    let conn = store.open()?;
    let prev = store::load_generation(&conn, &repo_id, &worktree_id)?;
    let force_full = request.force_full
        || prev.is_none()
        || prev
            .as_ref()
            .is_some_and(|p| p.meta.config_fingerprint != config_fingerprint)
        || prev
            .as_ref()
            .is_some_and(|p| p.meta.parser_fingerprint != parser_fingerprint());

    let prev_files: HashMap<String, FileRow> = prev
        .as_ref()
        .map(|p| {
            p.files
                .iter()
                .map(|f| (f.path.clone(), f.clone()))
                .collect()
        })
        .unwrap_or_default();
    let prev_chunks: HashMap<String, Vec<store::StoredChunk>> = prev
        .as_ref()
        .map(|p| {
            let mut map: HashMap<String, Vec<store::StoredChunk>> = HashMap::new();
            for chunk in &p.chunks {
                map.entry(chunk.path.clone())
                    .or_default()
                    .push(chunk.clone());
            }
            map
        })
        .unwrap_or_default();
    let prev_symbols: HashMap<String, Vec<store::SymbolRow>> = prev
        .as_ref()
        .map(|p| {
            let mut map: HashMap<String, Vec<store::SymbolRow>> = HashMap::new();
            for symbol in &p.symbols {
                map.entry(symbol.path.clone())
                    .or_default()
                    .push(symbol.clone());
            }
            map
        })
        .unwrap_or_default();
    let prev_references: HashMap<String, Vec<store::ReferenceRow>> = prev
        .as_ref()
        .map(|p| {
            let mut map: HashMap<String, Vec<store::ReferenceRow>> = HashMap::new();
            for reference in &p.references {
                map.entry(reference.path.clone())
                    .or_default()
                    .push(reference.clone());
            }
            map
        })
        .unwrap_or_default();
    let sizes: HashMap<String, u64> = inventory
        .manifest
        .entries
        .iter()
        .map(|entry| (crate::adapter::to_posix(&entry.path), entry.size))
        .collect();

    let mut file_rows: Vec<FileRow> = Vec::new();
    let mut chunk_rows: Vec<ChunkRow> = Vec::new();
    let mut files_reused = 0usize;
    let mut files_reparsed = 0usize;
    let mut chunks_reused = 0usize;
    let mut chunks_added = 0usize;
    let mut chunks_removed = 0usize;

    // Code files (card K4): structural grammar chunks plus symbols and
    // syntactic reference edges. Unchanged files carry their previous
    // symbols/edges forward; changed files are reparsed and resolved
    // against the current file set.
    let mut code_files: Vec<FileCodeData> = Vec::new();
    let mut symbol_rows: Vec<store::SymbolRow> = Vec::new();
    let mut reference_rows: Vec<store::ReferenceRow> = Vec::new();

    for file in &inventory.files {
        let posix_path = crate::adapter::to_posix(&file.path);
        let is_code = file.role == Role::Code;
        let selection = chunker_for(&file.language, file.role);
        let code_grammar = (is_code)
            .then_some(code_grammar_for(&file.language))
            .flatten();
        let corpus = selection
            .as_ref()
            .map(|(corpus, _, _)| *corpus)
            .or(is_code.then_some(Corpus::Code));
        let parser_version = selection
            .as_ref()
            .map(|(_, version, _)| version.to_string())
            .or_else(|| is_code.then(|| code_parser_version(&file.language).to_string()));
        let prev_file = prev_files.get(&posix_path);
        file_rows.push(FileRow {
            path: posix_path.clone(),
            content_hash: file.content_hash.clone(),
            role: file.role.name().to_string(),
            lifecycle: enum_name(file.lifecycle),
            classification_source: enum_name(file.classification_source),
            language: file.language.clone(),
            parser_version: parser_version.clone(),
            corpus: corpus.map(enum_name),
            parser_errors: prev_file.map(|p| p.parser_errors).unwrap_or(0),
            size: sizes.get(&posix_path).copied().unwrap_or(0),
        });
        let corpus = match corpus {
            Some(corpus) => corpus,
            None => {
                // Not chunked in this build; tombstone previous records.
                chunks_removed += prev_chunks.get(&posix_path).map(Vec::len).unwrap_or(0);
                continue;
            }
        };
        let parser_version = parser_version.expect("corpus selection has version");
        let unchanged = !force_full
            && prev_files.get(&posix_path).is_some_and(|p| {
                p.content_hash == file.content_hash
                    && p.parser_version.as_deref() == Some(parser_version.as_str())
            });
        if unchanged {
            // Unchanged chunk inputs are not reprocessed; location metadata
            // (ranges) and symbol/edge records are carried over exactly.
            files_reused += 1;
            for chunk in prev_chunks.get(&posix_path).cloned().unwrap_or_default() {
                chunks_reused += 1;
                chunk_rows.push(ChunkRow {
                    chunk_id: chunk.chunk_id,
                    parent_chunk_id: chunk.parent_chunk_id,
                    path: chunk.path,
                    heading_path: chunk.heading_path,
                    corpus: chunk.corpus,
                    text: chunk.text,
                    text_hash: chunk.text_hash,
                    symbol: chunk.symbol,
                    context: chunk.context,
                    line_start: chunk.line_start,
                    line_end: chunk.line_end,
                    byte_start: chunk.byte_start,
                    byte_end: chunk.byte_end,
                });
            }
            symbol_rows.extend(prev_symbols.get(&posix_path).cloned().unwrap_or_default());
            reference_rows.extend(
                prev_references
                    .get(&posix_path)
                    .cloned()
                    .unwrap_or_default(),
            );
        } else if is_code {
            let grammar_version = code_grammar
                .map(|(version, _)| version)
                .unwrap_or(PARSER_VERSION_CODE_LINE);
            reparse_code_file(
                &CodeFileScope {
                    key: &scope_key,
                    path: &posix_path,
                    parser_version: grammar_version,
                },
                &file.language,
                &Snapshot { adapter, mode },
                file,
                prev_chunks.get(&posix_path),
                &mut ReparseOutput {
                    chunk_rows: &mut chunk_rows,
                    files_reparsed: &mut files_reparsed,
                    chunks_reused: &mut chunks_reused,
                    chunks_added: &mut chunks_added,
                    chunks_removed: &mut chunks_removed,
                },
                &mut code_files,
            )?;
        } else {
            reparse_file(
                &ChunkScope {
                    key: &scope_key,
                    path: &posix_path,
                    parser_version: &parser_version,
                    corpus,
                },
                selection
                    .expect("chunker for non-code chunkable file")
                    .2
                    .as_ref(),
                &Snapshot { adapter, mode },
                file,
                prev_chunks.get(&posix_path),
                &mut ReparseOutput {
                    chunk_rows: &mut chunk_rows,
                    files_reparsed: &mut files_reparsed,
                    chunks_reused: &mut chunks_reused,
                    chunks_added: &mut chunks_added,
                    chunks_removed: &mut chunks_removed,
                },
            )?;
        }
    }
    // Files that existed before but are no longer eligible: tombstone chunks.
    for (path, chunks) in &prev_chunks {
        if !file_rows.iter().any(|f| &f.path == path) {
            chunks_removed += chunks.len();
        }
    }

    // Resolve code reference edges against this generation's file set and
    // symbol set (card K4); unresolved edges stay stored as uncertain.
    resolve_code_edges(
        &scope_key,
        &file_rows,
        &chunk_rows,
        &code_files,
        &mut symbol_rows,
        &mut reference_rows,
    );

    // Optional embedding pass: cache-first, bounded provider calls. Vectors
    // publish only with this generation (same transaction), so failed or
    // pending chunks never serve a vector and superseded vectors are
    // discarded. Unchanged chunk inputs are served from the content-
    // addressed cache without re-embedding. The embedding scope decides
    // which corpora are embedded (card K4).
    let embedding_scope = eff.embedding_scope;
    let mut vectors: Vec<store::ChunkVecRow> = Vec::new();
    let mut vector_profile: Option<String> = None;
    let mut embedding_stats: Option<crate::embed::EmbeddingStats> = None;
    if let Some(client) = embed_session {
        let profile = client.profile();
        let fingerprint = profile.fingerprint();
        let embedding_cache =
            crate::embed::EmbeddingCache::open(cache.embedding_cache_path(&repo_id))?;
        let work: Vec<crate::embed::EmbeddingWork> = chunk_rows
            .iter()
            .filter(|chunk| {
                chunk.corpus == "docs"
                    || (embedding_scope.includes_code() && chunk.corpus == "code")
            })
            .map(|chunk| crate::embed::EmbeddingWork {
                chunk_id: chunk.chunk_id.clone(),
                input: chunk.text.clone(),
                input_hash: chunk.text_hash.clone(),
            })
            .collect();
        let outcome = crate::embed::embed_chunks(&embedding_cache, client, &work, built_at_ms)?;
        embedding_stats = Some(outcome.stats);
        vector_profile = Some(fingerprint.clone());
        let text_hash_by_id: std::collections::HashMap<&str, &str> = chunk_rows
            .iter()
            .map(|chunk| (chunk.chunk_id.as_str(), chunk.text_hash.as_str()))
            .collect();
        for (chunk_id, vector) in outcome.vectors {
            let input_hash = text_hash_by_id
                .get(chunk_id.as_str())
                .expect("vector belongs to this generation's chunk")
                .to_string();
            vectors.push(store::ChunkVecRow {
                chunk_id,
                fingerprint: fingerprint.clone(),
                input_hash,
                dimension: profile.dimension,
                vector,
            });
        }
    }

    // Optional history lane (card K5): opt-in, bounded, offline local
    // collection; verified host enrichment only when a session is provided.
    // Enrichment failures never fail the local build (fail closed on denial).
    let mut history_rows: Vec<store::HistoryRow> = Vec::new();
    let mut history_summary: Option<crate::history::HistorySummary> = None;
    if eff.history.enabled {
        let spec = crate::history::HistorySpec {
            horizon: eff.history.horizon,
            max_paths_per_commit: eff.history.max_paths_per_commit,
            diff_hunks: eff.history.diff_hunks,
        };
        // History collection is adapter-backed: scopes without the capability
        // record an explicit Unavailable gap (the lane is empty, not failed).
        let mut set = match adapter.history_provider() {
            Some(_) => crate::history::collect_history(adapter, &spec)?,
            None => {
                let mut set = crate::history::HistorySet::default();
                set.gaps.push(crate::history::HistoryGap {
                    kind: crate::history::GapKind::Unavailable,
                    detail: "history collection is unavailable for this source scope; \
                            the history lane is empty"
                        .to_string(),
                });
                set
            }
        };
        let report = match enrichment {
            Some(session) => session.run(
                adapter.remote_identity().ok().flatten().as_deref(),
                &mut set,
            ),
            None => crate::history::EnrichmentReport::default(),
        };
        history_rows = set
            .items
            .iter()
            .map(|item| store::HistoryRow {
                item_id: crate::history::item_id(&scope_key, &item.revision_id),
                revision_id: item.revision_id.clone(),
                parents: item.parents.clone(),
                message: item.message.clone(),
                author: item.author.clone(),
                committed_at_ms: item.committed_at_ms,
                affected_paths: item.affected_paths.clone(),
                paths_truncated: item.paths_truncated,
                hunks: item.hunks.clone(),
                hunks_truncated: item.hunks_truncated,
                pr_hint: item.pr_hint.clone(),
                association: item.association.clone(),
            })
            .collect();
        history_summary = Some(crate::history::HistorySummary {
            count: set.count,
            enriched: report.enriched,
            no_association: report.no_association,
            failed: report.failed,
            gaps: set
                .gaps
                .iter()
                .map(|gap| format!("{:?}: {}", gap.kind, gap.detail))
                .collect(),
            rate_limited: report.rate_limited,
            permission_denied: report.permission_denied,
            budget_exhausted: report.budget_exhausted,
            unattempted: report.unattempted,
        });
    }

    let snapshot = inventory.snapshot.clone();
    let mode_name = format!("{mode:?}");
    // Persisted lane coverage: lets `status` and history search report gaps
    // and enrichment stops for the published generation.
    let history_meta = if eff.history.enabled {
        serde_json::to_string(&crate::history::HistoryLaneMeta {
            enabled: true,
            summary: history_summary.as_ref().cloned(),
        })
        .unwrap_or_default()
    } else {
        String::new()
    };
    let input = GenerationInput {
        repo_id: repo_id.clone(),
        worktree_id: worktree_id.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        snapshot_mode: mode_name.clone(),
        revision_id: snapshot
            .revision_id
            .as_ref()
            .map(|r| r.as_str().to_string()),
        manifest_hash: inventory.manifest.manifest_hash.clone(),
        config_fingerprint: config_fingerprint.clone(),
        parser_fingerprint: parser_fingerprint(),
        built_at_ms,
        files: file_rows.clone(),
        chunks: chunk_rows.clone(),
        vectors,
        vector_profile,
        embedding_scope,
        symbols: symbol_rows.clone(),
        references: reference_rows.clone(),
        history: history_rows.clone(),
        history_meta,
    };
    let generation_id = match store::publish(&conn, &input) {
        Ok(id) => {
            store::clear_last_error(&conn)?;
            id
        }
        Err(err) => {
            let _ = store::record_last_error(&conn, &err.to_string(), now_ms());
            return Err(err);
        }
    };
    // Reclaim shared embeddings no longer referenced by retained generations.
    // A GC problem never fails the build (lexical operation is unaffected).
    if embedding_stats.is_some() {
        let _ = crate::embed::gc_repo_embeddings(cache, &repo_id);
    }
    let state = StateFile {
        version: 1,
        repo_id: repo_id.clone(),
        worktree_id: worktree_id.clone(),
        snapshot_mode: mode_name,
        generation_id,
        snapshot_id: snapshot.snapshot_id.clone(),
        manifest_hash: snapshot.manifest_hash.clone(),
        config_fingerprint,
        parser_fingerprint: parser_fingerprint(),
        built_at_ms,
        last_error: None,
    };
    let state_path = cache.state_path(&repo_id, &worktree_id);
    write_state(&state_path, &state)?;
    Ok(IndexOutcome {
        repo_id,
        worktree_id,
        snapshot_mode: format!("{mode:?}"),
        snapshot,
        generation_id,
        manifest_hash: inventory.manifest.manifest_hash,
        files_indexed: file_rows.len(),
        files_reused,
        files_reparsed,
        chunks_total: chunk_rows.len(),
        chunks_reused,
        chunks_added,
        chunks_removed,
        symbols_total: symbol_rows.len(),
        references_total: reference_rows.len(),
        files_skipped: inventory.skips.len(),
        vectors_total: input.vectors.len(),
        embedding: embedding_stats,
        history: history_summary,
    })
}

/// Reparses one file, matching new chunks against the previous generation by
/// identity (unchanged inputs count as reused; the rest are added).
/// Chunk identity context: stable scope, path, parser version and corpus.
struct ChunkScope<'a> {
    /// Stable scope key.
    key: &'a str,
    /// Repository-relative POSIX path.
    path: &'a str,
    /// Parser version for this corpus.
    parser_version: &'a str,
    /// Logical corpus.
    corpus: Corpus,
}

/// Snapshot access for file reads.
struct Snapshot<'a> {
    /// Source adapter.
    adapter: &'a dyn crate::adapter::SourceAdapter,
    /// Snapshot mode.
    mode: SnapshotMode,
}

/// Mutable per-file reparse sink and counters.
struct ReparseOutput<'a> {
    /// Appended chunk rows.
    chunk_rows: &'a mut Vec<ChunkRow>,
    /// Files reparsed.
    files_reparsed: &'a mut usize,
    /// Chunks reused by identity.
    chunks_reused: &'a mut usize,
    /// Chunks newly added.
    chunks_added: &'a mut usize,
    /// Chunks replaced or removed.
    chunks_removed: &'a mut usize,
}

fn reparse_file(
    scope: &ChunkScope<'_>,
    chunker: &dyn Chunker,
    snapshot: &Snapshot<'_>,
    file: &crate::provenance::FileRecord,
    prev: Option<&Vec<store::StoredChunk>>,
    out: &mut ReparseOutput<'_>,
) -> Result<()> {
    *out.files_reparsed += 1;
    *out.chunks_removed += prev.map(Vec::len).unwrap_or(0);
    let bytes = read_snapshot_file(snapshot.adapter, snapshot.mode, file)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let chunks = chunker.parse(scope.path, &text);
    let corpus_name = enum_name(scope.corpus);
    let mut parent_ids: HashMap<u32, String> = HashMap::new();
    for chunk in chunks {
        let address = chunk_address(chunk.ordinal, chunk.parent);
        let id = chunk_id(
            scope.key,
            scope.parser_version,
            scope.path,
            &address,
            &chunk.text,
        );
        let parent_chunk_id = match chunk.parent {
            Some(parent) => parent_ids.get(&parent).cloned(),
            None => None,
        };
        if chunk.parent.is_none() {
            parent_ids.insert(chunk.ordinal, id.clone());
            if prev.is_some_and(|prev| prev.iter().any(|p| p.chunk_id == id)) {
                *out.chunks_reused += 1;
                *out.chunks_removed -= 1;
            } else {
                *out.chunks_added += 1;
            }
        }
        let (byte_start, byte_end) = line_byte_range(&bytes, chunk.line_start, chunk.line_end);
        // Defense-in-depth: never persist a known secret shape (discovery
        // already skips such files; this covers parser-produced text).
        let text = crate::ignore::redact_secret_content(&chunk.text);
        let text_hash = hash::sha256_hex(&text);
        out.chunk_rows.push(ChunkRow {
            chunk_id: id,
            parent_chunk_id,
            path: scope.path.to_string(),
            heading_path: chunk.heading_path.join(" > "),
            corpus: corpus_name.clone(),
            text,
            text_hash,
            symbol: String::new(),
            context: None,
            line_start: chunk.line_start,
            line_end: chunk.line_end,
            byte_start,
            byte_end,
        });
    }
    Ok(())
}

/// Reads one inventoried file at the snapshot (working-tree mode reads the
/// filesystem; committed mode reads the exact blob).
fn read_snapshot_file(
    adapter: &dyn crate::adapter::SourceAdapter,
    mode: SnapshotMode,
    file: &crate::provenance::FileRecord,
) -> Result<Vec<u8>> {
    let revision = adapter.resolve(None, mode)?;
    let bytes = adapter.read(&revision, mode, &file.path)?;
    if hash::sha256_hex(&bytes) != file.content_hash {
        return Err(Error::IndexState(format!(
            "file changed between inventory and parse: {}",
            crate::adapter::to_posix(&file.path)
        )));
    }
    Ok(bytes)
}

/// Resolves the reference edges of newly reparsed code files against the
/// current generation: import edges resolve to a file and target symbol via
/// the module specifier (certain when resolved, uncertain otherwise);
/// unqualified references resolve first to local symbols, then to imported
/// bindings (uncertain). Newly parsed symbols join the generation's symbol
/// set before resolution so edges may target them.
fn resolve_code_edges(
    scope_key: &str,
    file_rows: &[FileRow],
    chunk_rows: &[ChunkRow],
    code_files: &[FileCodeData],
    symbol_rows: &mut Vec<store::SymbolRow>,
    reference_rows: &mut Vec<store::ReferenceRow>,
) {
    symbol_rows.extend(code_files.iter().flat_map(|f| f.symbols.clone()));
    let all_paths: std::collections::HashSet<&str> =
        file_rows.iter().map(|file| file.path.as_str()).collect();
    let mut symbols_by_path: HashMap<&str, Vec<&store::SymbolRow>> = HashMap::new();
    for symbol in symbol_rows.iter() {
        symbols_by_path
            .entry(symbol.path.as_str())
            .or_default()
            .push(symbol);
    }
    for data in code_files {
        let local_symbols = symbols_by_path
            .get(data.path.as_str())
            .cloned()
            .unwrap_or_default();
        let mut import_bindings: HashMap<&str, Option<&store::SymbolRow>> = HashMap::new();
        // Import edges first: module resolution + target symbol lookup.
        for (ordinal, import) in data.imports.iter().enumerate() {
            let target_symbol =
                resolve_module(&data.path, &import.module, &all_paths).and_then(|target_path| {
                    symbols_by_path
                        .get(target_path.as_str())
                        .and_then(|symbols| {
                            symbols
                                .iter()
                                .copied()
                                .find(|symbol| symbol.name == import.local_name)
                        })
                });
            import_bindings.insert(&import.local_name, target_symbol);
            reference_rows.push(store::ReferenceRow {
                ref_id: ref_id(
                    scope_key,
                    &data.path,
                    enum_name(code::RefKind::Import).as_str(),
                    &import.local_name,
                    import.line,
                    ordinal as u32,
                ),
                path: data.path.clone(),
                name: import.local_name.clone(),
                kind: enum_name(code::RefKind::Import),
                confidence: if target_symbol.is_some() {
                    "certain"
                } else {
                    "uncertain"
                }
                .to_string(),
                line: import.line,
                chunk_id: chunk_for_line(chunk_rows, &data.path, import.line),
                target_symbol_id: target_symbol.map(|symbol| symbol.symbol_id.clone()),
            });
        }
        // Unqualified references: local symbol, then imported binding.
        for (ordinal, reference) in data.refs.iter().enumerate() {
            let local: Option<&store::SymbolRow> = local_symbols
                .iter()
                .find(|symbol| symbol.name == reference.name && symbol.line_start <= reference.line)
                .copied()
                .or_else(|| {
                    local_symbols
                        .iter()
                        .find(|symbol| symbol.name == reference.name)
                        .copied()
                });
            let target = match local {
                Some(symbol) => Some(symbol),
                None => import_bindings
                    .get(reference.name.as_str())
                    .copied()
                    .flatten(),
            };
            reference_rows.push(store::ReferenceRow {
                ref_id: ref_id(
                    scope_key,
                    &data.path,
                    enum_name(reference.kind).as_str(),
                    &reference.name,
                    reference.line,
                    ordinal as u32,
                ),
                path: data.path.clone(),
                name: reference.name.clone(),
                kind: enum_name(reference.kind),
                confidence: "uncertain".to_string(),
                line: reference.line,
                chunk_id: chunk_for_line(chunk_rows, &data.path, reference.line),
                target_symbol_id: target.map(|symbol| symbol.symbol_id.clone()),
            });
        }
    }
}

/// The chunk row (of one file) whose line range covers `line`.
fn chunk_for_line(chunk_rows: &[ChunkRow], path: &str, line: u32) -> Option<String> {
    chunk_rows
        .iter()
        .find(|chunk| chunk.path == path && chunk.line_start <= line && line <= chunk.line_end)
        .map(|chunk| chunk.chunk_id.clone())
}

/// Resolves a relative module specifier against the generation's file set
/// (extension and `index` candidates; first match wins).
fn resolve_module(
    importer: &str,
    module: &str,
    all_paths: &std::collections::HashSet<&str>,
) -> Option<String> {
    if !module.starts_with('.') {
        return None;
    }
    let mut segments: Vec<&str> = importer.split('/').collect();
    segments.pop();
    for segment in module.split('/') {
        match segment {
            "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let base = segments.join("/");
    let mut candidates: Vec<String> = Vec::new();
    for ext in [
        "", ".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs",
    ] {
        candidates.push(format!("{base}{ext}"));
    }
    for ext in [".ts", ".tsx", ".js", ".jsx"] {
        candidates.push(format!("{base}/index{ext}"));
    }
    candidates
        .into_iter()
        .find(|candidate| all_paths.contains(candidate.as_str()))
}

/// Writes the versioned `state.json` integration manifest (pretty JSON).
/// Chunking identity context for one code file.
struct CodeFileScope<'a> {
    /// Stable scope key.
    key: &'a str,
    /// Repository-relative POSIX path.
    path: &'a str,
    /// Grammar parser version (or the line-fallback version).
    parser_version: &'a str,
}

/// Raw parse data for one newly reparsed code file, awaiting edge
/// resolution against the current generation's file/symbol sets.
struct FileCodeData {
    /// Repository-relative POSIX path.
    path: String,
    /// Defined symbols (stable symbol ids assigned at parse time).
    symbols: Vec<store::SymbolRow>,
    /// Import edges (local binding name per import).
    imports: Vec<code::CodeImport>,
    /// Unqualified reference edges (calls/references/re-exports).
    refs: Vec<code::CodeRef>,
}

/// Opaque, stable symbol id for one declaration in a scope.
pub fn symbol_id(scope_key: &str, path: &str, name: &str, kind: &str, line_start: u32) -> String {
    let key = format!("{scope_key}\n{path}\n{kind}\n{name}\n{line_start}");
    format!("sym-{}", hash::sha256_hex(&key))
}

/// Opaque, stable reference edge id for one syntactic edge in a scope.
pub fn ref_id(
    scope_key: &str,
    path: &str,
    kind: &str,
    name: &str,
    line: u32,
    ordinal: u32,
) -> String {
    let key = format!("{scope_key}\n{path}\n{kind}\n{name}\n{line}\n{ordinal}");
    format!("ref-{}", hash::sha256_hex(&key))
}

/// The chunk id covering a source line (first matching unit in document
/// order), for symbols and references.
fn chunk_id_for_line(chunk_ids: &[String], units: &[code::CodeUnit], line: u32) -> Option<String> {
    units
        .iter()
        .position(|unit| unit.line_start <= line && line <= unit.line_end)
        .and_then(|index| chunk_ids.get(index).cloned())
}

/// Reparses one code file: grammar chunks (or line windows), symbols and
/// reference edges. Chunk rows are appended with their stable ids; the raw
/// edge data is deferred to generation-wide resolution.
fn reparse_code_file(
    scope: &CodeFileScope<'_>,
    language: &str,
    snapshot: &Snapshot<'_>,
    file: &crate::provenance::FileRecord,
    prev: Option<&Vec<store::StoredChunk>>,
    out: &mut ReparseOutput<'_>,
    code_files: &mut Vec<FileCodeData>,
) -> Result<()> {
    *out.files_reparsed += 1;
    *out.chunks_removed += prev.map(Vec::len).unwrap_or(0);
    let bytes = read_snapshot_file(snapshot.adapter, snapshot.mode, file)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parse = code::parse_code(language, &text);
    let corpus_name = enum_name(Corpus::Code);
    let mut unit_chunk_ids: Vec<String> = Vec::new();
    let mut parent_ids: HashMap<u32, String> = HashMap::new();
    for unit in &parse.units {
        let address = chunk_address(unit.ordinal, unit.parent);
        let id = chunk_id(
            scope.key,
            scope.parser_version,
            scope.path,
            &address,
            &unit.text,
        );
        let parent_chunk_id = match unit.parent {
            Some(parent) => parent_ids.get(&parent).cloned(),
            None => None,
        };
        if unit.parent.is_none() {
            parent_ids.insert(unit.ordinal, id.clone());
            if prev.is_some_and(|prev| prev.iter().any(|p| p.chunk_id == id)) {
                *out.chunks_reused += 1;
                *out.chunks_removed -= 1;
            } else {
                *out.chunks_added += 1;
            }
        }
        let (byte_start, byte_end) = line_byte_range(&bytes, unit.line_start, unit.line_end);
        let text = crate::ignore::redact_secret_content(&unit.text);
        let text_hash = hash::sha256_hex(&text);
        out.chunk_rows.push(ChunkRow {
            chunk_id: id.clone(),
            parent_chunk_id,
            path: scope.path.to_string(),
            heading_path: unit.heading_path.join(" > "),
            corpus: corpus_name.clone(),
            text,
            text_hash,
            symbol: unit.symbol.clone(),
            context: unit.context.clone(),
            line_start: unit.line_start,
            line_end: unit.line_end,
            byte_start,
            byte_end,
        });
        unit_chunk_ids.push(id);
    }
    let mut symbols: Vec<store::SymbolRow> = Vec::new();
    for symbol in &parse.symbols {
        let parent_symbol_id = symbol
            .parent
            .and_then(|parent| parse.symbols.get(parent))
            .map(|parent| {
                symbol_id(
                    scope.key,
                    scope.path,
                    &parent.name,
                    enum_name(parent.kind).as_str(),
                    parent.line_start,
                )
            });
        symbols.push(store::SymbolRow {
            symbol_id: symbol_id(
                scope.key,
                scope.path,
                &symbol.name,
                enum_name(symbol.kind).as_str(),
                symbol.line_start,
            ),
            path: scope.path.to_string(),
            name: symbol.name.clone(),
            kind: enum_name(symbol.kind),
            line_start: symbol.line_start,
            line_end: symbol.line_end,
            parent_symbol_id,
            exported: symbol.exported,
            chunk_id: chunk_id_for_line(&unit_chunk_ids, &parse.units, symbol.line_start),
        });
    }
    code_files.push(FileCodeData {
        path: scope.path.to_string(),
        symbols,
        imports: parse.imports.clone(),
        refs: parse.refs.clone(),
    });
    Ok(())
}

/// Writes the versioned `state.json` integration manifest (pretty JSON).
pub fn write_state(path: &std::path::Path, state: &StateFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}
