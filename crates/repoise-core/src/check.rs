//! Offline coverage check with an opt-in freshness gate (card K6, master
//! plan section 8).
//!
//! `check` reuses the operator status view (metadata only, no provider
//! calls) and adds a stable outcome category for automation:
//! `ok`, `missing`, `stale`, `coverage-not-met` or `error`. Coverage is the
//! scope-aware embedding requirement: the served generation is judged
//! against its own persisted embedding scope/profile (eligible chunks are
//! the docs corpus plus code when the scope is `all`; config and other
//! out-of-scope chunks are lexical-only and never pending). A hybrid preset
//! or configured embedding requires the served generation to carry a
//! profile (`profile-unavailable` otherwise), a non-empty eligible corpus
//! when it wants hybrid retrieval, and no pending eligible chunks
//! (`partial`). A stale index is only a failing outcome when
//! `CheckOptions::require_fresh` is set (`check --fresh`).
//!
//! Freshness carries the same meaning as `status`: the index is fresh when
//! its published content manifest and effective-config fingerprint match
//! the live snapshot for the declared scope. Git working-tree dirtiness is
//! reported in the status view but never makes the index stale; the
//! benchmark preflight's clean-committed-snapshot rule is a separate,
//! independent requirement (see `bench::preflight`).

use std::path::Path;

use serde::Serialize;

use crate::Result;
use crate::adapter::{SnapshotMode, SourceAdapter};
use crate::cache::CachePaths;
use crate::config::EffectiveConfig;
use crate::store::Store;

/// Check options.
#[derive(Clone, Copy, Debug)]
pub struct CheckOptions {
    /// Require the index to be fresh (used by `check --fresh`).
    pub require_fresh: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            require_fresh: true,
        }
    }
}

/// Stable outcome category for automation (serialized as kebab-case).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckCategory {
    /// Index exists, is fresh, and configured coverage is met.
    Ok,
    /// No published index for the scope.
    Missing,
    /// The published index no longer matches the live snapshot/config.
    Stale,
    /// The served generation does not meet the configured coverage.
    CoverageNotMet,
}

/// Machine-readable check outcome.
#[derive(Clone, Debug, Serialize)]
pub struct CheckResult {
    /// Stable outcome category.
    pub category: CheckCategory,
    /// Why the check did not pass (empty when `ok`).
    pub reasons: Vec<String>,
    /// Whether the index is fresh (same judgment as `status`).
    pub fresh: bool,
    /// Whether the configured coverage is met.
    pub coverage_met: bool,
    /// Full operator status view (same schema as `status --json`).
    pub status: crate::status::StatusView,
}

/// Runs the offline check for one declared scope.
pub fn check(
    adapter: &dyn SourceAdapter,
    mode: SnapshotMode,
    eff: &EffectiveConfig,
    config_file: Option<&Path>,
    store: &Store,
    cache: &CachePaths,
    options: &CheckOptions,
) -> Result<CheckResult> {
    let status = crate::status::status(adapter, mode, eff, config_file, store, cache)?;
    let mut reasons: Vec<String> = Vec::new();

    let freshness = status.freshness.status.as_str();
    let fresh = freshness == "fresh";
    if !fresh && options.require_fresh {
        reasons.extend(status.freshness.reasons.iter().cloned());
    }

    // Scope-aware embedding coverage for the served generation. The state is
    // computed from the generation's persisted scope/profile (see
    // `status`); today's config only decides whether vector work was
    // requested at all.
    let coverage_met = match &status.index {
        Some(index) => {
            let mut met = true;
            match index.embedding_coverage.as_str() {
                // A configured embedding profile is missing from the served
                // generation (it was built lexical-only; a rebuild is needed,
                // not pending embedding work).
                "profile-unavailable" => {
                    if eff.preset == crate::config::Preset::Hybrid {
                        reasons.push(
                            "hybrid preset requires vectors in the served generation (none \
                             stored; the generation was built without an embedding profile, \
                             rebuild the index)"
                                .to_string(),
                        );
                    } else {
                        reasons.push(
                            "an embedding profile is configured but the served generation \
                             has no stored vectors (built lexical-only); rebuild the index"
                                .to_string(),
                        );
                    }
                    met = false;
                }
                // A stored profile over an empty eligible corpus: nothing is
                // pending, but hybrid retrieval cannot run either.
                "empty"
                    if eff.preset == crate::config::Preset::Hybrid || eff.embedding.is_some() =>
                {
                    reasons.push(
                        "the stored embedding profile covers an empty eligible corpus \
                         (no chunks under the embedding scope); hybrid retrieval cannot \
                         run and no vector work is pending"
                            .to_string(),
                    );
                    met = false;
                }
                // Eligible chunks still missing vectors under the stored
                // profile (failed or pending embedding work).
                "partial" => {
                    reasons.push(format!(
                        "{} eligible chunk(s) await embeddings under the stored embedding \
                         scope ({}) — {}/{} eligible chunks covered",
                        index.vectors_pending,
                        index.embedding_scope,
                        index.vectors,
                        index.eligible_chunks
                    ));
                    met = false;
                }
                _ => {}
            }
            met
        }
        None => true,
    };

    let category = if status.index.is_none() {
        CheckCategory::Missing
    } else if !fresh && options.require_fresh {
        CheckCategory::Stale
    } else if !coverage_met {
        CheckCategory::CoverageNotMet
    } else {
        CheckCategory::Ok
    };

    Ok(CheckResult {
        category,
        reasons,
        fresh,
        coverage_met,
        status,
    })
}
