//! Offline lexical search (master plan section 8, PR 2 scope).
//!
//! Search runs entirely against the current published generation of the
//! declared scope using SQLite FTS5. Ranking is BM25 plus explainable
//! boosts: exact path/symbol precedence for identifier queries and a modest
//! current-document preference (history/plan/decision roles win only for
//! history-oriented queries). Results carry exact provenance and, for GitHub
//! remotes with Git revisions, a validated permalink. No matches yield a
//! lexical/filesystem fallback suggestion. Cursored pagination binds the
//! cursor to the generation, query and filters, with an expiry.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::adapter::SnapshotMode;
use crate::cache::worktree_id;
use crate::classify::Role;
use crate::error::Error;
use crate::hash;
use crate::provenance::RepositoryRecord;
use crate::store::{self, Store};

/// One page of search results (schema version 1).
pub const RESPONSE_SCHEMA_VERSION: u32 = 1;
/// Default page size.
pub const DEFAULT_MAX_RESULTS: u32 = 5;
/// Hard page cap.
pub const MAX_RESULTS_CAP: u32 = 20;
/// Default output token budget (estimated).
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 1200;
/// Max hits kept per file (sibling/range repetition suppression).
const MAX_HITS_PER_FILE: usize = 3;
/// Candidate row cap before ranking/pagination.
const CANDIDATE_CAP: usize = 400;
/// Cursor time-to-live (milliseconds).
const CURSOR_TTL_MS: i64 = 60 * 60 * 1000;

/// Words that mark history/rationale-oriented questions.
const HISTORY_WORDS: &[&str] = &[
    "why",
    "history",
    "plan",
    "planned",
    "decision",
    "decisions",
    "rationale",
    "changelog",
    "deprecated",
    "superseded",
    "originally",
];

/// Search request. All fields optional except the query.
#[derive(Clone, Debug, Default)]
pub struct SearchRequest {
    /// Natural-language query.
    pub query: String,
    /// Glob filter on the repository-relative path.
    pub path_filter: Option<String>,
    /// Role filter.
    pub role_filter: Option<Role>,
    /// Page size (default 5, capped at 20).
    pub max_results: Option<u32>,
    /// Output token budget (estimated; default 1,200).
    pub max_output_tokens: Option<u64>,
    /// Opaque pagination cursor from a previous response.
    pub cursor: Option<String>,
}

/// One ranked result.
#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    /// Opaque source id (chunk id) for `read`.
    pub source_id: String,
    /// Repository-relative path.
    pub path: String,
    /// Content hash of the file at the snapshot.
    pub revision_hash: String,
    /// Classification role.
    pub role: String,
    /// Lifecycle.
    pub lifecycle: String,
    /// Exact 1-based inclusive line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Heading ancestry or file name.
    pub title: String,
    /// Compact excerpt around the first match.
    pub excerpt: String,
    /// Why this result ranks where it does.
    pub explanation: String,
    /// Validated permalink (GitHub remotes with Git revisions only).
    pub url: Option<String>,
}

/// Scope the search ran against.
#[derive(Clone, Debug, Serialize)]
pub struct ScopeView {
    /// Stable repository scope id.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Snapshot mode.
    pub snapshot_mode: String,
    /// Snapshot id of the served generation.
    pub snapshot_id: String,
    /// Adapter-qualified opaque revision.
    pub revision: Option<String>,
}

/// Honest corpus coverage for the served generation.
#[derive(Clone, Debug, Serialize)]
pub struct Coverage {
    /// File record count.
    pub files: i64,
    /// Chunk record count.
    pub chunks: i64,
    /// Lexical coverage description.
    pub lexical: String,
    /// Vector coverage description (none in this PR).
    pub vector: String,
}

/// One page of search results.
#[derive(Clone, Debug, Serialize)]
pub struct SearchResponse {
    /// Response schema version.
    pub schema_version: u32,
    /// Scope the search ran against.
    pub scope: ScopeView,
    /// Served generation id.
    pub generation_id: i64,
    /// Retrieval mode used.
    pub retrieval_mode: String,
    /// Corpus coverage of the served generation.
    pub coverage: Coverage,
    /// Whether more results exist beyond this page/budget.
    pub truncated: bool,
    /// Opaque cursor for the next page, if truncated.
    pub next_cursor: Option<String>,
    /// Estimated output tokens consumed by this page.
    pub output_tokens: u64,
    /// Token counts are estimates (no tokenizer configured).
    pub tokens_estimated: bool,
    /// Ranked results.
    pub results: Vec<SearchHit>,
    /// Fallback file suggestions when nothing matched.
    pub fallback: Option<Vec<String>>,
}
/// Opaque cursor payload (JSON, base64url-encoded).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cursor {
    /// Cursor format version.
    v: u8,
    /// Generation the cursor binds to.
    generation: i64,
    /// Hash of the query.
    qh: String,
    /// Hash of the filters.
    fh: String,
    /// Zero-based offset into the ranked candidate list.
    off: u32,
    /// Issuance time in milliseconds since the Unix epoch.
    ts: i64,
}

fn b64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = *chunk.get(1).unwrap_or(&0) as usize;
        let b2 = *chunk.get(2).unwrap_or(&0) as usize;
        out.push(ALPHABET[b0 >> 2] as char);
        out.push(ALPHABET[((b0 & 0b0000_0011) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((b1 & 0b0000_1111) << 2) | (b2 >> 6)] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[b2 & 0b0011_1111] as char);
        }
    }
    out
}

fn b64url_decode(input: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let value = |b: u8| -> u8 { ALPHABET.iter().position(|c| *c == b).unwrap_or(0) as u8 };
    let chars: Vec<u8> = input.as_bytes().to_vec();
    let mut bytes = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        let v0 = value(chars[i]);
        let v1 = value(chars[i + 1]);
        bytes.push((v0 << 2) | (v1 >> 4));
        if i + 2 < chars.len() {
            let v2 = value(chars[i + 2]);
            bytes.push(((v1 & 0b0000_1111) << 4) | (v2 >> 2));
            if i + 3 < chars.len() {
                let v3 = value(chars[i + 3]);
                bytes.push(((v2 & 0b0000_0011) << 6) | v3);
            }
        }
        i += 4;
    }
    bytes
}

/// Encodes a cursor bound to a generation, query and filters.
pub fn encode_cursor(
    generation: i64,
    query: &str,
    filters: &str,
    offset: u32,
    now_ms: i64,
) -> String {
    let cursor = Cursor {
        v: 1,
        generation,
        qh: hash::sha256_hex(query),
        fh: hash::sha256_hex(filters),
        off: offset,
        ts: now_ms,
    };
    let bytes = serde_json::to_vec(&cursor).expect("serializable");
    b64url_encode(&bytes)
}

/// Decodes and validates a cursor against the current request.
fn decode_cursor(
    cursor: &str,
    generation: i64,
    query: &str,
    filters: &str,
    now_ms: i64,
) -> Result<u32> {
    let decoded: Cursor = serde_json::from_slice(&b64url_decode(cursor))
        .map_err(|_| Error::IndexState("invalid cursor".into()))?;
    if decoded.v != 1 {
        return Err(Error::IndexState("invalid cursor version".into()));
    }
    if decoded.generation != generation {
        return Err(Error::IndexState(
            "cursor bound to a different generation".into(),
        ));
    }
    if decoded.qh != hash::sha256_hex(query) {
        return Err(Error::IndexState(
            "cursor bound to a different query".into(),
        ));
    }
    if decoded.fh != hash::sha256_hex(filters) {
        return Err(Error::IndexState(
            "cursor bound to different filters".into(),
        ));
    }
    if decoded.ts > now_ms || now_ms - decoded.ts > CURSOR_TTL_MS {
        return Err(Error::IndexState("cursor expired".into()));
    }
    Ok(decoded.off)
}
/// Derives the (repo_id, worktree_id) scope plus sanitized remote identity
/// for one adapter without running a full inventory.
pub fn scope_for_search(
    adapter: &dyn crate::adapter::SourceAdapter,
    mode: SnapshotMode,
) -> Result<(String, String, Option<String>)> {
    let canonical = adapter.canonical_root()?;
    let remote_identity = adapter.remote_identity()?;
    let record = RepositoryRecord::new(adapter.kind(), &canonical, remote_identity.clone());
    let worktree = worktree_id(&record.repo_id, &canonical, mode);
    Ok((record.repo_id, worktree, remote_identity))
}

/// Builds the FTS5 MATCH expression: every whitespace term must match at
/// least one field; terms are quoted with doubled inner quotes.
pub fn fts_match_expression(query: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for term in query.split_whitespace() {
        if term.is_empty() {
            continue;
        }
        let quoted = format!("\"{}\"", term.replace('"', "\"\""));
        parts.push(format!(
            "path :{quoted} OR heading :{quoted} OR symbol :{quoted} OR body :{quoted}"
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

/// One FTS candidate row before ranking.
struct Candidate {
    chunk_id: String,
    path: String,
    heading: String,
    bm25: f64,
    line_start: u32,
    line_end: u32,
    text: String,
    role: String,
    lifecycle: String,
    content_hash: String,
}

/// Estimates token count (character count / 4), labeled as an estimate.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Builds a compact excerpt around the first case-insensitive term match.
pub fn excerpt(text: &str, terms: &[String]) -> String {
    let lower = text.to_lowercase();
    let mut position = None;
    for term in terms {
        if let Some(index) = lower.find(term.to_lowercase().as_str()) {
            let char_index = text[..index].chars().count();
            position = Some(char_index);
            break;
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let Some(start) = position else {
        return chars.iter().take(240).collect();
    };
    let begin = start.saturating_sub(120);
    let end = (start + 120).min(chars.len());
    let mut out: String = chars[begin..end].iter().collect();
    out = out.split('\n').collect::<Vec<&str>>().join(" ");
    if begin > 0 {
        out = format!("…{out}");
    }
    if end < chars.len() {
        out = format!("{out}…");
    }
    out
}

/// A validated GitHub permalink for a result, when the remote and revision
/// make one safe to construct.
pub fn github_url(
    remote_identity: &Option<String>,
    revision: &Option<String>,
    path: &str,
    line_start: u32,
    line_end: u32,
) -> Option<String> {
    let remote = remote_identity.as_deref()?;
    let (owner, repo) = remote.strip_prefix("github.com/")?.split_once('/')?;
    let revision = revision.as_deref()?;
    let hex = revision.strip_prefix("git:")?;
    if hex.len() != 40 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!(
        "https://github.com/{owner}/{repo}/blob/{hex}/{path}#L{line_start}-L{line_end}"
    ))
}

/// Runs one offline lexical search against the current published generation.
pub fn search(
    adapter: &dyn crate::adapter::SourceAdapter,
    mode: SnapshotMode,
    store: &Store,
    request: &SearchRequest,
) -> Result<SearchResponse> {
    let (repo_id, worktree_id, remote_identity) = scope_for_search(adapter, mode)?;
    let conn = store.open()?;
    let Some(meta) = store::current_generation(&conn, &repo_id, &worktree_id)? else {
        return Err(Error::IndexState(
            "no published index for this scope; run `repoise index`".into(),
        ));
    };
    let now = crate::indexing::now_ms();
    let filters = format!(
        "{:?}|{:?}|{:?}",
        request.path_filter, request.role_filter, request.max_results
    );
    let offset = match &request.cursor {
        Some(cursor) => decode_cursor(cursor, meta.generation_id, &request.query, &filters, now)?,
        None => 0,
    };
    let max_results = request
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .min(MAX_RESULTS_CAP);
    let budget = request
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let terms: Vec<String> = request
        .query
        .split_whitespace()
        .map(str::to_string)
        .collect();

    let mut candidates: Vec<Candidate> = Vec::new();
    let matched_any = fts_match_expression(&request.query).is_some();
    if matched_any {
        let match_expr = fts_match_expression(&request.query).expect("terms");
        let mut sql = String::from(
            "SELECT chunk_fts.chunk_id, chunk_fts.path, chunk_fts.heading, \
             bm25(chunk_fts), c.line_start, c.line_end, c.text, fr.role, \
             fr.lifecycle, fr.content_hash \
             FROM chunk_fts \
             JOIN chunk c ON c.chunk_id = chunk_fts.chunk_id \
             JOIN file fr ON fr.generation_id = c.generation_id AND fr.path = c.path \
             WHERE c.generation_id = ?1 AND chunk_fts MATCH ?2",
        );
        let mut next_param = 3;
        if request.path_filter.is_some() {
            sql.push_str(" AND chunk_fts.path GLOB ?3");
            next_param = 4;
        }
        if request.role_filter.is_some() {
            sql.push_str(&format!(" AND fr.role = ?{next_param}"));
        }
        sql.push_str(" ORDER BY bm25(chunk_fts) ASC, chunk_fts.path, c.line_start LIMIT ");
        sql.push_str(&CANDIDATE_CAP.to_string());
        let mut stmt = conn.prepare(&sql).map_err(Error::Sqlite)?;
        use rusqlite::params;
        let (path_param, role_param) = (
            request.path_filter.as_deref(),
            request.role_filter.as_ref().map(|role| role.name()),
        );
        let rows = match (path_param, role_param) {
            (Some(path), Some(role)) => stmt
                .query_map(
                    params![meta.generation_id, match_expr, path, role],
                    read_candidate,
                )
                .map_err(Error::Sqlite)?,
            (Some(path), None) => stmt
                .query_map(
                    params![meta.generation_id, match_expr, path],
                    read_candidate,
                )
                .map_err(Error::Sqlite)?,
            (None, Some(role)) => stmt
                .query_map(
                    params![meta.generation_id, match_expr, role],
                    read_candidate,
                )
                .map_err(Error::Sqlite)?,
            (None, None) => stmt
                .query_map(params![meta.generation_id, match_expr], read_candidate)
                .map_err(Error::Sqlite)?,
        };
        for row in rows {
            candidates.push(row.map_err(Error::Sqlite)?);
        }
    }

    // Explainable ranking: BM25 (lower is better) plus precedence boosts.
    let history_query = terms
        .iter()
        .any(|term| HISTORY_WORDS.contains(&term.to_lowercase().as_str()));
    let mut ranked: Vec<(f64, String, &Candidate)> = Vec::new();
    for candidate in &candidates {
        let mut score = candidate.bm25;
        let mut reasons = vec!["bm25 lexical match".to_string()];
        let path_lower = candidate.path.to_lowercase();
        if terms
            .iter()
            .any(|term| path_lower.contains(&term.to_lowercase()))
        {
            score -= 10.0;
            reasons.push("path/identifier precedence".to_string());
        }
        if !candidate.heading.is_empty()
            && terms.iter().any(|term| {
                candidate
                    .heading
                    .split(" > ")
                    .any(|h| h.to_lowercase() == term.to_lowercase())
            })
        {
            score -= 4.0;
            reasons.push("heading/symbol match".to_string());
        }
        let role_boosted = if history_query {
            matches!(
                candidate.role.as_str(),
                "plan" | "decision" | "historical" | "execution-record"
            )
        } else {
            matches!(candidate.role.as_str(), "current-doc" | "instruction")
        };
        if role_boosted {
            score -= 2.0;
            reasons.push(
                if history_query {
                    "history/decision role boost"
                } else {
                    "current-document preference"
                }
                .to_string(),
            );
        }
        ranked.push((score, reasons.join("; "), candidate));
    }
    ranked.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap()
            .then_with(|| a.2.path.cmp(&b.2.path))
            .then_with(|| a.2.line_start.cmp(&b.2.line_start))
    });

    // Cap hits per file, then paginate from the validated offset.
    let mut per_file: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut deduped: Vec<(f64, String, &Candidate)> = Vec::new();
    for entry in ranked {
        let count = per_file.entry(entry.2.path.as_str()).or_insert(0);
        if *count < MAX_HITS_PER_FILE {
            *count += 1;
            deduped.push(entry);
        }
    }
    let page_start = (offset as usize).min(deduped.len());
    let page_end = (page_start + max_results as usize).min(deduped.len());
    let page = &deduped[page_start..page_end];
    let more_pages = page_end < deduped.len();

    let mut results: Vec<SearchHit> = Vec::new();
    let mut output_tokens: u64 = 0;
    let mut budget_exhausted = false;
    for (_score, reasons, candidate) in page {
        let title = if candidate.heading.is_empty() {
            std::path::Path::new(&candidate.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| candidate.path.clone())
        } else {
            candidate.heading.clone()
        };
        let hit_excerpt = excerpt(&candidate.text, &terms);
        let hit_tokens = estimate_tokens(&hit_excerpt)
            .saturating_add(estimate_tokens(&title))
            .saturating_add(estimate_tokens(reasons));
        if output_tokens.saturating_add(hit_tokens) > budget && !results.is_empty() {
            budget_exhausted = true;
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
        results.push(SearchHit {
            source_id: candidate.chunk_id.clone(),
            path: candidate.path.clone(),
            revision_hash: candidate.content_hash.clone(),
            role: candidate.role.clone(),
            lifecycle: candidate.lifecycle.clone(),
            line_start: candidate.line_start,
            line_end: candidate.line_end,
            title,
            excerpt: hit_excerpt,
            explanation: reasons.to_string(),
            url,
        });
    }
    let fallback = if results.is_empty() {
        fallback_paths(&conn, meta.generation_id, &terms)?
    } else {
        None
    };

    let scope = ScopeView {
        repo_id: repo_id.clone(),
        worktree_id: worktree_id.clone(),
        snapshot_mode: format!("{mode:?}"),
        snapshot_id: meta.snapshot_id.clone(),
        revision: meta.revision_id.clone(),
    };
    let coverage = Coverage {
        files: meta.files,
        chunks: meta.chunks,
        lexical: format!("{} chunks, fts5 unicode61", meta.chunks),
        vector: "none (offline lexical only)".into(),
    };
    let truncated = more_pages || budget_exhausted;
    Ok(SearchResponse {
        schema_version: RESPONSE_SCHEMA_VERSION,
        scope,
        generation_id: meta.generation_id,
        retrieval_mode: "lexical".into(),
        coverage,
        truncated,
        next_cursor: if truncated && !results.is_empty() {
            Some(encode_cursor(
                meta.generation_id,
                &request.query,
                &filters,
                page_end as u32,
                now,
            ))
        } else {
            None
        },
        output_tokens,
        tokens_estimated: true,
        results,
        fallback,
    })
}

/// Reads one candidate row from the FTS join.
fn read_candidate(row: &rusqlite::Row) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        chunk_id: row.get(0)?,
        path: row.get(1)?,
        heading: row.get(2)?,
        bm25: row.get(3)?,
        line_start: row.get::<_, i64>(4)? as u32,
        line_end: row.get::<_, i64>(5)? as u32,
        text: row.get(6)?,
        role: row.get(7)?,
        lifecycle: row.get(8)?,
        content_hash: row.get(9)?,
    })
}

/// Lexical/filesystem fallback: file paths containing the first query term.
fn fallback_paths(
    conn: &rusqlite::Connection,
    generation_id: i64,
    terms: &[String],
) -> Result<Option<Vec<String>>> {
    let Some(term) = terms.first() else {
        return Ok(None);
    };
    let like = format!("%{}%", term.replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = conn
        .prepare(
            "SELECT path FROM file WHERE generation_id = ?1 AND path LIKE ?2 \
                 ORDER BY path LIMIT 5",
        )
        .map_err(Error::Sqlite)?;
    let rows = stmt
        .query_map(rusqlite::params![generation_id, like], |r| {
            r.get::<_, String>(0)
        })
        .map_err(Error::Sqlite)?;
    let mut paths = Vec::new();
    for row in rows {
        paths.push(row.map_err(Error::Sqlite)?);
    }
    if paths.is_empty() {
        Ok(None)
    } else {
        Ok(Some(paths))
    }
}
