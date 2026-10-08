//! Bounded local history lane and optional verified hosting enrichment
//! (master plan section 9, card K5).
//!
//! Local history is opt-in and offline-capable: the mainline (first-parent)
//! commits within a configurable horizon, with bounded changed-path summaries
//! and optional bounded diff hunk descriptors. Horizon and shallow-clone gaps
//! are recorded explicitly. History is a separate logical lane in the shared
//! store: it publishes with the same generation transaction, but history
//! search is a separate lane so old code never pollutes current search.
//!
//! Hosting enrichment is optional, disabled by default, and verified: commit
//! messages may carry unverified PR hints, but a change-request association
//! is stored only when the host confirms the commit belongs to it. Remote
//! content is ETag-cached under the repository scope, bounded by request
//! budgets, and fails closed: a remote permission denial invalidates the
//! cached remote content instead of serving it.
//!
//! Adapters own revision semantics through [`HistoryProvider`]; hosts own
//! credential access and URL construction through [`EnrichmentProvider`]
//! (the CLI ships the GitHub transport). Revisions stay adapter-qualified
//! opaque ids; hosting fields stay in namespaced, optional metadata.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::adapter::{AdapterError, Capability, SourceAdapter};
use crate::error::Error;
use crate::hash;

/// Kind of an explicit history coverage gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GapKind {
    /// The clone is shallow: history below the shallow boundary is unavailable.
    ShallowClone,
    /// The horizon cut the mainline short: older commits exist but are not recorded.
    Horizon,
    /// The source scope has no history capability (the lane is empty, not failed).
    Unavailable,
    /// Commit records could not be mapped cleanly; the affected revisions are
    /// omitted from the lane (never silent).
    Format,
}

/// One explicit history coverage gap (never silent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryGap {
    /// Gap kind.
    pub kind: GapKind,
    /// Human-readable detail.
    pub detail: String,
}

/// One bounded diff hunk descriptor (never the patch text itself).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkDescriptor {
    /// Repository-relative POSIX path.
    pub path: String,
    /// Old-side start line (0 when the file is new).
    pub old_start: u32,
    /// Old-side line count.
    pub old_lines: u32,
    /// New-side start line (0 when the file is deleted).
    pub new_start: u32,
    /// New-side line count.
    pub new_lines: u32,
    /// Bounded hunk context header text.
    pub context: String,
}

/// A verified change-request association from a host.
///
/// Present only when the host confirmed the commit belongs to this change
/// request. Unverified hints stay on [`HistoryItem::pr_hint`]; PR text is
/// historical intent, never current implementation proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrAssociation {
    /// Host name (for example `github`); hosting fields stay namespaced.
    pub host: String,
    /// Change request number on the host.
    pub number: u32,
    /// Change request title, when the host provided one (redacted).
    pub title: Option<String>,
    /// Bounded change-request body (historical intent only; redacted).
    pub body: Option<String>,
    /// Host state (`open`, `closed`, `merged`, ...), when provided.
    pub state: Option<String>,
    /// Validated permalink, when safe to construct.
    pub url: Option<String>,
    /// Host-reported last-updated time, milliseconds since the Unix epoch.
    pub updated_at_ms: Option<i64>,
    /// Fetched time, milliseconds since the Unix epoch.
    pub fetched_at_ms: i64,
    /// Always true when stored: the association was verified against the host.
    pub verified: bool,
}

/// One bounded local history item (master plan section 6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryItem {
    /// Adapter-qualified opaque revision id (never assumed to be a 40-hex hash).
    pub revision_id: String,
    /// Optional parent revision ids (adapter-qualified).
    pub parents: Vec<String>,
    /// Commit message (secret shapes redacted before storage).
    pub message: String,
    /// Author name, when the adapter has one.
    pub author: Option<String>,
    /// Commit time, milliseconds since the Unix epoch.
    pub committed_at_ms: Option<i64>,
    /// Bounded affected-path list (POSIX separators).
    pub affected_paths: Vec<String>,
    /// Number of affected paths omitted by the bound.
    pub paths_truncated: u32,
    /// Optional bounded diff hunk descriptors.
    pub hunks: Vec<HunkDescriptor>,
    /// Number of hunks omitted by the bound.
    pub hunks_truncated: u32,
    /// Unverified change-request numbers parsed from the message (hints only).
    pub pr_hint: Option<Vec<u32>>,
    /// Optional verified host/change-request association.
    pub association: Option<PrAssociation>,
}

/// A bounded local history set for one scope.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistorySet {
    /// The newest recorded mainline revision (adapter-qualified), if any.
    pub head_revision: Option<String>,
    /// Number of recorded items.
    pub count: usize,
    /// Explicit coverage gaps (shallow clone, horizon, ...).
    pub gaps: Vec<HistoryGap>,
    /// Recorded items, newest first.
    pub items: Vec<HistoryItem>,
}

/// Default maximum recorded mainline commits (config default).
pub const DEFAULT_HORIZON: u32 = 500;

/// Bounded history collection settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistorySpec {
    /// Maximum recorded mainline commits (default 500).
    pub horizon: u32,
    /// Maximum changed paths kept per commit (default 20).
    pub max_paths_per_commit: u32,
    /// Whether bounded diff hunk descriptors are collected (off by default).
    pub diff_hunks: bool,
}

impl Default for HistorySpec {
    fn default() -> Self {
        Self {
            horizon: 500,
            max_paths_per_commit: 20,
            diff_hunks: false,
        }
    }
}

/// Neutral history collection seam: VCS adapters own revision semantics.
pub trait HistoryProvider {
    /// Collects the bounded mainline history set for this adapter's default
    /// state; unsupported adapters never reach this method (the capability
    /// check happens first).
    fn collect_history(&self, spec: &HistorySpec) -> Result<HistorySet>;
}

/// Collects the history set for an adapter, requiring the explicit
/// `History` capability first. Adapters without it yield
/// [`Error`] via [`AdapterError::UnsupportedOperation`] (no history is not
/// an error state for the offline docs baseline).
pub fn collect_history(adapter: &dyn SourceAdapter, spec: &HistorySpec) -> Result<HistorySet> {
    adapter.require(Capability::History, "history collection")?;
    let Some(provider) = adapter.history_provider() else {
        return Err(Error::from(AdapterError::UnsupportedOperation {
            operation: "history collection",
            capability: Capability::History,
        }));
    };
    provider.collect_history(spec)
}

/// Parses unverified change-request numbers from a commit message.
///
/// Recognizes `#123` at a message start, after whitespace, or after `(`
/// (the common `(#123)` merge form). Results are hints only: an association
/// is verified separately against the host.
pub fn pr_hint_from_message(message: &str) -> Vec<u32> {
    const MAX_HINTS: usize = 5;
    let mut out: Vec<u32> = Vec::new();
    let bytes = message.as_bytes();
    let mut i = 0;
    while i < bytes.len() && out.len() < MAX_HINTS {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        let digits = &message[i + 1..j];
        if !digits.is_empty() && digits.len() <= 9 {
            let previous = bytes[..i].last().copied();
            let boundary = matches!(previous, None | Some(b'(') | Some(b' ') | Some(b'\t'));
            if boundary
                && let Ok(number) = digits.parse::<u32>()
                && number > 0
                && !out.contains(&number)
            {
                out.push(number);
            }
        }
        i = j.max(i + 1);
    }
    out
}

/// A validated GitHub commit permalink, only for a validated GitHub remote
/// and a `git:<40-hex>` revision (never a guessed URL).
pub fn github_commit_url(remote_identity: &Option<String>, revision: &str) -> Option<String> {
    let remote = remote_identity.as_deref()?;
    let (owner, repo) = remote.strip_prefix("github.com/")?.split_once('/')?;
    let hex = revision.strip_prefix("git:")?;
    if hex.len() != 40 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repo}/commit/{hex}"))
}

/// One cached remote record keyed under the repository-scope host cache.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HostCacheEntry {
    /// `ETag` header for conditional revalidation, when the host sent one.
    pub etag: Option<String>,
    /// Fetched time, milliseconds since the Unix epoch.
    pub fetched_at_ms: i64,
    /// Host-qualified payload (the provider owns its shape).
    pub payload: serde_json::Value,
    /// Paged-list continuation (`Link: rel="next"` URL) persisted with a
    /// first page, so a `304` revalidation (which need not repeat the
    /// `Link` header) can keep walking. Additive; older cache files
    /// deserialize as `None`.
    #[serde(default)]
    pub next_page: Option<String>,
}

/// Repository-scoped, host-qualified cache for verified remote records.
///
/// Lives under `repos/<repoId>/host-cache/<host>.json`, so `purge` removes
/// it with the repository scope. A remote permission denial invalidates the
/// whole cache (fail closed): stale remote content is never re-served.
#[derive(Clone, Debug)]
pub struct HostCache {
    /// File path of the JSON cache.
    pub path: PathBuf,
    /// Host name this cache file stores.
    host: &'static str,
}

#[derive(Serialize, Deserialize, Default)]
struct HostCacheFile {
    /// Cache format version.
    version: u32,
    /// Host name the file belongs to.
    host: String,
    /// Keyed entries.
    entries: std::collections::BTreeMap<String, HostCacheEntry>,
}

impl HostCache {
    /// Opens (lazily creating on first write) the cache file for `host`.
    pub fn open(path: PathBuf, host: &'static str) -> Self {
        Self { path, host }
    }

    /// Reads one entry, or `None` when absent (a corrupt or foreign file is
    /// a miss, never a failure: remote enrichment must not break local
    /// indexing).
    pub fn get(&self, key: &str) -> Option<HostCacheEntry> {
        let text = fs::read_to_string(&self.path).ok()?;
        let file: HostCacheFile = serde_json::from_str(&text).ok()?;
        if file.version != 1 || file.host != self.host {
            return None;
        }
        file.entries.get(key).cloned()
    }

    /// Writes one entry (read-modify-write with an atomic replace).
    pub fn put(&self, key: &str, entry: HostCacheEntry) -> Result<()> {
        let mut file = match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str::<HostCacheFile>(&text).unwrap_or_default(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HostCacheFile::default(),
            Err(err) => return Err(Error::Io(err)),
        };
        file.version = 1;
        file.host = self.host.to_string();
        file.entries.insert(key.to_string(), entry);
        let text = serde_json::to_string_pretty(&file)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Removes the cache (remote revocation / explicit retention policy).
    pub fn invalidate(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::Io(err)),
        }
    }
}

/// The result of one verified enrichment attempt.
pub enum EnrichmentOutcome {
    /// The host confirmed the commit belongs to this change request.
    Verified(PrAssociation),
    /// The host confirms no such change request includes this commit
    /// (deleted, wrong hint, or pushed directly).
    NoAssociation,
}

/// Explicit enrichment failures. Rate limits stop the build's enrichment;
/// permission denial fails closed (cached remote content is invalidated);
/// budget exhaustion stops quietly.
#[derive(Debug, PartialEq, Eq)]
pub enum EnrichmentError {
    /// The host rate limit was hit; no further requests this build.
    RateLimited,
    /// The remote denied access (missing/revoked credential); fail closed.
    PermissionDenied,
    /// The configured request budget was exhausted.
    BudgetExhausted,
    /// Any other host failure for this item (the build continues).
    Other(String),
}

impl std::fmt::Display for EnrichmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnrichmentError::RateLimited => write!(f, "host rate limit reached"),
            EnrichmentError::PermissionDenied => write!(f, "host denied access"),
            EnrichmentError::BudgetExhausted => write!(f, "enrichment request budget exhausted"),
            EnrichmentError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for EnrichmentError {}

/// Mutable per-build request budget handed to the provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestBudget {
    /// Requests remaining this build.
    pub remaining: u32,
}

impl RequestBudget {
    /// Charges one request; `false` when the budget is already exhausted.
    pub fn charge(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

/// One seam per hosting provider: the provider owns credential access,
/// pagination, ETag revalidation (through the session's [`HostCache`]), and
/// URL construction. The core never reads token files or sends requests.
pub trait EnrichmentProvider {
    /// Host name (for example `github`).
    fn host(&self) -> &'static str;

    /// The exact remote host component this provider serves (for example
    /// `github.com`); the session gates on it so foreign hosts are skipped.
    fn remote_host(&self) -> &'static str;

    /// Verifies the commit-to-change-request association and fetches bounded
    /// change-request data for one item carrying unverified hints.
    /// `remote` is the sanitized remote identity (`host/owner/repo`).
    fn enrich(
        &self,
        remote: &str,
        revision_id: &str,
        hints: &[u32],
        cache: &HostCache,
        budget: &mut RequestBudget,
    ) -> std::result::Result<EnrichmentOutcome, EnrichmentError>;
}

/// Bounded enrichment settings (defaults fill operator gaps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnrichmentBudgets {
    /// Maximum change requests enriched per build.
    pub max_prs_per_build: u32,
    /// Maximum host requests per build.
    pub max_requests_per_build: u32,
    /// Maximum fetched body characters kept per record.
    pub max_body_chars: u32,
}

impl Default for EnrichmentBudgets {
    fn default() -> Self {
        Self {
            max_prs_per_build: 10,
            max_requests_per_build: 50,
            max_body_chars: 20_000,
        }
    }
}

/// One enrichment session for a build: a host provider plus its
/// repository-scoped cache and budgets.
pub struct EnrichmentSession {
    provider: Box<dyn EnrichmentProvider>,
    cache: HostCache,
    budgets: EnrichmentBudgets,
}

/// Outcome statistics of one build's enrichment pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EnrichmentReport {
    /// Items that carried hints and were attempted.
    pub attempted: usize,
    /// Items given a verified association.
    pub enriched: usize,
    /// Items the host confirmed as having no association.
    pub no_association: usize,
    /// Items whose host request failed.
    pub failed: usize,
    /// The host rate limit stopped the pass.
    pub rate_limited: bool,
    /// A permission denial stopped the pass and invalidated the cache.
    pub permission_denied: bool,
    /// The request budget stopped the pass.
    pub budget_exhausted: bool,
    /// Hint-carrying items that were never attempted (budget, PR cap, or an
    /// early stop) — enrichment never covers them silently.
    pub unattempted: usize,
}

impl EnrichmentSession {
    /// Opens one session over a host cache and budgets.
    pub fn new(
        provider: Box<dyn EnrichmentProvider>,
        cache: HostCache,
        budgets: EnrichmentBudgets,
    ) -> Self {
        Self {
            provider,
            cache,
            budgets,
        }
    }

    /// The host name this session enriches.
    pub fn host(&self) -> &'static str {
        self.provider.host()
    }

    /// The repository-scoped host cache backing this session.
    pub fn cache(&self) -> &HostCache {
        &self.cache
    }

    /// Enriches every hint-carrying item in `set`, bounded by the budgets.
    /// Stops on rate limit (items keep hints), permission denial (fail
    /// closed: the remote cache is invalidated), or budget exhaustion.
    pub fn run(&self, remote: Option<&str>, set: &mut HistorySet) -> EnrichmentReport {
        let mut report = EnrichmentReport::default();
        let Some(remote) = remote else {
            return report;
        };
        // Gate on the exact remote host component the provider serves, so
        // lookalike hosts (`github.acme.corp`) are never sent to the provider.
        if remote.split('/').next() != Some(self.provider.remote_host()) {
            return report;
        }
        let hint_items: Vec<usize> = set
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.pr_hint
                    .as_deref()
                    .is_some_and(|hints| !hints.is_empty())
            })
            .map(|(index, _)| index)
            .collect();
        let mut budget = RequestBudget {
            remaining: self.budgets.max_requests_per_build,
        };
        for (position, index) in hint_items.iter().enumerate() {
            let item = &mut set.items[*index];
            if report.attempted >= self.budgets.max_prs_per_build as usize {
                report.unattempted = hint_items.len() - position;
                break;
            }
            if budget.remaining == 0 {
                report.budget_exhausted = true;
                report.unattempted = hint_items.len() - position;
                break;
            }
            report.attempted += 1;
            let hints = item.pr_hint.clone().unwrap_or_default();
            match self
                .provider
                .enrich(remote, &item.revision_id, &hints, &self.cache, &mut budget)
            {
                Ok(EnrichmentOutcome::Verified(mut association)) => {
                    // Host text is untrusted content: redact before storage
                    // (PR bodies are a classic place for pasted tokens).
                    association.title = association
                        .title
                        .as_ref()
                        .map(|title| crate::ignore::redact_secret_content(title));
                    association.body = association
                        .body
                        .as_ref()
                        .map(|body| crate::ignore::redact_secret_content(body));
                    item.association = Some(association);
                    report.enriched += 1;
                }
                Ok(EnrichmentOutcome::NoAssociation) => {
                    item.association = None;
                    report.no_association += 1;
                }
                Err(EnrichmentError::RateLimited) => {
                    report.rate_limited = true;
                    report.unattempted = hint_items.len() - position - 1;
                    break;
                }
                Err(EnrichmentError::PermissionDenied) => {
                    report.permission_denied = true;
                    report.unattempted = hint_items.len() - position - 1;
                    // Fail closed: never serve refreshed (or previously
                    // cached) remote content after a revocation.
                    let _ = self.cache.invalidate();
                    break;
                }
                Err(EnrichmentError::BudgetExhausted) => {
                    report.budget_exhausted = true;
                    report.unattempted = hint_items.len() - position - 1;
                    break;
                }
                Err(EnrichmentError::Other(_)) => {
                    report.failed += 1;
                }
            }
        }
        report
    }
}

/// Summary of one build's history lane (surfaced by `index` and `status`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySummary {
    /// Recorded items in the published generation.
    pub count: usize,
    /// Items given a verified association.
    pub enriched: usize,
    /// Items the host confirmed as having no association.
    pub no_association: usize,
    /// Enrichment failures.
    pub failed: usize,
    /// Explicit coverage gaps (shallow clone, horizon, ...).
    pub gaps: Vec<String>,
    /// The host rate limit stopped enrichment this build.
    pub rate_limited: bool,
    /// A permission denial stopped enrichment and invalidated the cache.
    pub permission_denied: bool,
    /// The request budget stopped enrichment this build.
    pub budget_exhausted: bool,
    /// Hint-carrying items never attempted (visible, never silent).
    pub unattempted: usize,
}

/// Persisted history-lane coverage state for a published generation
/// (stored as JSON in `generation.history_meta`; empty when the lane is off).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryLaneMeta {
    /// The history lane was enabled for this generation.
    pub enabled: bool,
    /// Build-time coverage/enrichment summary.
    pub summary: Option<HistorySummary>,
}

/// Read view of the history lane state for one published generation: lets
/// `search --lane history` and `status` tell an agent whether the lane is
/// on, how far it reaches, and which gaps or enrichment stops apply.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HistoryLaneStatus {
    /// The history lane was enabled for the current generation.
    pub enabled: bool,
    /// Recorded history items in the current generation.
    pub items: i64,
    /// The newest recorded revision (adapter-qualified), if any.
    pub head_revision: Option<String>,
    /// Explicit coverage gaps (shallow clone, horizon, unavailable).
    pub gaps: Vec<String>,
    /// Enrichment outcome for the current generation, if the lane is on.
    pub enrichment: Option<HistorySummary>,
}

/// Derives the FTS-indexed host text for one association: only the readable
/// title/body/state, never the raw JSON (whose keys would become searchable
/// terms). Empty when there is no association.
pub fn association_fts_text(association: Option<&PrAssociation>) -> String {
    let Some(association) = association else {
        return String::new();
    };
    let mut text = String::new();
    if let Some(title) = &association.title {
        text.push_str(title);
        text.push('\n');
    }
    if let Some(body) = &association.body {
        text.push_str(body);
        text.push('\n');
    }
    if let Some(state) = &association.state {
        text.push_str(state);
    }
    text
}

/// Reads the history lane status for the current generation (lane metadata
/// from the generation row, item count and head from the history tables).
pub fn lane_status(
    conn: &rusqlite::Connection,
    meta: &crate::store::GenerationMeta,
) -> Result<HistoryLaneStatus> {
    let lane_meta: HistoryLaneMeta = if meta.history_meta.trim().is_empty() {
        HistoryLaneMeta::default()
    } else {
        serde_json::from_str(&meta.history_meta).unwrap_or_default()
    };
    let head_revision = crate::store::history_head_revision(conn, meta.generation_id)?;
    Ok(HistoryLaneStatus {
        enabled: lane_meta.enabled,
        items: meta.history_items,
        head_revision,
        gaps: lane_meta
            .summary
            .as_ref()
            .map(|summary| summary.gaps.clone())
            .unwrap_or_default(),
        enrichment: lane_meta.summary,
    })
}

/// Builds one item's stable opaque id for a scope.
pub fn item_id(scope_key: &str, revision_id: &str) -> String {
    let key = format!("{scope_key}\n{revision_id}");
    format!("hist-{}", hash::sha256_hex(&key))
}

/// One ranked history-lane result.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct HistoryHit {
    /// Opaque history item id (for diagnostics; not a chunk `read` target).
    pub source_id: String,
    /// Adapter-qualified opaque revision id.
    pub revision_id: String,
    /// Commit message (bounded excerpt).
    pub message: String,
    /// Author name, when recorded.
    pub author: Option<String>,
    /// Commit time, milliseconds since the Unix epoch.
    pub committed_at_ms: Option<i64>,
    /// Bounded affected-path list.
    pub affected_paths: Vec<String>,
    /// Number of affected paths omitted by the bound.
    pub paths_truncated: u32,
    /// Optional bounded diff hunk descriptors.
    pub hunks: Vec<HunkDescriptor>,
    /// Number of hunks omitted by the bound.
    pub hunks_truncated: u32,
    /// Unverified change-request hints parsed from the message.
    pub pr_hint: Option<Vec<u32>>,
    /// Optional parent revision ids (adapter-qualified).
    pub parents: Vec<String>,
    /// Optional verified host/change-request association.
    pub association: Option<PrAssociation>,
    /// Validated permalink (GitHub remotes with full revisions only).
    pub url: Option<String>,
}

/// One page of history-lane search results.
#[derive(Clone, Debug, Serialize)]
pub struct HistorySearchResponse {
    /// Response schema version.
    pub schema_version: u32,
    /// Scope the search ran against.
    pub scope: crate::search::ScopeView,
    /// Served generation id.
    pub generation_id: i64,
    /// Retrieval lane label.
    pub retrieval_mode: String,
    /// Whether more results exist beyond this page.
    pub truncated: bool,
    /// Opaque cursor for the next page, if truncated.
    pub next_cursor: Option<String>,
    /// Lane coverage state for the served generation (gaps, enrichment).
    pub lane: HistoryLaneStatus,
    /// Ranked results.
    pub results: Vec<HistoryHit>,
}

/// History-lane search request.
#[derive(Clone, Debug, Default)]
pub struct HistorySearchRequest {
    /// Natural-language query.
    pub query: String,
    /// GLOB pattern filter on the recorded affected paths (each recorded
    /// path is matched individually, like the chunk lane).
    pub path_filter: Option<String>,
    /// Page size (default 5, capped at 20).
    pub max_results: Option<u32>,
    /// Opaque pagination cursor from a previous response.
    pub cursor: Option<String>,
}

/// Default history-lane page size.
pub const DEFAULT_MAX_RESULTS: u32 = 5;
/// Hard history-lane page cap.
pub const MAX_RESULTS_CAP: u32 = 20;
/// Bounded message excerpt length in characters.
const MESSAGE_EXCERPT_CHARS: usize = 500;

/// Bounds a message to the excerpt budget with an ellipsis marker.
pub fn bounded_excerpt(text: &str) -> String {
    if text.chars().count() <= MESSAGE_EXCERPT_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(MESSAGE_EXCERPT_CHARS).collect();
    format!("{truncated}…")
}

/// One history candidate row (bm25 is lower-is-better).
struct HistoryRow {
    /// Stable item id (FTS row id).
    item_id: String,
    /// BM25 rank.
    rank: f64,
    /// Revision id.
    revision_id: String,
    /// Commit message.
    message: String,
    /// Commit author, if recorded.
    author: Option<String>,
    /// Commit time, if recorded.
    committed_at_ms: Option<i64>,
    /// JSON array of affected paths.
    paths_json: String,
    /// JSON host metadata (verified association), if any.
    host_json: String,
    /// JSON parent revision list.
    parents_json: String,
    /// Number of affected paths omitted by the bound.
    paths_truncated: i64,
    /// JSON hunk descriptor list.
    hunks_json: String,
    /// Number of hunks omitted by the bound.
    hunks_truncated: i64,
    /// JSON pr-hint list, if any.
    pr_hint_json: String,
}

/// Reads one history candidate row.
fn read_history_row(row: &rusqlite::Row) -> rusqlite::Result<HistoryRow> {
    Ok(HistoryRow {
        item_id: row.get(0)?,
        rank: row.get(1)?,
        revision_id: row.get(2)?,
        message: row.get(3)?,
        author: row.get(4)?,
        committed_at_ms: row.get(5)?,
        paths_json: row.get(6)?,
        host_json: row.get(7)?,
        parents_json: row.get(8)?,
        paths_truncated: row.get(9)?,
        hunks_json: row.get(10)?,
        hunks_truncated: row.get(11)?,
        pr_hint_json: row.get(12)?,
    })
}

/// Builds the FTS5 MATCH expression for the history lane: every whitespace
/// term must match at least one history field (message, revision id,
/// affected paths, or host metadata).
pub fn history_fts_match_expression(query: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for term in query.split_whitespace() {
        if term.is_empty() {
            continue;
        }
        let quoted = format!("\"{}\"", term.replace('"', "\"\""));
        // Parenthesize each per-term OR group: without them the top-level
        // AND binds only the last group's first field (e.g. a two-term
        // query silently became `... OR paths:"t2"` and dropped `t1`).
        parts.push(format!(
            "(message :{quoted} OR revision_id :{quoted} OR paths :{quoted} OR host :{quoted})"
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

/// Searches the history lane of the current published generation. This is a
/// separate lane: current chunk search never sees history items, and this
/// never sees chunks. Ranking is FTS5 BM25 over message and affected paths.
pub fn search_history(
    adapter: &dyn SourceAdapter,
    mode: crate::adapter::SnapshotMode,
    store: &crate::store::Store,
    request: &HistorySearchRequest,
) -> Result<HistorySearchResponse> {
    let (repo_id, worktree_id, remote_identity) = crate::search::scope_for_search(adapter, mode)?;
    let conn = store.open()?;
    let Some(meta) = crate::store::current_generation(&conn, &repo_id, &worktree_id)? else {
        return Err(Error::IndexState(
            "no published index for this scope; run `repoise index`".into(),
        ));
    };
    let now = crate::indexing::now_ms();
    // Lane coverage state: an agent must see gaps (horizon, shallow) and
    // enrichment stops even when a query returns zero hits.
    let lane = lane_status(&conn, &meta)?;
    let max_results = request
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .min(MAX_RESULTS_CAP);
    let filters = format!(
        "history|{:?}|{:?}",
        request.path_filter, request.max_results
    );
    let offset = match &request.cursor {
        Some(cursor) => crate::search::decode_history_cursor(
            cursor,
            meta.generation_id,
            &request.query,
            &filters,
            now,
        )?,
        None => 0,
    };
    let Some(match_expr) = history_fts_match_expression(&request.query) else {
        return Ok(empty_history_response(
            &repo_id,
            &worktree_id,
            mode,
            &meta,
            lane,
        ));
    };
    // Path filter: per-path GLOB semantics (matching the chunk lane),
    // evaluated over each recorded path — not a LIKE over the whole JSON
    // blob, where a `_` in the filter would also match slashes and a `%`
    // in one path could bleed into others.
    let path_filter = request
        .path_filter
        .as_deref()
        .filter(|path| !path.is_empty());
    let mut sql = String::from(
        "SELECT history_fts.item_id, bm25(history_fts), h.revision_id, h.message, h.author, \
         h.committed_at_ms, h.affected_paths, h.host_metadata, h.parents, h.paths_truncated, \
         h.hunks, h.hunks_truncated, h.pr_hint \
         FROM history_fts JOIN history_item h ON h.item_id = history_fts.item_id \
         WHERE h.generation_id = ?1 AND history_fts MATCH ?2",
    );
    if path_filter.is_some() {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM json_each(h.affected_paths) je WHERE je.value GLOB ?3)",
        );
    }
    sql.push_str(" ORDER BY bm25(history_fts) ASC, h.item_id LIMIT ");
    sql.push_str(&crate::search::CANDIDATE_CAP.to_string());
    let mut stmt = conn.prepare(&sql).map_err(Error::Sqlite)?;
    use rusqlite::params;
    let rows = match path_filter {
        Some(path) => stmt
            .query_map(
                params![meta.generation_id, match_expr, path],
                read_history_row,
            )
            .map_err(Error::Sqlite)?,
        None => stmt
            .query_map(params![meta.generation_id, match_expr], read_history_row)
            .map_err(Error::Sqlite)?,
    };
    let mut hits: Vec<HistoryHit> = Vec::new();
    for row in rows {
        let HistoryRow {
            item_id,
            rank: _rank,
            revision_id,
            message,
            author,
            committed_at_ms,
            paths_json,
            host_json,
            parents_json,
            paths_truncated,
            hunks_json,
            hunks_truncated,
            pr_hint_json,
        } = row.map_err(Error::Sqlite)?;
        let affected_paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
        let parents: Vec<String> = serde_json::from_str(&parents_json).unwrap_or_default();
        let hunks: Vec<HunkDescriptor> = serde_json::from_str(&hunks_json).unwrap_or_default();
        let pr_hint: Option<Vec<u32>> = if pr_hint_json.trim().is_empty() {
            None
        } else {
            serde_json::from_str(&pr_hint_json).ok()
        };
        let mut association: Option<PrAssociation> = if host_json.trim().is_empty() {
            None
        } else {
            serde_json::from_str(&host_json).ok()
        };
        if let Some(association) = &mut association {
            // Serve the body under the same excerpt bound as commit messages.
            association.body = association.body.take().map(|body| bounded_excerpt(&body));
        }
        hits.push(HistoryHit {
            source_id: item_id,
            revision_id: revision_id.clone(),
            message: bounded_excerpt(&message),
            author,
            committed_at_ms,
            affected_paths,
            paths_truncated: paths_truncated as u32,
            hunks,
            hunks_truncated: hunks_truncated as u32,
            pr_hint,
            parents,
            association,
            url: github_commit_url(&remote_identity, &revision_id),
        });
    }
    let page_start = (offset as usize).min(hits.len());
    let page_end = (page_start + max_results as usize).min(hits.len());
    let truncated = page_end < hits.len();
    let page = hits[page_start..page_end].to_vec();
    let scope = crate::search::ScopeView {
        repo_id: repo_id.clone(),
        worktree_id: worktree_id.clone(),
        snapshot_mode: format!("{mode:?}"),
        snapshot_id: meta.snapshot_id.clone(),
        revision: meta.revision_id.clone(),
    };
    Ok(HistorySearchResponse {
        schema_version: 2,
        scope,
        generation_id: meta.generation_id,
        retrieval_mode: "history".to_string(),
        truncated,
        next_cursor: if truncated && !page.is_empty() {
            Some(crate::search::encode_history_cursor(
                meta.generation_id,
                &request.query,
                &filters,
                page_end as u32,
                now,
            ))
        } else {
            None
        },
        lane,
        results: page,
    })
}

/// An empty history-lane response (no query terms).
fn empty_history_response(
    repo_id: &str,
    worktree_id: &str,
    mode: crate::adapter::SnapshotMode,
    meta: &crate::store::GenerationMeta,
    lane: HistoryLaneStatus,
) -> HistorySearchResponse {
    HistorySearchResponse {
        schema_version: 2,
        scope: crate::search::ScopeView {
            repo_id: repo_id.to_string(),
            worktree_id: worktree_id.to_string(),
            snapshot_mode: format!("{mode:?}"),
            snapshot_id: meta.snapshot_id.clone(),
            revision: meta.revision_id.clone(),
        },
        generation_id: meta.generation_id,
        retrieval_mode: "history".to_string(),
        truncated: false,
        next_cursor: None,
        lane,
        results: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fake host outcome for a fake provider.
    #[derive(Clone, Copy)]
    enum FakeBehavior {
        Verified,
        NoAssoc,
        RateLimited,
        Denied,
        Other,
    }

    /// A fake host provider returning a fixed outcome per call.
    struct FakeProvider {
        behavior: FakeBehavior,
        calls: std::cell::Cell<usize>,
    }

    impl EnrichmentProvider for FakeProvider {
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
        ) -> std::result::Result<EnrichmentOutcome, EnrichmentError> {
            self.calls.set(self.calls.get() + 1);
            match self.behavior {
                FakeBehavior::Verified => Ok(EnrichmentOutcome::Verified(PrAssociation {
                    host: "github".to_string(),
                    number: 42,
                    title: Some("ghp_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6".to_string()),
                    body: None,
                    state: Some("merged".to_string()),
                    url: None,
                    updated_at_ms: None,
                    fetched_at_ms: 1,
                    verified: true,
                })),
                FakeBehavior::NoAssoc => Ok(EnrichmentOutcome::NoAssociation),
                FakeBehavior::RateLimited => Err(EnrichmentError::RateLimited),
                FakeBehavior::Denied => Err(EnrichmentError::PermissionDenied),
                FakeBehavior::Other => Err(EnrichmentError::Other("boom".to_string())),
            }
        }
    }

    /// One history item with an optional PR hint.
    fn item(n: u32, hint: Option<u32>) -> HistoryItem {
        HistoryItem {
            revision_id: format!("git:{n:040x}"),
            parents: Vec::new(),
            message: "commit message".to_string(),
            author: None,
            committed_at_ms: None,
            affected_paths: Vec::new(),
            paths_truncated: 0,
            hunks: Vec::new(),
            hunks_truncated: 0,
            pr_hint: hint.map(|n| vec![n]),
            association: None,
        }
    }

    /// One fresh host-cache file path per test.
    fn cache_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "repoise-history-{tag}-{}-github.json",
            std::process::id()
        ))
    }

    fn budgets() -> EnrichmentBudgets {
        EnrichmentBudgets::default()
    }

    #[test]
    fn pr_hint_parsing_requires_boundary_and_dedupes() {
        assert_eq!(pr_hint_from_message("Merge #123 into main"), vec![123]);
        assert_eq!(pr_hint_from_message("fixes (#456) and #456"), vec![456]);
        assert_eq!(pr_hint_from_message("no hints here"), Vec::<u32>::new());
        assert_eq!(pr_hint_from_message("issue #1 and #2"), vec![1, 2]);
        // `a#1` is not a hint boundary.
        assert_eq!(pr_hint_from_message("tag v1.0#1"), Vec::<u32>::new());
    }

    #[test]
    fn github_permalink_only_for_validated_remotes_and_revisions() {
        let remote = Some("github.com/owner/repo".to_string());
        let revision = "git:0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            github_commit_url(&remote, revision),
            Some(
                "https://github.com/owner/repo/commit/0123456789abcdef0123456789abcdef01234567"
                    .to_string()
            )
        );
        // Short or foreign revisions never produce a URL.
        assert_eq!(github_commit_url(&remote, "git:abc"), None);
        let other = Some("gitlab.com/owner/repo".to_string());
        assert_eq!(github_commit_url(&other, revision), None);
        assert_eq!(github_commit_url(&None, revision), None);
    }

    #[test]
    fn item_ids_are_stable_and_scoped() {
        let a = item_id("scope-a", "git:r1");
        assert_eq!(a, item_id("scope-a", "git:r1"));
        assert_ne!(a, item_id("scope-b", "git:r1"));
        assert!(a.starts_with("hist-"));
    }

    #[test]
    fn bounded_excerpt_truncates_with_marker() {
        assert_eq!(bounded_excerpt("short"), "short");
        let long = "x".repeat(MESSAGE_EXCERPT_CHARS + 10);
        let excerpt = bounded_excerpt(&long);
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= MESSAGE_EXCERPT_CHARS + 1);
    }

    #[test]
    fn host_cache_round_trips_and_invalidates() {
        let path = cache_path("roundtrip");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        assert_eq!(cache.get("k"), None);
        cache
            .put(
                "k",
                HostCacheEntry {
                    etag: Some("e1".to_string()),
                    fetched_at_ms: 5,
                    payload: serde_json::json!({ "n": 1 }),
                    next_page: None,
                },
            )
            .unwrap();
        let entry = cache.get("k").expect("entry stored");
        assert_eq!(entry.etag.as_deref(), Some("e1"));
        cache.invalidate().unwrap();
        assert_eq!(cache.get("k"), None);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn verified_association_is_stored_and_counted() {
        let path = cache_path("verified");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::Verified,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            budgets(),
        );
        let mut set = HistorySet {
            head_revision: Some("git:1".to_string()),
            count: 2,
            gaps: Vec::new(),
            items: vec![item(1, Some(10)), item(2, None)],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert_eq!(report.attempted, 1);
        assert_eq!(report.enriched, 1);
        assert!(set.items[0].association.is_some());
        assert!(set.items[0].association.as_ref().unwrap().verified);
        assert!(set.items[1].association.is_none());
        let _ = fs::remove_file(&path);
    }
    #[test]
    fn no_association_keeps_item_unenriched() {
        let path = cache_path("noassoc");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::NoAssoc,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            budgets(),
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 1,
            gaps: Vec::new(),
            items: vec![item(1, Some(10))],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert_eq!(report.no_association, 1);
        assert!(set.items[0].association.is_none());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn permission_denial_fails_closed_and_invalidates_cache() {
        let path = cache_path("denied");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        // Simulate previously cached remote content.
        cache
            .put(
                "pr:owner/repo/9",
                HostCacheEntry {
                    etag: Some("e".to_string()),
                    fetched_at_ms: 1,
                    payload: serde_json::json!({}),
                    next_page: None,
                },
            )
            .unwrap();
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::Denied,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            budgets(),
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 2,
            gaps: Vec::new(),
            items: vec![item(1, Some(9)), item(2, Some(10))],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert!(report.permission_denied);
        assert_eq!(report.attempted, 1, "the pass stops at the denial");
        let probe = HostCache::open(path.clone(), "github");
        assert_eq!(
            probe.get("pr:owner/repo/9"),
            None,
            "cached remote content invalidated"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn rate_limit_and_budget_stop_the_pass() {
        let path = cache_path("rate");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::RateLimited,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            budgets(),
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 2,
            gaps: Vec::new(),
            items: vec![item(1, Some(1)), item(2, Some(2))],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert!(report.rate_limited);
        assert_eq!(report.attempted, 1);
        assert!(
            set.items[1].association.is_none(),
            "later items keep hints only"
        );

        let cache = HostCache::open(path.clone(), "github");
        let zero_budget = EnrichmentBudgets {
            max_prs_per_build: 1,
            max_requests_per_build: 0,
            max_body_chars: 20_000,
        };
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::Verified,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            zero_budget,
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 1,
            gaps: Vec::new(),
            items: vec![item(1, Some(1))],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert!(report.budget_exhausted);
        let _ = fs::remove_file(&path);
    }
    #[test]
    fn pr_budget_caps_attempts_per_build() {
        let path = cache_path("prcap");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        let capped = EnrichmentBudgets {
            max_prs_per_build: 1,
            max_requests_per_build: 50,
            max_body_chars: 20_000,
        };
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::Verified,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            capped,
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 3,
            gaps: Vec::new(),
            items: vec![item(1, Some(1)), item(2, Some(2)), item(3, Some(3))],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert_eq!(report.attempted, 1);
        assert_eq!(report.enriched, 1);
        assert!(set.items[1].association.is_none());
        assert!(set.items[2].association.is_none());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn other_host_failures_count_as_failed_and_continue() {
        let path = cache_path("other");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::Other,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            budgets(),
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 2,
            gaps: Vec::new(),
            items: vec![item(1, Some(1)), item(2, Some(2))],
        };
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert_eq!(report.failed, 2, "other failures never stop the pass");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn enrichment_never_runs_without_a_matching_remote() {
        let path = cache_path("remote");
        let _ = fs::remove_file(&path);
        let cache = HostCache::open(path.clone(), "github");
        let session = EnrichmentSession::new(
            Box::new(FakeProvider {
                behavior: FakeBehavior::Verified,
                calls: std::cell::Cell::new(0),
            }),
            cache,
            budgets(),
        );
        let mut set = HistorySet {
            head_revision: None,
            count: 1,
            gaps: Vec::new(),
            items: vec![item(1, Some(1))],
        };
        let report = session.run(None, &mut set);
        assert_eq!(report.attempted, 0);
        let report = session.run(Some("gitlab.com/owner/repo"), &mut set);
        assert_eq!(report.attempted, 0, "foreign host remotes are not enriched");
        let _ = fs::remove_file(&path);
    }
    #[test]
    fn multi_term_fts_expression_parenthesizes_each_group() {
        let expr = history_fts_match_expression("alpha beta").unwrap();
        // Every per-term group must be parenthesized or the top-level AND
        // would bind only the last group's first field and drop earlier terms.
        assert_eq!(
            expr,
            "(message :\"alpha\" OR revision_id :\"alpha\" OR paths :\"alpha\" OR host :\"alpha\") AND (message :\"beta\" OR revision_id :\"beta\" OR paths :\"beta\" OR host :\"beta\")"
        );
    }

    #[test]
    fn pr_hint_dedupes_non_adjacent_repeats() {
        assert_eq!(
            pr_hint_from_message("see #1 then #2 and later #1 again"),
            vec![1, 2]
        );
    }

    #[test]
    fn enrichment_redacts_secret_shapes_in_host_text() {
        let provider = Box::new(FakeProvider {
            behavior: FakeBehavior::Verified,
            calls: std::cell::Cell::new(0),
        });
        let mut set = HistorySet {
            items: vec![item(1, Some(1))],
            ..Default::default()
        };
        let session = EnrichmentSession::new(
            provider,
            HostCache::open(cache_path("redact"), "github"),
            budgets(),
        );
        let report = session.run(Some("github.com/owner/repo"), &mut set);
        assert_eq!(report.enriched, 1);
        let title = set.items[0]
            .association
            .as_ref()
            .unwrap()
            .title
            .as_deref()
            .unwrap();
        assert!(!title.contains("ghp_"));
        assert!(title.contains("[REDACTED]"));
    }

    #[test]
    fn enrichment_skips_lookalike_remote_hosts() {
        let provider = Box::new(FakeProvider {
            behavior: FakeBehavior::Verified,
            calls: std::cell::Cell::new(0),
        });
        let mut set = HistorySet {
            items: vec![item(1, Some(1))],
            ..Default::default()
        };
        let session = EnrichmentSession::new(
            provider,
            HostCache::open(cache_path("lookalike"), "github"),
            budgets(),
        );
        // A prefix-matching host must never reach the provider.
        let report = session.run(Some("github.acme.corp/owner/repo"), &mut set);
        assert_eq!(report.attempted, 0);
        assert!(set.items[0].association.is_none());
    }

    #[test]
    fn association_fts_text_is_readable_text_only() {
        let association = PrAssociation {
            host: "github".to_string(),
            number: 7,
            title: Some("Add feature".to_string()),
            body: Some("Explain".to_string()),
            state: Some("merged".to_string()),
            url: None,
            updated_at_ms: None,
            fetched_at_ms: 1,
            verified: true,
        };
        assert_eq!(
            association_fts_text(Some(&association)),
            "Add feature\nExplain\nmerged"
        );
        assert_eq!(association_fts_text(None), "");
    }
}
