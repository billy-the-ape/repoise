//! Progress contract for index builds.
//!
//! Core services report structured progress through [`IndexProgressReporter`]
//! while building; no terminal or UI rendering lives in core. Adapters (the
//! CLI today) render events however they choose, or omit progress entirely by
//! passing `None`. Events are emitted in execution order on the building
//! thread, and never alter indexing semantics, budgets, or publication.

use serde::Serialize;

/// Named phases of one index build, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexPhase {
    /// Discovery/inventory: metadata intake, policy decisions, intake bounds
    /// (size, UTF-8, secrets) and snapshot manifest construction.
    Discovery,
    /// Reconciliation against the previous generation: unchanged files and
    /// chunks are reused (not reparsed), changed/new files are reparsed,
    /// deleted or excluded records are tombstoned.
    Reconcile,
    /// Optional embedding pass: content-addressed cache lookup first, then
    /// bounded provider calls for misses.
    Embedding,
    /// Optional history lane: bounded local history collection plus verified
    /// host enrichment when a session is configured.
    History,
    /// Atomic generation publication and state manifest write.
    Publication,
}

impl IndexPhase {
    /// Stable short name for reports and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Reconcile => "reconcile",
            Self::Embedding => "embedding",
            Self::History => "history",
            Self::Publication => "publication",
        }
    }
}

/// Progress reporter callback for one index build.
///
/// Implementations run on the building thread and must be cheap and
/// non-blocking; throttling and rendering are the implementer's
/// responsibility.
pub trait IndexProgressReporter {
    /// A phase has started. `total` is a reliable work denominator when the
    /// phase's work is measurable up front; `None` when it is not (renderers
    /// must not invent a percentage until a denominator arrives via
    /// [`progress`](Self::progress)).
    fn phase_started(&self, phase: IndexPhase, total: Option<u64>);

    /// In-phase progress update: `done` of `total` units complete.
    fn progress(&self, phase: IndexPhase, done: u64, total: u64);

    /// A phase has finished.
    fn phase_finished(&self, phase: IndexPhase);
}

/// A no-op reporter (progress disabled).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullProgressReporter;

impl IndexProgressReporter for NullProgressReporter {
    fn phase_started(&self, _phase: IndexPhase, _total: Option<u64>) {}
    fn progress(&self, _phase: IndexPhase, _done: u64, _total: u64) {}
    fn phase_finished(&self, _phase: IndexPhase) {}
}
