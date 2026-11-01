//! Incremental index watcher (card K6, master plan section 12).
//!
//! The watcher observes the declared scope by polling the deterministic
//! inventory (metadata only between scans) and runs the same serialized,
//! incremental index operation used by `repoise index` — no separate build
//! path. Changes are debounced (default 200 ms) and coalesced into one
//! publication per quiescent window; a branch/workspace switch forces a full
//! rescan and reconciliation. The pending change set is bounded; beyond the
//! bound the watcher degrades to a full reparse instead of queuing. The
//! generated cache directory is excluded from the corpus by default, so the
//! watcher's own output never feeds back into the index.
//!
//! When the watch process is given a configured embedding client, each
//! debounced publication runs the same cache-first embedding pass as
//! `repoise index`: unchanged chunk inputs are served from the
//! content-addressed cache without provider calls, and only changed/new
//! inputs are sent under the session's existing timeout, retry, request,
//! input and cancellation budgets. An asynchronous embedding queue is a
//! deferred follow-up; this first stage is synchronous within the
//! publication. Host enrichment remains an explicit operator action. A
//! provider failure, missing configuration, or cancellation during a
//! publication leaves the generation lexically complete with partial or no
//! vector coverage, which `status`/`check` report through their existing
//! coverage semantics. The watcher does not re-run a publication solely to
//! retry failed or pending embedding inputs: once the corpus and
//! configuration match the published generation it goes idle. Failed or
//! pending inputs are retried by the next publication (triggered by a
//! content or configuration change) or by an explicit `repoise index`.
//!
//! Live configuration reload: on every step the watcher re-resolves the
//! effective configuration from its [`ConfigSources`] (the resolved
//! committed and local config files — explicit CLI paths when given — plus
//! the process's fixed CLI overrides) and validates it before any scan.
//! A change is applied as one coherent configuration revision at the next
//! publication boundary: the include/exclude/role policy and the embedding
//! scope/profile/client move together, never mixed across a generation.
//! Reloadable settings are the corpus policy (preset, include/exclude,
//! max file bytes, role rules, Git-ignore inheritance, search fusion) and
//! the embedding scope/profile/budgets. Settings bound to the running
//! process (`cacheDir`, `sharedCache`, the history lane) and any revision
//! that fails parsing, validation, or embedding-client construction are
//! rejected with a sanitized, throttled [`WatchStep::ConfigIssue`] warning;
//! the last complete published generation stays served and no further
//! generation is published under the obsolete configuration. Correcting the
//! configuration recovers the watcher automatically on the next step.
//! Creation, deletion, and branch-switch changes of the config files are
//! detected because the files are monitored directly, even when the policy
//! excludes them from the corpus. Unchanged effective settings (same
//! fingerprint) never trigger a rebuild.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::Result;
use crate::adapter::{SnapshotMode, SourceAdapter};
use crate::cache::CachePaths;
use crate::config::{CliOverrides, EffectiveConfig, EmbeddingConfig};
use crate::discovery;
use crate::indexing::{self, IndexRequest};
use crate::store::{self, Store};

/// Default poll interval between scans.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);
/// Default debounce before a detected change is published.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(200);
/// Default bound on the pending changed-path set before degrading to full.
pub const DEFAULT_MAX_PENDING_PATHS: usize = 4096;
/// Default suppression window for repeated identical configuration warnings.
pub const DEFAULT_CONFIG_WARNING_INTERVAL: Duration = Duration::from_secs(30);

/// Watcher tuning.
#[derive(Clone, Copy, Debug)]
pub struct WatchOptions {
    /// Poll interval between scans.
    pub interval: Duration,
    /// Debounce before a detected change is published.
    pub debounce: Duration,
    /// Bound on the pending changed-path set (beyond it: full reparse).
    pub max_pending_paths: usize,
    /// Suppression window for repeated identical configuration warnings.
    pub config_warning_interval: Duration,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            debounce: DEFAULT_DEBOUNCE,
            max_pending_paths: DEFAULT_MAX_PENDING_PATHS,
            config_warning_interval: DEFAULT_CONFIG_WARNING_INTERVAL,
        }
    }
}

/// The resolved configuration sources a running watch re-resolves on every
/// step: the committed config file, the local override file, and the
/// fixed CLI overrides of the process (precedence CLI > local > committed >
/// default). The files are monitored directly, so creation, deletion, and
/// branch-switch changes are detected even when the policy excludes them
/// from the corpus.
#[derive(Clone, Debug)]
pub struct ConfigSources {
    /// Committed config path under watch (explicit CLI path or repository
    /// default).
    pub committed: PathBuf,
    /// Local override path under watch (explicit CLI path or repository
    /// default).
    pub local: PathBuf,
    /// CLI overrides fixed for the watch process (highest precedence).
    pub cli: CliOverrides,
}

impl ConfigSources {
    /// The repository default configuration sources.
    pub fn default_for(root: &std::path::Path) -> Self {
        Self {
            committed: root.join(crate::CONFIG_FILENAME),
            local: root.join(crate::LOCAL_CONFIG_FILENAME),
            cli: CliOverrides::default(),
        }
    }
}

/// Rebuilds the embedding client for one effective configuration revision.
/// The CLI supplies the concrete construction (provider transport,
/// credential resolution, cancellation); the watcher only asks for a
/// replacement when the effective embedding settings move, so the
/// service/adapter boundary stays intact.
pub type EmbeddingClientFactory =
    dyn Fn(&EffectiveConfig) -> Result<Option<crate::embed::EmbeddingClient>>;

/// Why a detected configuration revision cannot be applied in-process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigIssueKind {
    /// The revision failed to parse/resolve, failed validation, or its
    /// embedding client could not be built.
    Invalid,
    /// The revision changes settings bound to the running process
    /// (`cacheDir`, `sharedCache`, the history lane, or the embedding
    /// client when this watcher cannot rebuild it in-process).
    RestartRequired,
}

/// A configuration revision detected during a running watch that was not
/// applied. The last complete published generation remains served and no
/// further generation is published under the obsolete configuration until
/// the revision is corrected.
#[derive(Clone, Debug, Serialize)]
pub struct WatchConfigIssue {
    /// Sanitized, actionable message: configuration bytes and credentials
    /// never appear in it.
    pub message: String,
    /// The class of the issue.
    pub kind: ConfigIssueKind,
}

/// Outcome of one watcher step that published a new generation.
#[derive(Clone, Debug, Serialize)]
pub struct WatchPublication {
    /// Published generation id.
    pub generation_id: i64,
    /// Changed paths that triggered the publication (empty when a full
    /// reparse was run); repository-relative with forward-slash separators.
    pub changed_paths: Vec<String>,
    /// Files reparsed by the publication.
    pub files_reparsed: usize,
    /// Whether a full reparse (not incremental) was run.
    pub force_full: bool,
    /// Branch/workspace switch detected since the previous scan.
    pub branch_switch: bool,
    /// Effective configuration fingerprint changed since the published
    /// generation (a configuration-driven publication; false for the
    /// watcher's first publication).
    pub config_change: bool,
    /// Vector records published in this generation (0 when lexical-only).
    pub vectors_total: usize,
    /// Embedding session stats for this publication, when an embedding
    /// session was configured for the watch process.
    pub embedding: Option<crate::embed::EmbeddingStats>,
    /// Publication duration in milliseconds.
    pub duration_ms: u64,
}

/// Outcome of one watcher step.
#[derive(Debug)]
pub enum WatchStep {
    /// No changes since the last scan.
    Idle,
    /// A new generation was published.
    Published(WatchPublication),
    /// A change was detected but the debounce window was cancelled first.
    Cancelled,
    /// A configuration revision was detected but cannot be applied in this
    /// process (invalid, or restart-required). No generation was published;
    /// the last complete published generation remains served.
    ConfigIssue(WatchConfigIssue),
}

/// The incremental watcher for one declared scope.
pub struct Watcher {
    adapter: Box<dyn SourceAdapter>,
    mode: SnapshotMode,
    eff: EffectiveConfig,
    store: Store,
    cache: CachePaths,
    options: WatchOptions,
    embed_client: Option<crate::embed::EmbeddingClient>,
    config_sources: ConfigSources,
    client_factory: Option<Box<EmbeddingClientFactory>>,
    /// Identity of the embedding settings the current client was built
    /// from; a reload that moves this rebuilds the client before any
    /// publication.
    client_revision: String,
    repo_id: String,
    worktree_id: String,
    last_branch: Option<String>,
    published_manifest: Option<String>,
    published_fingerprint: Option<String>,
    /// Last emitted configuration issue (message, time) for throttling.
    last_config_issue: Option<(String, Instant)>,
}

impl Watcher {
    /// Creates a watcher for one declared scope.
    ///
    /// `embed_client` is the operator-configured embedding session, built
    /// once for the watch process and passed into every debounced
    /// publication through the shared index path (cache-first; unchanged
    /// chunk inputs never reach the provider). `None` keeps publications
    /// lexical-only. Without [`with_config_sources`], the watcher watches
    /// the repository's default committed/local config paths with no CLI
    /// overrides and no client factory (embedding settings cannot be
    /// rebuilt in-process on that path).
    pub fn new(
        adapter: Box<dyn SourceAdapter>,
        mode: SnapshotMode,
        eff: EffectiveConfig,
        store: Store,
        cache: CachePaths,
        options: WatchOptions,
        embed_client: Option<crate::embed::EmbeddingClient>,
    ) -> Result<Self> {
        let root = adapter.canonical_root()?;
        let record = crate::provenance::RepositoryRecord::new(
            adapter.kind(),
            &root,
            adapter.remote_identity()?,
        );
        let worktree_id = crate::cache::worktree_id(&record.repo_id, &root, mode);
        let conn = store.open()?;
        let published = store::current_generation(&conn, &record.repo_id, &worktree_id)?;
        let client_revision = embedding_revision(&eff);
        Ok(Self {
            adapter,
            mode,
            eff,
            store,
            cache,
            options,
            embed_client,
            config_sources: ConfigSources::default_for(&root),
            client_factory: None,
            client_revision,
            repo_id: record.repo_id,
            worktree_id,
            last_branch: None,
            published_manifest: published.as_ref().map(|m| m.manifest_hash.clone()),
            published_fingerprint: published.as_ref().map(|m| m.config_fingerprint.clone()),
            last_config_issue: None,
        })
    }

    /// Replaces the configuration sources (and, when provided, the
    /// embedding client factory) used to live-reload the effective
    /// configuration on every step. The CLI passes the resolved explicit
    /// config paths, its fixed overrides, and a factory that rebuilds the
    /// embedding session through the shared configuration helpers.
    pub fn with_config_sources(
        mut self,
        sources: ConfigSources,
        client_factory: Option<Box<EmbeddingClientFactory>>,
    ) -> Self {
        self.config_sources = sources;
        self.client_factory = client_factory;
        self
    }
    /// Runs one scan-and-reconcile iteration, publishing at most one new
    /// generation. Serializes with itself through the caller's loop; the
    /// `cancel` flag is checked throughout the debounce and honored before
    /// any publication starts.
    pub fn step(&mut self, cancel: &AtomicBool) -> Result<WatchStep> {
        let started = Instant::now();
        // Re-resolve and validate the effective configuration before any
        // scan: a rejected revision freezes the watcher on its last good
        // configuration (throttled warning), an applied revision moves the
        // policy and embedding client together.
        if let Some(issue) = self.reload_config().issue() {
            return Ok(self.throttle_config_issue(issue));
        }
        let inventory = discovery::inventory(self.adapter.as_ref(), self.mode, &self.eff, None)?;
        let live_branch = inventory.snapshot.branch.clone();
        let branch_switch = self.branch_changed(&live_branch);

        if !self.needs_publish(&inventory) {
            self.last_branch = live_branch;
            return Ok(WatchStep::Idle);
        }

        // Debounce: give in-flight writes time to settle, checking cancel.
        let deadline = started + self.options.debounce;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.last_branch = live_branch;
                return Ok(WatchStep::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(20)));
        }
        if cancel.load(Ordering::SeqCst) {
            self.last_branch = live_branch;
            return Ok(WatchStep::Cancelled);
        }

        // Coalesce: re-resolve the configuration and rescan after the
        // debounce and reconcile against the freshest state, not the
        // pre-debounce scan (a config edit during the debounce window is
        // applied to this publication, not the next one).
        if let Some(issue) = self.reload_config().issue() {
            return Ok(self.throttle_config_issue(issue));
        }
        let inventory = discovery::inventory(self.adapter.as_ref(), self.mode, &self.eff, None)?;
        let live_branch = inventory.snapshot.branch.clone();
        if !self.needs_publish(&inventory) {
            self.last_branch = live_branch;
            return Ok(WatchStep::Idle);
        }
        let branch_switch_now = branch_switch || self.branch_changed(&live_branch);

        // Bounded pending set: paths changed versus the published generation.
        let changed_paths = self.changed_paths(&inventory)?;
        let force_full = branch_switch_now || changed_paths.len() > self.options.max_pending_paths;
        // Configuration-driven publication: the effective fingerprint moved
        // away from the published one (not applicable on the first build).
        let config_change = self
            .published_fingerprint
            .as_deref()
            .is_some_and(|prev| prev != self.eff.fingerprint());

        let built = Instant::now();
        let request = IndexRequest {
            affected_paths: Some(changed_paths.iter().map(PathBuf::from).collect()),
            force_full,
        };
        let outcome = indexing::index(
            self.adapter.as_ref(),
            self.mode,
            &self.eff,
            &self.store,
            &self.cache,
            &request,
            self.embed_client.as_ref(),
            None,
            None,
        )?;
        self.last_branch = live_branch;
        self.published_manifest = Some(outcome.manifest_hash.clone());
        self.published_fingerprint = Some(self.eff.fingerprint());
        Ok(WatchStep::Published(WatchPublication {
            generation_id: outcome.generation_id,
            changed_paths: if force_full {
                Vec::new()
            } else {
                changed_paths
            },
            files_reparsed: outcome.files_reparsed,
            force_full,
            branch_switch: branch_switch_now,
            config_change,
            vectors_total: outcome.vectors_total,
            embedding: outcome.embedding,
            duration_ms: built.elapsed().as_millis() as u64,
        }))
    }

    /// Whether the live snapshot differs from the published generation.
    fn needs_publish(&self, inventory: &crate::discovery::Inventory) -> bool {
        match (
            self.published_manifest.as_ref(),
            self.published_fingerprint.as_ref(),
        ) {
            (Some(manifest), Some(fp)) => {
                manifest != &inventory.manifest.manifest_hash || fp != &self.eff.fingerprint()
            }
            _ => true,
        }
    }

    /// Re-resolves the effective configuration from the monitored sources
    /// and applies it as one coherent revision when it is safe to do so:
    /// the corpus policy and the embedding scope/profile/client move
    /// together, and the embedding client is rebuilt before any publication
    /// when the effective embedding settings moved.
    fn reload_config(&mut self) -> ConfigReload {
        let sources = &self.config_sources;
        let candidate = match EffectiveConfig::resolve_files(
            &sources.committed,
            &sources.local,
            &sources.cli,
        ) {
            Ok(candidate) => candidate,
            Err(err) => {
                return ConfigReload::Issue(WatchConfigIssue {
                    kind: ConfigIssueKind::Invalid,
                    // `err` is structural (path plus parse/schema detail); it
                    // never carries file bytes or credentials.
                    message: format!(
                        "watch configuration reload failed: {err}; the last published generation is retained; fix the configuration to resume publishing"
                    ),
                });
            }
        };
        let problems = candidate.validate();
        if !problems.is_empty() {
            return ConfigReload::Issue(WatchConfigIssue {
                kind: ConfigIssueKind::Invalid,
                message: format!(
                    "watch configuration reload rejected: {} ({} problem(s) in total); the last published generation is retained; fix the configuration to resume publishing",
                    problems.first().expect("problems are non-empty"),
                    problems.len()
                ),
            });
        }
        // Settings bound to the running process (the store location, the
        // worktree family store, and the history lane) cannot move
        // in-process: the whole revision is rejected rather than mixing old
        // and new settings in one generation, and the warning is reported
        // even when the corpus fingerprint is unchanged (never silently
        // retained).
        let mut restart_settings: Vec<&str> = Vec::new();
        if candidate.cache_dir != self.eff.cache_dir {
            restart_settings.push("cacheDir");
        }
        if candidate.shared_cache != self.eff.shared_cache {
            restart_settings.push("sharedCache");
        }
        if candidate.history != self.eff.history {
            restart_settings.push("history");
        }
        if !restart_settings.is_empty() {
            return ConfigReload::Issue(WatchConfigIssue {
                kind: ConfigIssueKind::RestartRequired,
                message: format!(
                    "watch configuration change to {} requires restarting the watch process; the running watch keeps its startup settings and publishes nothing until restarted",
                    restart_settings.join(", ")
                ),
            });
        }
        // Unchanged effective settings: the corpus fingerprint covers the
        // corpus and embedding settings; the MCP refresh flag is the only
        // effective setting outside it. Equal settings mean no rebuild.
        if candidate.fingerprint() == self.eff.fingerprint()
            && candidate.mcp_refresh == self.eff.mcp_refresh
        {
            self.last_config_issue = None;
            return ConfigReload::Unchanged;
        }
        // The embedding client must match the new revision before any
        // publication; a model/profile/input-template change never reuses
        // incompatible cached vectors.
        let revision = embedding_revision(&candidate);
        if revision != self.client_revision {
            match &self.client_factory {
                Some(factory) => match factory(&candidate) {
                    Ok(client) => {
                        self.embed_client = client;
                        self.client_revision = revision;
                    }
                    Err(_) => {
                        // Sanitized: the underlying failure can carry an
                        // endpoint or credential reference from the
                        // environment; it is not echoed here.
                        return ConfigReload::Issue(WatchConfigIssue {
                            kind: ConfigIssueKind::Invalid,
                            message: "watch could not build the embedding client for the new configuration; the previous configuration is retained; fix the configuration (run `repoise doctor` for details) to resume publishing".to_string(),
                        });
                    }
                },
                None => {
                    // No factory (plain `Watcher::new` consumers): the only
                    // in-process client operation is dropping a client for a
                    // lexical-only revision; building a client is impossible.
                    let needs_client = candidate.embedding.as_ref().is_some_and(|settings| {
                        crate::config::profile_from_config(settings).is_some()
                    });
                    if needs_client {
                        return ConfigReload::Issue(WatchConfigIssue {
                            kind: ConfigIssueKind::RestartRequired,
                            message: "watch embedding settings changed to require an embedding client this watcher cannot build in-process; restart the watch to apply the new embedding configuration".to_string(),
                        });
                    }
                    self.embed_client = None;
                }
            }
        }
        self.eff = candidate;
        self.last_config_issue = None;
        ConfigReload::Applied
    }

    /// Throttles repeated identical configuration issues: within the
    /// configured window the same message suppresses to `Idle` (no tight
    /// warning/rebuild loop), and a changed message or an elapsed window
    /// emits again.
    fn throttle_config_issue(&mut self, issue: WatchConfigIssue) -> WatchStep {
        let suppressed = self
            .last_config_issue
            .as_ref()
            .is_some_and(|(previous, at)| {
                previous == &issue.message && at.elapsed() < self.options.config_warning_interval
            });
        if suppressed {
            return WatchStep::Idle;
        }
        self.last_config_issue = Some((issue.message.clone(), Instant::now()));
        WatchStep::ConfigIssue(issue)
    }

    /// True when the live branch differs from the previously observed one.
    fn branch_changed(&self, live_branch: &Option<String>) -> bool {
        match (self.last_branch.as_ref(), live_branch.as_ref()) {
            (Some(prev), Some(live)) => prev != live,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// Paths added, removed or changed versus the published generation.
    fn changed_paths(&self, inventory: &crate::discovery::Inventory) -> Result<Vec<String>> {
        let conn = self.store.open()?;
        let prev = store::load_generation(&conn, &self.repo_id, &self.worktree_id)?;
        let prev_set: std::collections::HashMap<&std::path::Path, &str> = prev
            .as_ref()
            .map(|prev| {
                prev.files
                    .iter()
                    .map(|file| (std::path::Path::new(&file.path), file.content_hash.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        let live_set: std::collections::HashMap<&std::path::Path, &str> = inventory
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.content_hash.as_str()))
            .collect();
        let mut changed: Vec<String> = Vec::new();
        for file in &inventory.files {
            let path: &std::path::Path = file.path.as_path();
            match prev_set.get(path) {
                Some(prev_hash) if *prev_hash == file.content_hash => {}
                _ => changed.push(normalized_rel_path(path)),
            }
        }
        for path in prev_set.keys() {
            if !live_set.contains_key(path) {
                changed.push(normalized_rel_path(path));
            }
        }
        changed.sort();
        changed.dedup();
        Ok(changed)
    }
}

/// Outcome of one configuration reload.
enum ConfigReload {
    /// The effective configuration is unchanged (same fingerprint).
    Unchanged,
    /// A new revision was applied coherently (policy + embedding client).
    Applied,
    /// The revision was rejected; the previous configuration is retained.
    Issue(WatchConfigIssue),
}

impl ConfigReload {
    /// The rejected revision, when one was detected.
    fn issue(self) -> Option<WatchConfigIssue> {
        match self {
            ConfigReload::Issue(issue) => Some(issue),
            ConfigReload::Unchanged | ConfigReload::Applied => None,
        }
    }
}

/// Stable identity of the embedding settings a client was built from: the
/// full embedding configuration (provider, model, dimensions, scope, input
/// template, endpoint/credential references, budgets) plus the effective
/// embedding scope and input profile. Any difference means a new client and
/// a new profile fingerprint, so cached vectors from the old revision are
/// never reused.
fn embedding_revision(eff: &EffectiveConfig) -> String {
    let default_settings = EmbeddingConfig::default();
    let settings: &EmbeddingConfig = eff.embedding.as_ref().unwrap_or(&default_settings);
    let mut key = serde_json::to_string(settings).unwrap_or_default();
    key.push('\n');
    key.push_str(eff.embedding_scope.as_str());
    key.push('\n');
    key.push_str(eff.embedding_input_profile.identity().as_str());
    crate::hash::sha256_hex(key)
}

/// Renders a repository-relative path with forward-slash separators on every
/// platform (repository paths in the index and CLI output are always
/// forward-slash relative).
fn normalized_rel_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
