//! Durable, crash-consistent commit boundary for owner-only host TEE journals.

use std::{
    fs::{self, File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Durable journal metadata and the operation's distinct lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalSnapshotV1<S> {
    pub version: u8,
    pub generation: u64,
    pub lifecycle: S,
}

impl<S> JournalSnapshotV1<S> {
    pub const fn new(lifecycle: S) -> Self {
        Self {
            version: 1,
            generation: 1,
            lifecycle,
        }
    }
}

/// Typed errors from atomic host journal writes.
#[derive(Debug, Error)]
pub enum JournalStorageError {
    /// The encoded journal exceeds the byte limit.
    #[error("{0} journal exceeds its size cap")]
    SizeCapExceeded(&'static str),

    /// JSON serialization of the journal snapshot failed.
    #[error("encode {label} journal: {source}")]
    Encode {
        label: &'static str,
        #[source]
        source: serde_json::Error,
    },

    /// Creating, writing, or fsyncing the scratch file failed.
    #[error("{action} {label} journal scratch")]
    ScratchIo {
        action: &'static str,
        label: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// Renaming the scratch file over the committed journal failed.
    #[error("commit {label} journal: {source}")]
    Commit {
        label: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// Opening or fsyncing a parent/journal directory failed.
    #[error("{action} directory {}: {source}", path.display())]
    DirectorySync {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub struct JournalCommit<'a> {
    pub root: &'a Path,
    pub scratch: &'a Path,
    pub journal: &'a Path,
    pub label: &'static str,
}

impl JournalCommit<'_> {
    pub fn commit_json(
        &self,
        snapshot: &impl Serialize,
        options: &OpenOptions,
        max_bytes: u64,
    ) -> Result<(), JournalStorageError> {
        let encoded =
            serde_json::to_vec(snapshot).map_err(|source| JournalStorageError::Encode {
                label: self.label,
                source,
            })?;
        if encoded.len() as u64 > max_bytes {
            return Err(JournalStorageError::SizeCapExceeded(self.label));
        }
        let mut next =
            options
                .open(self.scratch)
                .map_err(|source| JournalStorageError::ScratchIo {
                    action: "create",
                    label: self.label,
                    source,
                })?;
        next.write_all(&encoded)
            .map_err(|source| JournalStorageError::ScratchIo {
                action: "write",
                label: self.label,
                source,
            })?;
        next.sync_all()
            .map_err(|source| JournalStorageError::ScratchIo {
                action: "fsync",
                label: self.label,
                source,
            })?;
        fs::rename(self.scratch, self.journal).map_err(|source| JournalStorageError::Commit {
            label: self.label,
            source,
        })?;
        sync_directory(self.root)
    }
}

pub fn sync_directory(path: &Path) -> Result<(), JournalStorageError> {
    File::open(path)
        .map_err(|source| JournalStorageError::DirectorySync {
            action: "open",
            path: path.to_path_buf(),
            source,
        })?
        .sync_all()
        .map_err(|source| JournalStorageError::DirectorySync {
            action: "fsync",
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Clone, Debug)]
pub struct JournalPaths {
    pub root: PathBuf,
    pub journal: PathBuf,
    pub next: PathBuf,
    pub lock: PathBuf,
}

impl JournalPaths {
    #[must_use]
    pub fn new(node_data_dir: &Path, directory: &str) -> Self {
        let root = node_data_dir.join(directory);
        Self {
            journal: root.join("journal.json"),
            next: root.join("journal.next"),
            lock: root.join("state.lock"),
            root,
        }
    }

    #[must_use]
    pub fn commit(&self, label: &'static str) -> JournalCommit<'_> {
        JournalCommit {
            root: &self.root,
            scratch: &self.next,
            journal: &self.journal,
            label,
        }
    }
}

#[must_use]
pub fn private_file_options(create: bool) -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    fn options() -> OpenOptions {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        options
    }

    fn existing_journal() -> (tempfile::TempDir, JournalPaths) {
        let root = tempfile::tempdir().unwrap();
        let paths = JournalPaths::new(root.path(), "");
        fs::write(&paths.journal, b"old").unwrap();
        (root, paths)
    }

    #[test]
    fn cap_rejection_does_not_create_scratch_or_replace_committed_bytes() {
        let (_root, paths) = existing_journal();
        let scratch = &paths.next;
        let journal = &paths.journal;
        let commit = paths.commit("upgrade");
        assert_eq!(
            commit
                .commit_json(&vec![1, 2, 3], &options(), 1)
                .unwrap_err()
                .to_string(),
            "upgrade journal exceeds its size cap"
        );
        assert!(!scratch.exists());
        assert_eq!(fs::read(journal).unwrap(), b"old");
    }

    #[test]
    fn commit_is_exclusive_and_preserves_canonical_bytes_and_private_mode() {
        let (_root, paths) = existing_journal();
        let scratch = &paths.next;
        let journal = &paths.journal;
        fs::write(scratch, b"unfinished").unwrap();
        let commit = paths.commit("renewal");
        let value = serde_json::json!({"version":1,"generation":2});
        assert_eq!(
            commit
                .commit_json(&value, &options(), 4096)
                .unwrap_err()
                .to_string(),
            "create renewal journal scratch"
        );
        assert_eq!(fs::read(journal).unwrap(), b"old");
        assert_eq!(fs::read(scratch).unwrap(), b"unfinished");
        fs::remove_file(scratch).unwrap();
        commit.commit_json(&value, &options(), 4096).unwrap();
        assert!(!scratch.exists());
        assert_eq!(
            fs::read(journal).unwrap(),
            serde_json::to_vec(&value).unwrap()
        );
        assert_eq!(
            fs::metadata(journal).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
