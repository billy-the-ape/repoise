//! Experiment artifact layout and JSON writing.
//!
//! Everything an experiment produces lives under one operator-configured
//! output root, outside the indexed scope:
//!
//! ```text
//! <output root>/<experimentId>/
//!   manifest.json        immutable frozen experiment manifest (written once)
//!   report.json          final summary (written once, at the end)
//!   cache/               REPOISE_CACHE_DIR for trial index builds (shared,
//!                       content-addressed, immutable — declared in manifest)
//!   workspaces/<trialId> disposable per-trial Git worktree
//!   trials/<trialId>/    append-only per-trial record
//!     request.json       the exact AP-03 adapter request sent
//!     trial.json         coordinator trial record (written once per trial)
//!     adapter/
//!       events.ndjson    host event stream (written by the host)
//!       result.json      host result (written by the host)
//!       stderr.log       adapter stderr (written by the host)
//!     host.log           host/bridge diagnostics
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::Error;
use crate::hash;

/// The on-disk layout for one experiment.
#[derive(Clone, Debug)]
pub struct Layout {
    pub experiment_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub workspaces_dir: PathBuf,
    pub trials_dir: PathBuf,
}

impl Layout {
    /// Builds the layout (creating directories); `experiment_id` is part of
    /// the path, so two experiments never share artifact space.
    pub fn create(output_root: &Path, experiment_id: &str) -> crate::Result<Self> {
        let experiment_dir = output_root.join(experiment_id);
        let layout = Self {
            cache_dir: experiment_dir.join("cache"),
            workspaces_dir: experiment_dir.join("workspaces"),
            trials_dir: experiment_dir.join("trials"),
            experiment_dir: experiment_dir.clone(),
        };
        fs::create_dir_all(&layout.experiment_dir).map_err(Error::Io)?;
        fs::create_dir_all(&layout.cache_dir).map_err(Error::Io)?;
        fs::create_dir_all(&layout.workspaces_dir).map_err(Error::Io)?;
        fs::create_dir_all(&layout.trials_dir).map_err(Error::Io)?;
        Ok(layout)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.experiment_dir.join("manifest.json")
    }

    pub fn report_path(&self) -> PathBuf {
        self.experiment_dir.join("report.json")
    }

    pub fn trial_dir(&self, trial_id: &str) -> PathBuf {
        self.trials_dir.join(trial_id)
    }

    pub fn workspace_path(&self, trial_id: &str) -> PathBuf {
        self.workspaces_dir.join(trial_id)
    }

    pub fn adapter_dir(&self, trial_id: &str) -> PathBuf {
        self.trial_dir(trial_id).join("adapter")
    }
}

/// Writes one JSON document atomically (write-then-rename in the same dir).
pub fn write_json(path: &Path, value: &Value) -> crate::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let serialized =
        serde_json::to_string_pretty(value).map_err(|err| Error::Json(err.to_string()))?;
    let tmp = path.with_extension(format!("{}.tmp", path_extension_suffix(path)));
    fs::write(&tmp, format!("{serialized}\n")).map_err(Error::Io)?;
    fs::rename(&tmp, path).map_err(Error::Io)?;
    Ok(())
}

fn path_extension_suffix(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_else(|| "json".into())
}

/// Appends one line (e.g. an adapter stderr chunk or host diagnostic).
pub fn append_line(path: &Path, line: &str) -> crate::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(Error::Io)?;
    use std::io::Write;
    writeln!(file, "{line}").map_err(Error::Io)?;
    Ok(())
}

/// Reads a whole file as a string (missing file is an error).
pub fn read_to_string(path: &Path) -> crate::Result<String> {
    fs::read_to_string(path).map_err(Error::Io)
}

/// Lowercase SHA-256 of a whole file.
pub fn file_hash(path: &Path) -> crate::Result<String> {
    let bytes = fs::read(path).map_err(Error::Io)?;
    Ok(hash::sha256_hex(bytes))
}

/// True when `path` is `root` or anywhere underneath it (canonical forms).
pub fn is_within(root: &Path, path: &Path) -> bool {
    path == root || path.starts_with(root)
}
