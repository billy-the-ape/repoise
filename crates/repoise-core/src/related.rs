//! Bounded "related knowledge" retrieval around one source chunk (card K6,
//! master plan section 8).
//!
//! Given one opaque source id from a search hit, `related` follows the
//! structural graph of the current published generation: syntactic reference
//! edges out of the chunk (`references`), reference edges into symbols the
//! chunk declares (`referenced-by`), and the labeled-split parent/child chunk
//! links. Results are deterministic, bounded by a limit and an output token
//! budget, and carry exact provenance so callers can follow up with `read`.
//! The graph is evidence only: no retrieved content changes tool grants or
//! becomes agent instructions.

use serde::{Deserialize, Serialize};

use rusqlite::OptionalExtension;

use crate::Result;
use crate::adapter::SnapshotMode;
use crate::error::Error;
use crate::search::{ScopeView, estimate_tokens, github_url, scope_for_search};
use crate::store::{self, Store};

/// Related-response schema version.
pub const RESPONSE_SCHEMA_VERSION: u32 = 1;
/// Default related-result limit.
pub const DEFAULT_LIMIT: u32 = 10;
/// Hard related-result cap.
pub const LIMIT_CAP: u32 = 20;
/// Default output token budget (estimated).
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 2500;

/// One relation kind the caller is willing to follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelationKind {
    /// Chunks this chunk references (outgoing syntactic reference edges).
    References,
    /// Chunks that reference symbols declared in this chunk.
    ReferencedBy,
    /// Labeled-split child chunks of this chunk.
    Children,
    /// The labeled-split parent chunk of this chunk, if any.
    Parent,
}

impl RelationKind {
    /// Parses a relation kind name; `None` when unknown.
    pub fn parse(value: &str) -> Option<RelationKind> {
        match value {
            "references" => Some(RelationKind::References),
            "referenced-by" => Some(RelationKind::ReferencedBy),
            "children" => Some(RelationKind::Children),
            "parent" => Some(RelationKind::Parent),
            _ => None,
        }
    }

    /// Canonical relation kind name.
    pub fn name(self) -> &'static str {
        match self {
            RelationKind::References => "references",
            RelationKind::ReferencedBy => "referenced-by",
            RelationKind::Children => "children",
            RelationKind::Parent => "parent",
        }
    }

    /// All relation kinds, in deterministic order.
    pub fn all() -> [RelationKind; 4] {
        [
            RelationKind::References,
            RelationKind::ReferencedBy,
            RelationKind::Children,
            RelationKind::Parent,
        ]
    }
}

/// One related-knowledge request.
#[derive(Clone, Debug)]
pub struct RelatedRequest {
    /// Opaque source id (chunk id) from a search hit or previous related result.
    pub source_id: String,
    /// Relation kinds to follow (all kinds when empty).
    pub kinds: Vec<RelationKind>,
    /// Result limit (default 10, cap 20).
    pub limit: Option<u32>,
    /// Output token budget (estimated; default 2,500).
    pub max_output_tokens: Option<u64>,
}

/// The source chunk the relation graph was followed from.
#[derive(Clone, Debug, Serialize)]
pub struct RelatedSource {
    /// Opaque source id (chunk id).
    pub source_id: String,
    /// Repository-relative path.
    pub path: String,
    /// Heading ancestry or file name.
    pub title: String,
    /// Primary declared symbol (empty for module/fallback chunks).
    pub symbol: String,
    /// Exact 1-based inclusive line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Validated permalink (GitHub remotes with Git revisions only).
    pub url: Option<String>,
}

/// One related chunk.
#[derive(Clone, Debug, Serialize)]
pub struct RelatedHit {
    /// Opaque source id for `read`.
    pub source_id: String,
    /// Relation kind that produced this result.
    pub relation: String,
    /// Edge kind when the relation is a reference edge (`import`, `call`, ...).
    pub edge_kind: Option<String>,
    /// Referenced name when the relation is a reference edge.
    pub name: Option<String>,
    /// Repository-relative path.
    pub path: String,
    /// Heading ancestry or file name.
    pub title: String,
    /// Exact 1-based inclusive line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Bounded excerpt of the chunk text.
    pub excerpt: String,
    /// Validated permalink (GitHub remotes with Git revisions only).
    pub url: Option<String>,
}

/// One bounded related-knowledge response (schema version 1).
#[derive(Clone, Debug, Serialize)]
pub struct RelatedResponse {
    /// Response schema version.
    pub schema_version: u32,
    /// Scope the relation query ran against.
    pub scope: ScopeView,
    /// Served generation id.
    pub generation_id: i64,
    /// The source chunk the graph was followed from.
    pub source: RelatedSource,
    /// Ranked related results (deterministic order).
    pub results: Vec<RelatedHit>,
    /// Whether the limit or token budget cut the result set short.
    pub truncated: bool,
    /// Estimated output tokens consumed by this response.
    pub output_tokens: u64,
    /// Token counts are estimates (no tokenizer configured).
    pub tokens_estimated: bool,
}

/// One chunk row fetched by id (helper for candidate lookup).
#[derive(Clone)]
struct CandidateChunk {
    chunk_id: String,
    path: String,
    heading: String,
    line_start: u32,
    line_end: u32,
    text: String,
}
/// Follows the structural graph from one source chunk, bounded and deterministic.
pub fn related(
    adapter: &dyn crate::adapter::SourceAdapter,
    mode: SnapshotMode,
    store: &Store,
    request: &RelatedRequest,
) -> Result<RelatedResponse> {
    let (repo_id, worktree_id, remote_identity) = scope_for_search(adapter, mode)?;
    let conn = store.open()?;
    let Some(meta) = store::current_generation(&conn, &repo_id, &worktree_id)? else {
        return Err(Error::IndexState(
            "no published index for this scope; run `repoise index`".into(),
        ));
    };
    let gen_id = meta.generation_id;

    // The source chunk (current generation only).
    let mut stmt = conn
        .prepare(
            "SELECT chunk_id, path, heading_path, symbol, line_start, line_end,              parent_chunk_id              FROM chunk WHERE generation_id = ?1 AND chunk_id = ?2",
        )
        .map_err(Error::Sqlite)?;
    let source_row = stmt
        .query_row(rusqlite::params![gen_id, request.source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)? as u32,
                r.get::<_, i64>(5)? as u32,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .optional()
        .map_err(Error::Sqlite)?
        .ok_or_else(|| {
            Error::IndexState(format!(
                "source id not in the current generation: {}",
                request.source_id
            ))
        })?;
    let (
        source_chunk_id,
        source_path,
        source_heading,
        source_symbol,
        source_line_start,
        source_line_end,
        source_parent,
    ) = source_row;
    let source_title = if source_heading.is_empty() {
        source_path.clone()
    } else {
        source_heading.clone()
    };

    let kinds: Vec<RelationKind> = if request.kinds.is_empty() {
        RelationKind::all().to_vec()
    } else {
        request.kinds.clone()
    };
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, LIMIT_CAP) as usize;
    let budget = request
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
        .max(1);

    // Candidate: target chunk row plus the relation that produced it.
    #[derive(Clone)]
    struct Candidate {
        relation: RelationKind,
        edge_kind: Option<String>,
        name: Option<String>,
        chunk_id: String,
        path: String,
        heading: String,
        line_start: u32,
        line_end: u32,
        text: String,
    }
    let mut candidates: Vec<Candidate> = Vec::new();

    let mut chunk_stmt = conn
        .prepare(
            "SELECT chunk_id, path, heading_path, line_start, line_end, text              FROM chunk WHERE generation_id = ?1 AND chunk_id = ?2",
        )
        .map_err(Error::Sqlite)?;
    let mut chunk_of = |chunk_id: &str| -> Result<Option<CandidateChunk>> {
        let found = chunk_stmt
            .query_row(rusqlite::params![gen_id, chunk_id], |r| {
                Ok(CandidateChunk {
                    chunk_id: chunk_id.to_string(),
                    path: r.get::<_, String>(1)?,
                    heading: r.get::<_, String>(2)?,
                    line_start: r.get::<_, i64>(3)? as u32,
                    line_end: r.get::<_, i64>(4)? as u32,
                    text: r.get::<_, String>(5)?,
                })
            })
            .optional()
            .map_err(Error::Sqlite)?;
        Ok(found)
    };
    for kind in &kinds {
        match kind {
            RelationKind::References => {
                // Outgoing edges from this chunk to resolved target symbols,
                // then to the chunk covering each target declaration.
                for (edge_kind, name, target_symbol_id, _) in
                    reference_edges(&conn, gen_id, &source_chunk_id)?
                {
                    if let Some(chunk_id) = symbol_chunk_id(&conn, gen_id, &target_symbol_id)?
                        && let Some(chunk) = chunk_of(&chunk_id)?
                        && chunk.chunk_id != source_chunk_id
                    {
                        candidates.push(Candidate {
                            relation: RelationKind::References,
                            edge_kind: Some(edge_kind),
                            name: Some(name),
                            chunk_id: chunk.chunk_id,
                            path: chunk.path,
                            heading: chunk.heading,
                            line_start: chunk.line_start,
                            line_end: chunk.line_end,
                            text: chunk.text,
                        });
                    }
                }
            }
            RelationKind::ReferencedBy => {
                // Inbound edges into symbols declared in this chunk.
                let symbol_ids = declared_symbol_ids(&conn, gen_id, &source_chunk_id)?;
                if symbol_ids.is_empty() {
                    continue;
                }
                let mut seen: Vec<String> = Vec::new();
                for symbol_id in &symbol_ids {
                    for (edge_kind, name, _, chunk_id) in
                        references_to_symbol(&conn, gen_id, symbol_id)?
                    {
                        let is_new = !seen.iter().any(|id| id == &chunk_id);
                        if chunk_id == source_chunk_id || !is_new {
                            continue;
                        }
                        if is_new {
                            seen.push(chunk_id.clone());
                        }
                        if let Some(chunk) = chunk_of(&chunk_id)? {
                            candidates.push(Candidate {
                                relation: RelationKind::ReferencedBy,
                                edge_kind: Some(edge_kind),
                                name: Some(name),
                                chunk_id: chunk.chunk_id,
                                path: chunk.path,
                                heading: chunk.heading,
                                line_start: chunk.line_start,
                                line_end: chunk.line_end,
                                text: chunk.text,
                            });
                        }
                    }
                }
            }
            RelationKind::Children => {
                let mut stmt = conn
                    .prepare(
                        "SELECT chunk_id, path, heading_path, line_start, line_end, text                          FROM chunk WHERE generation_id = ?1 AND parent_chunk_id = ?2                          ORDER BY line_start, chunk_id",
                    )
                    .map_err(Error::Sqlite)?;
                let rows = stmt
                    .query_map(rusqlite::params![gen_id, source_chunk_id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)? as u32,
                            r.get::<_, i64>(4)? as u32,
                            r.get::<_, String>(5)?,
                        ))
                    })
                    .map_err(Error::Sqlite)?;
                for row in rows {
                    let (chunk_id, path, heading, line_start, line_end, text) =
                        row.map_err(Error::Sqlite)?;
                    if chunk_id == source_chunk_id {
                        continue;
                    }
                    candidates.push(Candidate {
                        relation: RelationKind::Children,
                        edge_kind: None,
                        name: None,
                        chunk_id,
                        path,
                        heading,
                        line_start,
                        line_end,
                        text,
                    });
                }
            }
            RelationKind::Parent => {
                if let Some(parent_id) = &source_parent
                    && parent_id != &source_chunk_id
                    && let Some(chunk) = chunk_of(parent_id)?
                {
                    candidates.push(Candidate {
                        relation: RelationKind::Parent,
                        edge_kind: None,
                        name: None,
                        chunk_id: chunk.chunk_id,
                        path: chunk.path,
                        heading: chunk.heading,
                        line_start: chunk.line_start,
                        line_end: chunk.line_end,
                        text: chunk.text,
                    });
                }
            }
        }
    }

    // Deterministic order: requested kind order, then path, line, chunk id.
    let kind_rank: std::collections::HashMap<RelationKind, usize> = kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| (*kind, index))
        .collect();
    candidates.sort_by(|a, b| {
        (
            kind_rank[&a.relation],
            a.path.as_str(),
            a.line_start,
            a.chunk_id.as_str(),
        )
            .cmp(&(
                kind_rank[&b.relation],
                b.path.as_str(),
                b.line_start,
                b.chunk_id.as_str(),
            ))
    });
    // Deduplicate by target chunk id (first relation in order wins).
    let mut seen: Vec<String> = Vec::new();
    candidates.retain(|candidate| {
        let fresh = !seen.iter().any(|id| id == &candidate.chunk_id);
        if fresh {
            seen.push(candidate.chunk_id.clone());
        }
        fresh
    });

    let mut results: Vec<RelatedHit> = Vec::new();
    let mut output_tokens = estimate_tokens(&source_title)
        .saturating_add(estimate_tokens(&source_path))
        .saturating_add(16);
    let mut truncated = false;
    for candidate in &candidates {
        if results.len() >= limit {
            truncated = true;
            break;
        }
        let title = if candidate.heading.is_empty() {
            candidate.path.clone()
        } else {
            candidate.heading.clone()
        };
        let excerpt = crate::history::bounded_excerpt(&candidate.text);
        let hit_tokens = estimate_tokens(&title)
            .saturating_add(estimate_tokens(&candidate.path))
            .saturating_add(estimate_tokens(&excerpt))
            .saturating_add(16);
        if !results.is_empty() && output_tokens.saturating_add(hit_tokens) > budget {
            truncated = true;
            break;
        }
        output_tokens = output_tokens.saturating_add(hit_tokens);
        let url = github_url(
            &remote_identity,
            &meta.revision_id,
            &candidate.path,
            candidate.line_start,
            candidate.line_end,
        );
        results.push(RelatedHit {
            source_id: candidate.chunk_id.clone(),
            relation: candidate.relation.name().to_string(),
            edge_kind: candidate.edge_kind.clone(),
            name: candidate.name.clone(),
            path: candidate.path.clone(),
            title,
            line_start: candidate.line_start,
            line_end: candidate.line_end,
            excerpt,
            url,
        });
    }
    if candidates.len() > results.len() {
        truncated = true;
    }

    Ok(RelatedResponse {
        schema_version: RESPONSE_SCHEMA_VERSION,
        scope: ScopeView {
            repo_id,
            worktree_id,
            snapshot_mode: format!("{mode:?}"),
            snapshot_id: meta.snapshot_id,
            revision: meta.revision_id.clone(),
        },
        generation_id: gen_id,
        source: RelatedSource {
            source_id: source_chunk_id,
            path: source_path.clone(),
            title: source_title,
            symbol: source_symbol,
            line_start: source_line_start,
            line_end: source_line_end,
            url: github_url(
                &remote_identity,
                &meta.revision_id,
                &source_path,
                source_line_start,
                source_line_end,
            ),
        },
        results,
        truncated,
        output_tokens,
        tokens_estimated: true,
    })
}

/// Outgoing reference edges (from one chunk to resolved target symbols) as
/// (edge kind, referenced name, resolved target symbol id, source chunk id),
/// deterministically ordered.
fn reference_edges(
    conn: &rusqlite::Connection,
    gen_id: i64,
    chunk_id: &str,
) -> Result<Vec<(String, String, String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT kind, name, target_symbol_id, chunk_id FROM reference 
             WHERE generation_id = ?1 AND chunk_id = ?2 AND target_symbol_id IS NOT NULL 
             ORDER BY line, ref_id",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params![gen_id, chunk_id], edge_row)
        .map_err(Error::Sqlite)?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.map_err(Error::Sqlite)?);
    }
    Ok(edges)
}

/// Inbound reference edges (references resolving to one target symbol) as
/// (edge kind, referenced name, resolved target symbol id, source chunk id),
/// deterministically ordered.
fn references_to_symbol(
    conn: &rusqlite::Connection,
    gen_id: i64,
    symbol_id: &str,
) -> Result<Vec<(String, String, String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT kind, name, target_symbol_id, chunk_id FROM reference 
             WHERE generation_id = ?1 AND target_symbol_id = ?2 AND chunk_id IS NOT NULL 
             ORDER BY path, line, ref_id",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params![gen_id, symbol_id], edge_row)
        .map_err(Error::Sqlite)?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.map_err(Error::Sqlite)?);
    }
    Ok(edges)
}

/// One reference edge row (kind, name, target symbol id, source chunk id).
fn edge_row(row: &rusqlite::Row) -> rusqlite::Result<(String, String, String, String)> {
    Ok((
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
    ))
}

/// The chunk covering a symbol declaration, if recorded.
fn symbol_chunk_id(
    conn: &rusqlite::Connection,
    gen_id: i64,
    symbol_id: &str,
) -> Result<Option<String>> {
    let row = conn
        .query_row(
            "SELECT chunk_id FROM symbol WHERE generation_id = ?1 AND symbol_id = ?2",
            rusqlite::params![gen_id, symbol_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(Error::Sqlite)?;
    Ok(row.flatten())
}

/// Symbol ids declared in one chunk (via the symbol table's chunk link).
fn declared_symbol_ids(
    conn: &rusqlite::Connection,
    gen_id: i64,
    chunk_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT symbol_id FROM symbol WHERE generation_id = ?1 AND chunk_id = ?2 
             ORDER BY line_start, symbol_id",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params![gen_id, chunk_id], |r| {
            r.get::<_, String>(0)
        })
        .map_err(Error::Sqlite)?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.map_err(Error::Sqlite)?);
    }
    Ok(ids)
}
