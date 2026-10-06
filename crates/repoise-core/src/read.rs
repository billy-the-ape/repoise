//! Exact source read-back for a search hit (master plan sections 6 and 8).
//!
//! `read` resolves one opaque source id against the current published
//! generation of the declared scope, re-reads the exact source file at the
//! snapshot, and validates the expected content hash and chunk text hash.
//! Any mismatch is a stale diagnostic (never a silent fallback to different
//! content). Provenance carries source kind, opaque revision, manifest hash
//! and per-file content hash; a GitHub permalink is included only for GitHub
//! remotes with a Git revision.

use serde::Serialize;

use crate::Result;
use crate::adapter::SnapshotMode;
use crate::error::Error;
use crate::hash;
use crate::search::{github_url, scope_for_search};
use crate::store::{self, Store};

/// One exact read request.
#[derive(Clone, Debug)]
pub struct ReadRequest {
    /// Opaque source id from a search hit.
    pub source_id: String,
}

/// One exact read result.
#[derive(Clone, Debug, Serialize)]
pub struct ReadResult {
    /// Opaque source id.
    pub source_id: String,
    /// Scope the read ran against.
    pub scope: crate::search::ScopeView,
    /// Served generation id.
    pub generation_id: i64,
    /// Repository-relative path.
    pub path: String,
    /// Content hash of the file at the snapshot.
    pub content_hash: String,
    /// Alias of `content_hash` for search-result parity.
    pub revision_hash: String,
    /// Exact 1-based inclusive line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Inclusive byte range in the snapshot file.
    pub byte_start: u64,
    pub byte_end: u64,
    /// Chunk text at the validated range.
    pub text: String,
    /// Classification role.
    pub role: String,
    /// Lifecycle.
    pub lifecycle: String,
    /// Validated permalink (GitHub remotes with Git revisions only).
    pub url: Option<String>,
}
/// Reads one source exactly, validating hashes against the live snapshot.
pub fn read(
    adapter: &dyn crate::adapter::SourceAdapter,
    mode: SnapshotMode,
    store: &Store,
    request: &ReadRequest,
) -> Result<ReadResult> {
    let (repo_id, worktree_id, remote_identity) = scope_for_search(adapter, mode)?;
    let conn = store.open()?;
    let Some(meta) = store::current_generation(&conn, &repo_id, &worktree_id)? else {
        return Err(Error::IndexState(
            "no published index for this scope; run `repoise index`".into(),
        ));
    };
    let chunk = {
        let mut stmt = conn
            .prepare(
                "SELECT c.chunk_id, c.path, c.line_start, c.line_end, c.byte_start, \
                 c.byte_end, c.text_hash, fr.content_hash, fr.role, fr.lifecycle \
                 FROM chunk c \
                 JOIN file fr ON fr.generation_id = c.generation_id AND fr.path = c.path \
                 WHERE c.generation_id = ?1 AND c.chunk_id = ?2",
            )
            .map_err(Error::Sqlite)?;
        let rows = stmt
            .query_map(
                rusqlite::params![meta.generation_id, request.source_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u32,
                        r.get::<_, i64>(3)? as u32,
                        r.get::<_, i64>(4)? as u64,
                        r.get::<_, i64>(5)? as u64,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, String>(9)?,
                    ))
                },
            )
            .map_err(Error::Sqlite)?;
        let mut found = None;
        for row in rows {
            found = Some(row.map_err(Error::Sqlite)?);
        }
        found
    }
    .ok_or_else(|| {
        Error::IndexState(format!(
            "source id not in the current generation: {}",
            request.source_id
        ))
    })?;
    let (
        _,
        path,
        line_start,
        line_end,
        byte_start,
        byte_end,
        text_hash,
        content_hash,
        role,
        lifecycle,
    ) = chunk;

    // Re-read the exact source at the snapshot and validate the file hash.
    let revision = adapter.resolve(None, mode)?;
    let bytes = adapter.read(&revision, mode, std::path::Path::new(&path))?;
    let current_hash = hash::sha256_hex(&bytes);
    if current_hash != content_hash {
        return Err(Error::Stale {
            path: path.clone(),
            revision_hash: current_hash,
            reason: format!("content hash changed since the index: expected {content_hash}"),
        });
    }
    // Validate the exact range text against the stored chunk text hash.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    if line_start > line_end || line_end as usize > lines.len() {
        return Err(Error::Stale {
            path: path.clone(),
            revision_hash: current_hash,
            reason: "stored line range no longer exists in the file".into(),
        });
    }
    let range_text = lines[(line_start as usize - 1)..line_end as usize].join("\n");
    if hash::sha256_hex(&range_text) != text_hash {
        return Err(Error::Stale {
            path: path.clone(),
            revision_hash: current_hash,
            reason: "chunk text at the stored range differs from the index".into(),
        });
    }
    Ok(ReadResult {
        source_id: request.source_id.clone(),
        scope: crate::search::ScopeView {
            repo_id,
            worktree_id,
            snapshot_mode: format!("{mode:?}"),
            snapshot_id: meta.snapshot_id,
            revision: meta.revision_id.clone(),
        },
        generation_id: meta.generation_id,
        path: path.clone(),
        content_hash: content_hash.clone(),
        revision_hash: content_hash,
        line_start,
        line_end,
        byte_start,
        byte_end,
        text: range_text,
        role,
        lifecycle,
        url: github_url(
            &remote_identity,
            &meta.revision_id,
            &path,
            line_start,
            line_end,
        ),
    })
}
