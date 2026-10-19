//! Cross-worktree reuse for linked checkouts of one local repository.
//!
//! A *reuse family* is the set of linked worktrees of one local Git
//! repository (primary checkout plus attached/detached worktrees). Family
//! data is immutable and content-keyed, so it is safe to share: each
//! worktree still builds and publishes its own complete, scope-bound index,
//! and exact reads always validate against the destination snapshot.
//!
//! The shared store is a versioned, Repoise-owned directory:
//!
//! - default (checkout-local cache root, `.repoise`):
//!   `<git-common-dir>/repoise/<STORE_VERSION>/` — discovered automatically,
//!   no per-task environment setup;
//! - explicit cache root (`REPOISE_CACHE_DIR` or a non-default `cacheDir`):
//!   `<cache-root>/families/<family-id>/<STORE_VERSION>/` — always
//!   family-namespaced, so unrelated repositories sharing one external
//!   cache root never share data.
//!
//! Layout inside the store: `parse-cache/` (immutable parse artifacts),
//! `embeddings.sqlite` (the family-scoped content-addressed embedding cache)
//! and `members/` (one small file per registered worktree scope, used by
//! garbage collection). Everything here is generated data, excluded from the
//! corpus by the `.git`/cache package defaults, and safe to delete: deletion
//! only costs a rebuild, never correctness.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::DEFAULT_CACHE_DIR;
use crate::Result;
use crate::adapter::SourceAdapter;
use crate::cache::CachePaths;
use crate::config::EffectiveConfig;
use crate::error::Error;
use crate::hash;

/// Version of the shared-store directory layout. Bumping it makes every
/// existing store a miss (old stores are simply rebuilt, never rewritten).
pub const SHARED_STORE_VERSION: u32 = 1;
/// Store directory name under the canonical Git common directory.
pub const SHARED_STORE_NAME: &str = "repoise";
/// Version of the serialized parse-artifact format.
pub const ARTIFACT_VERSION: u32 = 1;
/// Bounded retention for parse artifacts (oldest are evicted first).
pub const MAX_PARSE_ARTIFACTS: usize = 20_000;
/// GC grace: embedding cache entries created within this window are never
/// reclaimed, so an in-flight sibling build cannot lose vectors it is about
/// to reference (liveness cannot always be proved; retain conservatively).
pub const GC_GRACE_MS: i64 = 3_600_000;

/// Milliseconds cutoff for GC grace: entries newer than this are retained.
pub fn gc_grace_cutoff_ms(now_ms: i64) -> i64 {
    now_ms.saturating_sub(GC_GRACE_MS)
}

/// One registered worktree scope that references the family store.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberEntry {
    /// Root-bound repository scope id of the worktree.
    pub repo_id: String,
    /// Worktree scope id.
    pub worktree_id: String,
    /// Absolute path of the worktree's index database.
    pub db_path: PathBuf,
    /// Last registration time (milliseconds since the Unix epoch).
    pub updated_at_ms: i64,
}

/// A parse artifact: the immutable structural parse output of one file.
///
/// Artifacts are keyed by the exact inputs that determine the output:
/// content hash, detected language, parser version, logical corpus and the
/// repository-relative path (labeled-split context labels embed the path, so
/// the path is part of the parse input). Scope-bound ids (chunk ids, symbol
/// ids, reference ids) are never stored; the destination rematerializes them
/// for its own scope and re-resolves cross-file edges.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseArtifact {
    /// Artifact format version (other values are treated as misses).
    pub version: u32,
    /// SHA-256 of the file content the artifact was parsed from.
    pub content_hash: String,
    /// Detected language key.
    pub language: String,
    /// Parser version that produced the chunks.
    pub parser_version: String,
    /// Logical corpus (`docs`, `config` or `code`).
    pub corpus: String,
    /// Repository-relative path (POSIX separators).
    pub path: String,
    /// Chunk units (pre-redaction text; the destination re-applies redaction
    /// and re-derives text hashes).
    pub chunks: Vec<ArtifactChunk>,
    /// Defined code symbols (document order; `parent` is an index into this
    /// list). Empty for non-code files.
    pub symbols: Vec<ArtifactSymbol>,
    /// Import bindings (code files only).
    pub imports: Vec<ArtifactImport>,
    /// Local reference hints (code files only).
    pub refs: Vec<ArtifactRef>,
}

/// One chunk unit inside a parse artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactChunk {
    /// Index among siblings.
    pub ordinal: u32,
    /// Parent chunk ordinal for labeled splits.
    pub parent: Option<u32>,
    /// Heading ancestry (outermost first).
    pub heading_path: Vec<String>,
    /// Chunk text before destination redaction.
    pub text: String,
    /// 1-based inclusive source line range (valid for any byte-identical
    /// file, so ranges are stored and reused, not recomputed).
    pub line_start: u32,
    pub line_end: u32,
    /// Inclusive byte range in the source file.
    pub byte_start: u64,
    pub byte_end: u64,
    /// Primary defined symbol (code files; empty otherwise).
    pub symbol: String,
    /// Parent/signature context for a labeled split (metadata only).
    pub context: Option<String>,
}

/// One defined code symbol inside a parse artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSymbol {
    /// Symbol name.
    pub name: String,
    /// Declaration kind name (kebab-case, as serialized by the parser).
    pub kind: String,
    /// 1-based inclusive line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Index into the artifact's symbol list of the enclosing symbol.
    pub parent: Option<usize>,
    /// Module-level export.
    pub exported: bool,
}

/// One import binding inside a parse artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactImport {
    /// Line of the import.
    pub line: u32,
    /// Locally bound name.
    pub local_name: String,
    /// Module specifier (quotes stripped).
    pub module: String,
}

/// One local reference hint inside a parse artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    /// Line of the reference.
    pub line: u32,
    /// Unqualified referenced name.
    pub name: String,
    /// Edge kind name (kebab-case).
    pub kind: String,
}

impl ParseArtifact {
    /// Whether all stored reference kinds are known to this build. An unknown
    /// kind marks the artifact incompatible (the file is reparsed locally) so
    /// partial edge data is never silently dropped.
    pub fn refs_have_known_kinds(&self) -> bool {
        self.refs.iter().all(|reference| {
            serde_json::from_value::<crate::chunk::code::RefKind>(serde_json::Value::String(
                reference.kind.clone(),
            ))
            .is_ok()
        })
    }
}

/// Content-addressed artifact key: the exact parse inputs hashed to a short,
/// path-safe name (artifacts are immutable, so the key never mutates).
fn key_for(
    content_hash: &str,
    language: &str,
    parser_version: &str,
    corpus: &str,
    path: &str,
) -> String {
    let key = format!("{content_hash}|{language}|{parser_version}|{corpus}|{path}");
    hash::sha256_hex(&key)
}

/// Content-addressed store of immutable parse artifacts. One JSON file per
/// artifact key, written atomically (temporary file plus rename) so
/// concurrent processes can never observe a partial artifact.
#[derive(Clone, Debug)]
pub struct ArtifactCache {
    dir: PathBuf,
}

impl ArtifactCache {
    /// Opens the artifact store under a shared-store root.
    pub fn open(root: &Path) -> Self {
        Self {
            dir: root.join("parse-cache"),
        }
    }

    /// The store directory (for diagnostics and tests).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Looks up the artifact for one destination file. Any read/parse
    /// failure, unknown version or key mismatch is a safe miss (the file
    /// will be reparsed locally).
    pub fn get(
        &self,
        content_hash: &str,
        language: &str,
        parser_version: &str,
        corpus: &str,
        path: &str,
    ) -> Result<Option<ParseArtifact>> {
        let key = key_for(content_hash, language, parser_version, corpus, path);
        let file = self.dir.join(format!("{key}.json"));
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::Io(err)),
        };
        match serde_json::from_slice::<ParseArtifact>(&bytes) {
            Ok(artifact)
                if artifact.version == ARTIFACT_VERSION
                    && artifact.content_hash == content_hash
                    && artifact.language == language
                    && artifact.parser_version == parser_version
                    && artifact.corpus == corpus
                    && artifact.path == path =>
            {
                Ok(Some(artifact))
            }
            _ => {
                // Corrupt, truncated or incompatible artifact: reject and
                // rebuild through the normal safe path.
                let _ = fs::remove_file(&file);
                Ok(None)
            }
        }
    }

    /// Atomically stores one artifact. Bounded retention is enforced once
    /// per build via `enforce_bound`, not on every write.
    pub fn put(&self, artifact: &ParseArtifact) -> Result<()> {
        let key = key_for(
            &artifact.content_hash,
            &artifact.language,
            &artifact.parser_version,
            &artifact.corpus,
            &artifact.path,
        );
        fs::create_dir_all(&self.dir)?;
        let target = self.dir.join(format!("{key}.json"));
        let tmp = self
            .dir
            .join(format!("{key}.json.tmp-{}", std::process::id()));
        fs::write(
            &tmp,
            serde_json::to_vec(artifact).map_err(|err| Error::Json(err.to_string()))?,
        )?;
        // Atomic publication: on platforms where rename requires an absent
        // target, remove first (a same-key artifact is deterministic, so a
        // lost race still yields valid data).
        if target.exists() {
            let _ = fs::remove_file(&target);
        }
        fs::rename(&tmp, &target).map_err(Error::Io)?;
        Ok(())
    }

    /// Keeps the store within `MAX_PARSE_ARTIFACTS` by evicting the oldest
    /// entries (best effort; eviction is always safe because artifacts are
    /// reparseable). Callers run this once per build, after the file loop, so
    /// a cold build is linear in the number of reparsed files rather than
    /// quadratic in the store size.
    pub fn enforce_bound(&self) {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let mtime = fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            files.push((path, mtime));
        }
        if files.len() <= MAX_PARSE_ARTIFACTS {
            return;
        }
        files.sort_by_key(|entry| entry.1);
        let excess = files.len() - MAX_PARSE_ARTIFACTS;
        for (path, _) in files.into_iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }
}

/// The shared family store: a versioned, Repoise-owned directory plus the
/// family identity that names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedStore {
    /// Opaque family id (`family-<sha256>`).
    pub family_id: String,
    /// Versioned store root (e.g. `<common-dir>/repoise/1` or
    /// `<cache-root>/families/<family-id>/1`).
    pub root: PathBuf,
}

/// Why a build uses (or does not use) the shared family store.
#[derive(Clone, Debug)]
pub enum SharingState {
    /// The shared store is resolved and will be used for this build.
    Shared(SharedStore),
    /// Sharing is disabled by configuration (`sharedCache: false`).
    Disabled,
    /// The adapter has no reuse family (plain directories, fake adapters).
    Unavailable,
    /// A family exists but the store cannot be used; the build stays isolated
    /// (e.g. the common directory vanished). Never claims shared hits.
    Fallback(String),
}

/// Canonicalizes `path` as far as its existing prefix goes, so equivalent
/// spellings of the same location (symlinked prefixes such as macOS
/// `/var` vs `/private/var`, short Windows paths, `..` components) compare
/// equal. Components beyond the deepest existing ancestor are re-joined
/// lexically; the input is returned unchanged when no ancestor exists.
fn canonicalize_existing(path: &Path) -> PathBuf {
    if path.exists() {
        return fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    }
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    while let Some(name) = current.file_name() {
        remainder.push(name.to_os_string());
        let Some(parent) = current.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() {
            break;
        }
        if parent.exists() {
            let base = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            let mut out = base;
            for name in remainder.iter().rev() {
                out.push(name);
            }
            return out;
        }
        current = parent;
    }
    path.to_path_buf()
}

/// Resolves the sharing state for one declared scope. This probe never
/// creates anything (read-only operations must not create shared state);
/// builds create the store directory after resolving.
pub fn resolve_sharing(
    adapter: &dyn SourceAdapter,
    eff: &EffectiveConfig,
    cache: &CachePaths,
) -> Result<SharingState> {
    if !eff.shared_cache {
        return Ok(SharingState::Disabled);
    }
    let Some(family) = adapter.reuse_family()? else {
        return Ok(SharingState::Unavailable);
    };
    let canonical_root = adapter.canonical_root()?;
    let version = PathBuf::from(SHARED_STORE_VERSION.to_string());
    let checkout_local_default = canonical_root.join(DEFAULT_CACHE_DIR);
    // Compare canonicalized locations, never raw spellings: a non-canonical
    // checkout root (symlink, short path) must still select the same store
    // as its siblings.
    let is_default =
        canonicalize_existing(&cache.root) == canonicalize_existing(&checkout_local_default);
    let root = if is_default {
        // Default checkout-local cache: shared data lives with the
        // repository itself, under the canonical Git common directory
        // (always outside the corpus). Explicit cache locations
        // (`cacheDir` or `REPOISE_CACHE_DIR`) keep the operator's chosen
        // root; families are namespaced under it.
        let root = family.anchor.join(SHARED_STORE_NAME).join(version);
        if !family.anchor.is_dir() {
            return Ok(SharingState::Fallback(
                "repository common directory is unavailable; indexed isolated".to_string(),
            ));
        }
        root
    } else {
        // Explicit cache root: keep the operator's chosen location, but keep
        // every family isolated under it.
        cache.root.join("families").join(&family.id).join(version)
    };
    Ok(SharingState::Shared(SharedStore {
        family_id: family.id,
        root,
    }))
}

/// Serializable sharing diagnostic for reports (doctor, index, status).
#[derive(Clone, Debug, Default, Serialize)]
pub struct SharingInfo {
    /// State name: `shared`, `disabled`, `unavailable` or `fallback`.
    pub state: String,
    /// Family id (`shared` state only).
    pub family_id: Option<String>,
    /// Shared store root (`shared` state only).
    pub store: Option<PathBuf>,
    /// Note for `unavailable`/`fallback` states.
    pub note: Option<String>,
}

/// Describes the sharing state for reports.
pub fn describe(state: &SharingState) -> SharingInfo {
    match state {
        SharingState::Shared(store) => SharingInfo {
            state: "shared".into(),
            family_id: Some(store.family_id.clone()),
            store: Some(store.root.clone()),
            note: None,
        },
        SharingState::Disabled => SharingInfo {
            state: "disabled".into(),
            family_id: None,
            store: None,
            note: None,
        },
        SharingState::Unavailable => SharingInfo {
            state: "unavailable".into(),
            family_id: None,
            store: None,
            note: Some("adapter has no linked-checkout family (plain directory)".into()),
        },
        SharingState::Fallback(reason) => SharingInfo {
            state: "fallback".into(),
            family_id: None,
            store: None,
            note: Some(reason.clone()),
        },
    }
}

/// Validates a scope id the same way purging does: `[a-z0-9][a-z0-9-]*`.
fn validate_scope_id(value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(Error::Policy(format!("invalid scope id: {value}")))
    }
}

/// Registers (or refreshes) one worktree scope in the family member
/// registry. Each scope owns exactly one file, so concurrent registrations
/// never read-modify-write each other.
pub fn upsert_member(root: &Path, repo_id: &str, worktree_id: &str, db_path: &Path) -> Result<()> {
    validate_scope_id(repo_id)?;
    validate_scope_id(worktree_id)?;
    let dir = members_dir(root);
    fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{repo_id}--{worktree_id}.json"));
    let tmp = dir.join(format!(
        "{repo_id}--{worktree_id}.json.tmp-{}",
        std::process::id()
    ));
    let entry = MemberEntry {
        repo_id: repo_id.to_string(),
        worktree_id: worktree_id.to_string(),
        db_path: db_path.to_path_buf(),
        updated_at_ms: crate::shared::now_ms(),
    };
    fs::write(
        &tmp,
        serde_json::to_vec(&entry).map_err(|err| Error::Json(err.to_string()))?,
    )?;
    if file.exists() {
        let _ = fs::remove_file(&file);
    }
    fs::rename(&tmp, &file).map_err(Error::Io)?;
    Ok(())
}

/// Removes one worktree scope's registration from the family member
/// registry (purge path). Sibling registrations are untouched; a missing
/// file is a no-op.
pub fn remove_member(root: &Path, repo_id: &str, worktree_id: &str) -> Result<()> {
    validate_scope_id(repo_id)?;
    validate_scope_id(worktree_id)?;
    let file = members_dir(root).join(format!("{repo_id}--{worktree_id}.json"));
    match fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::Io(err)),
    }
}

/// The member-registry directory of one shared store.
pub fn members_dir(root: &Path) -> PathBuf {
    root.join("members")
}

/// Loads all registered members. An unreadable or malformed entry is an
/// error: garbage collection must fail closed rather than guess reference
/// sets.
pub fn load_members(root: &Path) -> Result<Vec<MemberEntry>> {
    let dir = members_dir(root);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::Io(err)),
    };
    let mut members = Vec::new();
    for entry in entries {
        let entry = entry.map_err(Error::Io)?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(Error::Io)?;
        let member: MemberEntry = serde_json::from_slice(&bytes)
            .map_err(|err| Error::Json(format!("{}: {err}", path.display())))?;
        validate_scope_id(&member.repo_id)?;
        validate_scope_id(&member.worktree_id)?;
        members.push(member);
    }
    members.sort_by(|a, b| {
        a.repo_id
            .cmp(&b.repo_id)
            .then(a.worktree_id.cmp(&b.worktree_id))
    });
    Ok(members)
}

/// Current time in milliseconds since the Unix epoch (diagnostics, GC).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
