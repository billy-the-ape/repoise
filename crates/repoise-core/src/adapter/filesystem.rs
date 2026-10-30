//! Filesystem source adapter for plain directories (and Git working trees).

use std::fs;
use std::path::{Path, PathBuf};

use crate::adapter::{
    AdapterError, Capabilities, Revision, RevisionId, SnapshotMode, SourceAdapter, SourceEntry,
    SourceKind, normalize_relative,
};
use crate::hash;

/// Adapter over a plain directory. Symlinks are never followed.
#[derive(Clone, Debug)]
pub struct FilesystemAdapter {
    root: PathBuf,
    canonical_root: PathBuf,
}

impl FilesystemAdapter {
    /// Opens a filesystem adapter rooted at `root`.
    pub fn new(root: &Path) -> Result<Self, AdapterError> {
        let canonical_root = root.canonicalize().map_err(AdapterError::Io)?;
        if !canonical_root.is_dir() {
            return Err(AdapterError::NotARepository(root.to_path_buf()));
        }
        Ok(Self {
            root: root.to_path_buf(),
            canonical_root,
        })
    }
}

impl SourceAdapter for FilesystemAdapter {
    fn kind(&self) -> SourceKind {
        SourceKind::Filesystem
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::NONE
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn canonical_root(&self) -> Result<PathBuf, AdapterError> {
        Ok(self.canonical_root.clone())
    }

    fn resolve(
        &self,
        requested: Option<&RevisionId>,
        mode: SnapshotMode,
    ) -> Result<Revision, AdapterError> {
        match mode {
            SnapshotMode::PlainDirectory => {
                if requested.is_some() {
                    return Err(AdapterError::MissingRevision(RevisionId::new(
                        "filesystem snapshots have no named revisions",
                    )));
                }
                Ok(Revision {
                    id: RevisionId::new("fs:worktree"),
                    branch: None,
                })
            }
            _ => Err(AdapterError::UnsupportedOperation {
                operation: "committed snapshot",
                capability: crate::adapter::Capability::AtomicSnapshots,
            }),
        }
    }

    fn enumerate(
        &self,
        _revision: &Revision,
        mode: SnapshotMode,
    ) -> Result<Vec<SourceEntry>, AdapterError> {
        match mode {
            SnapshotMode::PlainDirectory => {
                scan_directory(&self.canonical_root, &self.canonical_root)
            }
            _ => Err(AdapterError::UnsupportedOperation {
                operation: "committed snapshot",
                capability: crate::adapter::Capability::AtomicSnapshots,
            }),
        }
    }

    fn read(
        &self,
        _revision: &Revision,
        mode: SnapshotMode,
        relative: &Path,
    ) -> Result<Vec<u8>, AdapterError> {
        if mode != SnapshotMode::PlainDirectory {
            return Err(AdapterError::UnsupportedOperation {
                operation: "committed snapshot",
                capability: crate::adapter::Capability::AtomicSnapshots,
            });
        }
        read_within_root(&self.canonical_root, relative)
    }
}

/// Reads `relative` from `canonical_root` after containment validation.
pub fn read_within_root(canonical_root: &Path, relative: &Path) -> Result<Vec<u8>, AdapterError> {
    let relative = normalize_relative(relative)?;
    if relative.as_os_str().is_empty() {
        return Err(AdapterError::MissingFile(PathBuf::from(".")));
    }
    let target = canonical_root.join(&relative);
    match fs::canonicalize(&target) {
        Ok(resolved) => {
            if !resolved.starts_with(canonical_root) {
                return Err(AdapterError::Containment(relative));
            }
            fs::read(&resolved).map_err(AdapterError::Io)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(AdapterError::MissingFile(relative))
        }
        Err(err) => Err(AdapterError::Io(err)),
    }
}

/// Recursively scans `dir` (never following symlinks) into sorted entries.
pub fn scan_directory(canonical_root: &Path, dir: &Path) -> Result<Vec<SourceEntry>, AdapterError> {
    // A missing root is a real failure: scanning it as an empty inventory
    // would let a `watch` publish a zero-file generation over a real index
    // if the root is renamed, removed or unmounted. Only entries that vanish
    // mid-recursion are skipped.
    let mut entries = Vec::new();
    scan(canonical_root, dir, true, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// True when an I/O error means the entry is gone rather than a real
/// failure. On Unix only NotFound qualifies (permission errors are real).
/// On Windows, a file that a concurrent writer just unlinked (delete-pending)
/// can surface as PermissionDenied (ERROR_ACCESS_DENIED); re-check the entry
/// — if the metadata lookup also fails with NotFound or PermissionDenied the
/// entry is gone. When the re-check sees the entry still present (or the
/// error is otherwise inconclusive), the caller retries the operation once
/// before treating the error as genuine: the writer may have recreated the
/// entry in between, in which case the retry resolves the race.
#[cfg_attr(not(windows), allow(unused_variables))]
fn entry_gone(path: &Path, err: &std::io::Error) -> bool {
    match err.kind() {
        std::io::ErrorKind::NotFound => true,
        #[cfg(windows)]
        std::io::ErrorKind::PermissionDenied => {
            matches!(
                fs::symlink_metadata(path).err().map(|err| err.kind()),
                Some(std::io::ErrorKind::NotFound) | Some(std::io::ErrorKind::PermissionDenied)
            )
        }
        _ => false,
    }
}

/// Probes a directory entry that may be racing with a concurrent writer.
/// A first-attempt error that confirms the entry is gone skips it without
/// retrying. Any other failure retries once — the `entry_gone` re-check
/// itself can race with the writer and see a recreated entry — and the
/// second attempt decides: success includes the entry, a gone entry skips
/// it, and a second genuine failure is a real error.
fn probe_entry<T, F>(path: &Path, mut probe: F) -> Result<Option<T>, AdapterError>
where
    F: FnMut() -> std::io::Result<T>,
{
    match probe() {
        Ok(value) => Ok(Some(value)),
        Err(err) if entry_gone(path, &err) => Ok(None),
        Err(_) => match probe() {
            Ok(value) => Ok(Some(value)),
            Err(err) if entry_gone(path, &err) => Ok(None),
            Err(err) => Err(AdapterError::Io(err)),
        },
    }
}

fn scan(
    canonical_root: &Path,
    dir: &Path,
    is_root: bool,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), AdapterError> {
    // A scan of a live tree can race with a concurrent writer (for example
    // `status` inventories the root while `watch` publishes into the
    // in-root cache). Entries that vanish between enumeration and read are
    // skipped; any other I/O error is a real failure. The root itself stays
    // fatal: a vanished root must fail the scan, not yield an empty
    // inventory.
    let read_dir = if is_root {
        Some(fs::read_dir(dir).map_err(AdapterError::Io)?)
    } else {
        probe_entry(dir, || fs::read_dir(dir))?
    };
    let Some(read_dir) = read_dir else {
        return Ok(());
    };
    for result in read_dir {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) if !is_root && entry_gone(dir, &err) => continue,
            Err(err) => return Err(AdapterError::Io(err)),
        };
        let path = entry.path();
        let Some(file_type) = probe_entry(&path, || entry.file_type())? else {
            continue;
        };
        if file_type.is_symlink() {
            // Never follow symlinks (security and determinism).
            continue;
        }
        if file_type.is_dir() {
            scan(canonical_root, &path, false, entries)?;
        } else if file_type.is_file() {
            let Some(bytes) = probe_entry(&path, || fs::read(&path))? else {
                continue;
            };
            let relative = path
                .strip_prefix(canonical_root)
                .map_err(|_| AdapterError::Containment(path.clone()))?
                .to_path_buf();
            entries.push(SourceEntry {
                path: relative,
                content_hash: hash::sha256_hex(&bytes),
                size: bytes.len() as u64,
            });
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A concurrent writer churning a file must not fail a scan: entries that
    /// vanish between enumeration and read are skipped, stable entries remain.
    #[test]
    fn scan_skips_entries_that_vanish_mid_scan() {
        let dir = std::env::temp_dir().join(format!("repoise-scan-race-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let root = fs::canonicalize(&dir).unwrap();
        fs::write(root.join("stable.txt"), "stable").unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let churn_stop = stop.clone();
        let churn_root = root.clone();
        let churner = std::thread::spawn(move || {
            let mut writing = true;
            while !churn_stop.load(Ordering::Relaxed) {
                let path = churn_root.join("churning.txt");
                if writing {
                    let _ = fs::write(&path, "data");
                } else {
                    let _ = fs::remove_file(&path);
                }
                writing = !writing;
            }
        });
        for _ in 0..200 {
            let entries =
                scan_directory(&root, &root).expect("scan must tolerate vanishing entries");
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.path.to_string_lossy() == "stable.txt"),
                "stable entries must survive: {entries:?}"
            );
        }
        stop.store(true, Ordering::Relaxed);
        let _ = churner.join();
        let _ = fs::remove_dir_all(&root);
    }

    /// The skip/retry/fail decision for an entry that vanishes mid-scan must
    /// not depend on timing: these synthetic probes pin the decision table
    /// the churn test exercises against a real concurrent writer.
    #[test]
    fn probe_entry_decision_table_is_deterministic() {
        let dir = std::env::temp_dir().join(format!(
            "repoise-probe-entry-{}-{}",
            std::process::id(),
            now_id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let live = dir.join("live.txt");
        fs::write(&live, "live").unwrap();

        // A successful probe includes the value.
        let calls = AtomicUsize::new(0);
        let result = probe_entry(&live, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(1u32)
        });
        assert_eq!(result.unwrap(), Some(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // A first-attempt NotFound skips immediately, without retrying.
        let calls = AtomicUsize::new(0);
        let result: Result<Option<u32>, AdapterError> = probe_entry(&live, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!(result.unwrap(), None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // A failed probe of a live entry retries; a live entry on the second
        // attempt is included (the writer recreated it in between).
        let calls = AtomicUsize::new(0);
        let result = probe_entry(&live, || {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                Ok(2u32)
            }
        });
        assert_eq!(result.unwrap(), Some(2));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Two genuine failures are a real error, not a skipped entry.
        let calls = AtomicUsize::new(0);
        let result: Result<Option<u32>, AdapterError> = probe_entry(&live, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert!(matches!(result, Err(AdapterError::Io(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // A probe of an entry that is gone skips it. The `entry_gone`
        // re-check may or may not already see the gone entry (platform-
        // dependent), so the probe takes one or two attempts, but the
        // outcome must be a skip.
        let calls = AtomicUsize::new(0);
        let missing = dir.join("missing.txt");
        let result: Result<Option<u32>, AdapterError> =
            probe_entry(&missing, || match calls.fetch_add(1, Ordering::SeqCst) {
                0 => Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                _ => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            });
        assert_eq!(result.unwrap(), None);
        assert!((1..=2).contains(&calls.load(Ordering::SeqCst)));

        fs::remove_dir_all(&dir).unwrap();
    }

    /// A missing scan root is a real failure, not an empty inventory: a
    /// `watch` must not publish a zero-file generation over a real index if
    /// the root is renamed, removed or unmounted.
    #[test]
    fn scan_of_a_missing_root_is_an_error() {
        let missing = std::env::temp_dir().join(format!(
            "repoise-scan-missing-{}-{}",
            std::process::id(),
            now_id()
        ));
        let _ = fs::remove_dir_all(&missing);
        let result = scan_directory(&missing, &missing);
        let missing_root = matches!(
            result.as_ref(),
            Err(AdapterError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound
        );
        assert!(
            missing_root,
            "a missing root must fail, not scan empty: {result:?}"
        );
    }

    /// Unique id suffix so parallel test binaries never share a path.
    fn now_id() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }
}
