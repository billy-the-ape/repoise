//! Git source adapter.
//!
//! Git is invoked with argument arrays (never shell strings) and NUL-delimited
//! output. Committed mode reads blobs from one resolved commit, so hashing
//! never touches mutable files. Detached HEAD is an explicit, supported state.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::adapter::{
    AdapterError, Capabilities, Capability, Revision, RevisionId, SnapshotMode, SourceAdapter,
    SourceEntry, SourceKind,
    filesystem::{read_within_root, scan_directory},
    normalize_relative,
};
use crate::hash;
use crate::provenance::sanitize_remote_identity;

/// Adapter over a Git repository.
#[derive(Clone, Debug)]
pub struct GitAdapter {
    root: PathBuf,
    canonical_root: PathBuf,
}

impl GitAdapter {
    /// Opens a Git adapter rooted at `root`; fails if it is not a Git repository.
    pub fn new(root: &Path) -> Result<Self, AdapterError> {
        let canonical_root = root.canonicalize().map_err(AdapterError::Io)?;
        let git_marker = canonical_root.join(".git");
        if !git_marker.exists() {
            return Err(AdapterError::NotARepository(root.to_path_buf()));
        }
        Ok(Self {
            root: root.to_path_buf(),
            canonical_root,
        })
    }

    /// Runs `git <args>` in the repository, returning stdout on success.
    pub fn run_git(&self, args: &[&str]) -> Result<String, AdapterError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .map_err(AdapterError::Io)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AdapterError::Other(format!(
                "git {args:?} failed: {stderr}"
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Counts modified/untracked entries via `git status --porcelain`.
    pub fn dirty_count(&self) -> Result<usize, AdapterError> {
        let out = self.run_git(&["status", "--porcelain"])?;
        Ok(out.lines().filter(|line| !line.trim().is_empty()).count())
    }

    /// Resolves `git:<object id>` style revisions to a commit id.
    pub fn resolve_commit(&self, revision_id: &RevisionId) -> Result<String, AdapterError> {
        let Some(object) = revision_id.as_str().strip_prefix("git:") else {
            return Err(AdapterError::MissingRevision(revision_id.clone()));
        };
        match self.run_git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{object}^{{commit}}"),
        ]) {
            Ok(commit) if !commit.trim().is_empty() => Ok(commit.trim().to_string()),
            _ => Err(AdapterError::MissingRevision(revision_id.clone())),
        }
    }

    /// Exact bytes of the file at `commit:path`, or `MissingFile`.
    fn blob_at(&self, commit: &str, relative: &Path) -> Result<Vec<u8>, AdapterError> {
        let relative = normalize_relative(relative)?;
        let spec = super::to_posix(&relative);
        let blob = match self.run_git(&["rev-parse", "--verify", &format!("{commit}:{spec}")]) {
            Ok(value) => value,
            Err(_) => return Err(AdapterError::MissingFile(relative)),
        };
        let blob = blob.trim().to_string();
        self.run_git(&["cat-file", "blob", &blob])
            .map(|text| text.into_bytes())
    }

    /// Lists `(path, blob sha)` for every blob in the tree (NUL-safe).
    fn tree_blobs(&self, commit: &str) -> Result<Vec<(String, String)>, AdapterError> {
        let output = self.run_git(&["ls-tree", "-r", "-z", commit])?;
        let mut blobs = Vec::new();
        for entry in output.split('\0') {
            if entry.is_empty() {
                continue;
            }
            let Some((metadata, path)) = entry.split_once('\t') else {
                continue;
            };
            // metadata: "<mode> SP <type> SP <sha>"
            let mut parts = metadata.split(' ');
            let _mode = parts.next();
            let object_type = parts.next();
            let sha = parts.next();
            if object_type == Some("blob")
                && let Some(sha) = sha
            {
                blobs.push((path.to_string(), sha.to_string()));
            }
        }
        Ok(blobs)
    }

    /// Branch of the current HEAD; `None` when detached (explicit, not an error).
    pub fn current_branch(&self) -> Option<String> {
        self.run_git(&["symbolic-ref", "--short", "HEAD"])
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    /// Nearest name-rev of a commit; `None` when it cannot be named.
    fn branch_for_commit(&self, commit: &str) -> Option<String> {
        self.run_git(&["name-rev", "--name-only", commit])
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
}

/// Bounded diff hunk descriptors for one commit (card K5): parse diff
/// headers only — never patch text.
///
/// Hunk bases are computed with `git diff <first-parent> <commit>` (or
/// `git diff --root` for the root commit): `git show` renders *combined*
/// diffs for merge commits, which contain no per-file hunks at all, and
/// `--first-parent` does not change that. The parent-base tree diff is
/// exactly what `--name-only --first-parent` uses for its path lists.
fn hunk_descriptors(
    adapter: &GitAdapter,
    commit: &str,
    first_parent: Option<&str>,
    max_hunks: u32,
) -> Result<(Vec<crate::history::HunkDescriptor>, u32), AdapterError> {
    use crate::history::HunkDescriptor;
    let args: Vec<&str> = match first_parent {
        Some(parent) => vec![
            "-c",
            "core.quotepath=false",
            "diff",
            "--unified=0",
            "--no-prefix",
            parent,
            commit,
        ],
        None => vec![
            "-c",
            "core.quotepath=false",
            "diff",
            "--root",
            "--unified=0",
            "--no-prefix",
            commit,
        ],
    };
    let out = adapter.run_git(&args)?;
    let mut hunks: Vec<HunkDescriptor> = Vec::new();
    let mut truncated: u32 = 0;
    let mut current_path = String::new();
    for line in out.lines() {
        if let Some(old) = line.strip_prefix("--- ") {
            // Deletions: the hunk keeps the old path.
            if old != "/dev/null" {
                current_path = old.to_string();
            }
            continue;
        }
        if let Some(new) = line.strip_prefix("+++ ") {
            if new != "/dev/null" {
                current_path = new.to_string();
            }
            continue;
        }
        let Some(body) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some((ranges, context)) = body.split_once(" @@") else {
            continue;
        };
        let mut range_parts = ranges.split_whitespace();
        let Some(old_range) = range_parts.next() else {
            continue;
        };
        let Some(new_range) = range_parts.next() else {
            continue;
        };
        let parse_range = |range: &str| -> (u32, u32) {
            let body = range.trim_start_matches('-').trim_start_matches('+');
            let mut parts = body.splitn(2, ',');
            let start = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let lines = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            (start, lines)
        };
        let (old_start, old_lines) = parse_range(old_range);
        let (new_start, new_lines) = parse_range(new_range);
        if hunks.len() >= max_hunks as usize {
            truncated += 1;
            continue;
        }
        let context: String = context.trim().chars().take(80).collect();
        if !current_path.is_empty() {
            hunks.push(HunkDescriptor {
                path: current_path.clone(),
                old_start,
                old_lines,
                new_start,
                new_lines,
                context,
            });
        }
    }
    Ok((hunks, truncated))
}

/// The mainline commit list (first-parent from HEAD, newest first) under the
/// horizon. `rev-list` output is git-generated (SHAs only), so it needs no
/// framing — the fragile part (commit fields and repo-controlled paths) is
/// isolated in the separately parsed NUL-framed log below.
fn list_mainline(adapter: &GitAdapter, horizon: u32) -> Result<Vec<String>, AdapterError> {
    let out = adapter.run_git(&[
        "rev-list",
        "--first-parent",
        "--max-count",
        &horizon.to_string(),
        "HEAD",
        "-z",
    ])?;
    Ok(out
        .split('\0')
        .map(str::trim)
        .filter(|sha| !sha.is_empty())
        .map(str::to_string)
        .collect())
}

impl SourceAdapter for GitAdapter {
    fn kind(&self) -> SourceKind {
        SourceKind::Git
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn canonical_root(&self) -> Result<PathBuf, AdapterError> {
        Ok(self.canonical_root.clone())
    }

    fn history_provider(&self) -> Option<&dyn crate::history::HistoryProvider> {
        Some(self)
    }

    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        mode: SnapshotMode,
    ) -> Result<Revision, AdapterError> {
        if mode == SnapshotMode::PlainDirectory {
            return Err(AdapterError::UnsupportedOperation {
                operation: "plain directory snapshot",
                capability: Capability::AtomicSnapshots,
            });
        }
        let (commit, branch) = match requested {
            Some(revision_id) => {
                let commit = self.resolve_commit(revision_id)?;
                let branch = self.branch_for_commit(&commit);
                (commit, branch)
            }
            None => {
                let head = self
                    .run_git(&["rev-parse", "HEAD"])
                    .map_err(|_| AdapterError::NoRevision("repository has no HEAD".into()))?
                    .trim()
                    .to_string();
                (head, self.current_branch())
            }
        };
        Ok(Revision {
            id: RevisionId::new(format!("git:{commit}")),
            branch,
        })
    }
    fn enumerate(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        match mode {
            SnapshotMode::Committed => {
                let commit = self.resolve_commit(&revision.id)?;
                let mut entries = Vec::new();
                for (path, blob_sha) in self.tree_blobs(&commit)? {
                    let bytes = self.run_git(&["cat-file", "blob", &blob_sha])?.into_bytes();
                    entries.push(SourceEntry {
                        path: PathBuf::from(path),
                        content_hash: hash::sha256_hex(&bytes),
                        size: bytes.len() as u64,
                    });
                }
                entries.sort_by(|a, b| a.path.cmp(&b.path));
                Ok(entries)
            }
            SnapshotMode::WorkingTree => scan_directory(&self.canonical_root, &self.canonical_root),
            SnapshotMode::PlainDirectory => Err(AdapterError::UnsupportedOperation {
                operation: "plain directory snapshot",
                capability: Capability::AtomicSnapshots,
            }),
        }
    }

    fn read(
        &self,
        revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        match mode {
            SnapshotMode::Committed => {
                let commit = self.resolve_commit(&revision.id)?;
                self.blob_at(&commit, relative)
            }
            SnapshotMode::WorkingTree => read_within_root(&self.canonical_root, relative),
            SnapshotMode::PlainDirectory => Err(AdapterError::UnsupportedOperation {
                operation: "plain directory snapshot",
                capability: Capability::AtomicSnapshots,
            }),
        }
    }

    fn remote_identity(&self) -> Result<Option<String>, AdapterError> {
        let Ok(url) = self.run_git(&["remote", "get-url", "origin"]) else {
            return Ok(None);
        };
        Ok(sanitize_remote_identity(&url))
    }
}

/// Lines of `.git/info/exclude` when present (working-tree local policy).
pub fn git_exclude_lines(root: &Path) -> Vec<String> {
    match fs::read_to_string(root.join(".git").join("info").join("exclude")) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Bounded local mainline history collection (card K5) for the Git adapter.
///
/// Mainline means first-parent order from HEAD. Output is bounded per the
/// spec: a horizon of commits, a bound on changed paths per commit, and
/// optional bounded diff hunk descriptors (never patch text). Coverage gaps
/// (shallow clone, horizon) are recorded explicitly, never silently.
impl crate::history::HistoryProvider for GitAdapter {
    fn collect_history(
        &self,
        spec: &crate::history::HistorySpec,
    ) -> crate::Result<crate::history::HistorySet> {
        use crate::history::{GapKind, HistoryGap, HistoryItem, HistorySet};
        const MAX_HUNKS_PER_COMMIT: u32 = 20;
        let shallow = self
            .run_git(&["rev-parse", "--is-shallow-repository"])
            .map(|out| out.trim() == "true")
            .unwrap_or(false);
        // An empty repository (no HEAD) has an empty history lane, not an error.
        let Ok(total_text) = self.run_git(&["rev-list", "--first-parent", "--count", "HEAD"])
        else {
            return Ok(HistorySet::default());
        };
        let total = total_text.trim().parse::<i64>().unwrap_or(0);
        let mut gaps = Vec::new();
        if shallow {
            gaps.push(HistoryGap {
                kind: GapKind::ShallowClone,
                detail: "shallow clone: history below the shallow boundary is unavailable"
                    .to_string(),
            });
        }
        if total > spec.horizon as i64 {
            gaps.push(HistoryGap {
                kind: GapKind::Horizon,
                detail: format!(
                    "only the newest {} mainline commits are recorded; the mainline has {total}",
                    spec.horizon
                ),
            });
        }
        let horizon = spec.horizon.to_string();
        let rev_list = list_mainline(self, spec.horizon)?;
        if rev_list.is_empty() {
            return Ok(HistorySet {
                gaps,
                ..Default::default()
            });
        }
        // NUL-framed log: each token is either a header record (prefixed
        // with \x01: sha, parents, author, time, message) or a path line.
        // Records are never concatenated into one giant string, so a
        // control character in a repo-controlled field (message or path)
        // cannot shift the field positions of other commits.
        const SOH: char = '\u{1}';
        const US: char = '\u{1f}';
        let out = self.run_git(&[
            "-c",
            "core.quotepath=false",
            "log",
            "--first-parent",
            "--max-count",
            &horizon,
            "-z",
            "--name-only",
            &format!("--format={SOH}%H{US}%P{US}%an{US}%at{US}%B"),
        ])?;
        // `--name-only` interleaves a bare newline with the framing; it can
        // only appear at token edges (a repository path cannot contain a
        // literal newline unquoted), so trimming newline edges is safe.
        let tokens: Vec<String> = out
            .split('\0')
            .map(|token| token.trim_matches(['\n']))
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect();

        /// One parsed commit record: header fields (when well-formed) and
        /// the path lines that follow it in the stream.
        struct Entry {
            /// (sha, parents, author, time, message) when the header is valid.
            fields: Option<(String, String, String, String, String)>,
            /// Recorded paths for this commit (bounded later).
            paths: Vec<String>,
        }
        let mut entries: Vec<Entry> = Vec::new();
        for token in tokens {
            let Some(rest) = token.strip_prefix(SOH) else {
                match entries.last_mut() {
                    Some(entry) => entry.paths.push(token),
                    None => entries.push(Entry {
                        fields: None,
                        paths: vec![token],
                    }),
                }
                continue;
            };
            let parts: Vec<&str> = rest.splitn(5, US).collect();
            if parts.len() == 5 {
                entries.push(Entry {
                    fields: Some((
                        parts[0].to_string(),
                        parts[1].to_string(),
                        parts[2].to_string(),
                        parts[3].to_string(),
                        parts[4].to_string(),
                    )),
                    paths: Vec::new(),
                });
            } else {
                // A header-shaped token that is not a valid header (a
                // repository file named after the framing bytes): treat it
                // as a path line of the previous record.
                match entries.last_mut() {
                    Some(entry) => entry.paths.push(token),
                    None => entries.push(Entry {
                        fields: None,
                        paths: vec![token],
                    }),
                }
            }
        }
        // Map records to the trusted rev-list: a sha claimed by more than
        // one record (a planted fake header) is never trusted.
        let mut by_sha: std::collections::HashMap<&str, Vec<usize>> =
            std::collections::HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            if let Some((sha, _, _, _, _)) = &entry.fields {
                by_sha.entry(sha.as_str()).or_default().push(index);
            }
        }
        let mut items = Vec::new();
        for sha in &rev_list {
            let entry = match by_sha.get(sha.as_str()) {
                Some(candidates) if candidates.len() == 1 => Some(&entries[candidates[0]]),
                _ => None,
            };
            let Some((_, parents_raw, author_raw, ts_raw, message_raw)) =
                entry.and_then(|entry| entry.fields.as_ref())
            else {
                // The log stream cannot be mapped cleanly for this commit
                // (e.g. a planted fake header); drop it rather than trust
                // repository content for field positions.
                continue;
            };
            let raw_paths = entry.map(|entry| entry.paths.clone()).unwrap_or_default();
            let max_paths = spec.max_paths_per_commit as usize;
            let paths_truncated = raw_paths.len().saturating_sub(max_paths);
            let affected_paths: Vec<String> = raw_paths.into_iter().take(max_paths).collect();
            let redacted = crate::ignore::redact_secret_content(message_raw);
            let pr_hint = crate::history::pr_hint_from_message(&redacted);
            let mut item = HistoryItem {
                revision_id: format!("git:{sha}"),
                parents: parents_raw
                    .split_whitespace()
                    .map(|parent| format!("git:{parent}"))
                    .collect(),
                message: redacted,
                author: if author_raw.is_empty() {
                    None
                } else {
                    Some(author_raw.to_string())
                },
                committed_at_ms: ts_raw
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .map(|seconds| seconds * 1000),
                affected_paths,
                paths_truncated: paths_truncated as u32,
                hunks: Vec::new(),
                hunks_truncated: 0,
                pr_hint: if pr_hint.is_empty() {
                    None
                } else {
                    Some(pr_hint)
                },
                association: None,
            };
            if spec.diff_hunks {
                let first_parent = parents_raw.split_whitespace().next();
                let (hunks, truncated) =
                    hunk_descriptors(self, sha, first_parent, MAX_HUNKS_PER_COMMIT)?;
                item.hunks = hunks;
                item.hunks_truncated = truncated;
            }
            items.push(item);
        }
        let head_revision = items.first().map(|item| item.revision_id.clone());
        Ok(HistorySet {
            head_revision,
            count: items.len(),
            gaps,
            items,
        })
    }
}
