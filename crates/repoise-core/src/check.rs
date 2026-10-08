//! Offline freshness and coverage check (card K6, master plan section 8).
//!
//! `check` reuses the operator status view (metadata only, no provider
//! calls) and adds a stable outcome category for automation:
//! `ok`, `missing`, `stale`, `coverage-not-met` or `error`. Coverage is the
//! configured embedding requirement: the hybrid preset requires vectors in
//! the served generation, and any configured embedding scope must not leave
//! pending vectors after a build. Freshness uses the same honest judgment as
//! `status` (manifest, config fingerprint, working-tree dirt).

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
    _options: &CheckOptions,
) -> Result<CheckResult> {
    let status = crate::status::status(adapter, mode, eff, config_file, store, cache)?;
    let mut reasons: Vec<String> = Vec::new();

    let freshness = status.freshness.status.as_str();
    let fresh = freshness == "fresh";
    if !fresh {
        reasons.extend(status.freshness.reasons.iter().cloned());
    }

    // Configured embedding coverage for the served generation.
    let coverage_met = match &status.index {
        Some(index) => {
            let mut met = true;
            if eff.preset == crate::config::Preset::Hybrid && index.vectors == 0 {
                reasons.push(
                    "hybrid preset requires vectors in the served generation (none stored)"
                        .to_string(),
                );
                met = false;
            } else if eff.embedding.is_some() && index.vectors_pending > 0 {
                reasons.push(format!(
                    "{} chunk(s) await embeddings for the configured embedding scope",
                    index.vectors_pending
                ));
                met = false;
            }
            met
        }
        None => true,
    };

    let category = if !fresh {
        if status.index.is_none() {
            CheckCategory::Missing
        } else {
            CheckCategory::Stale
        }
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
