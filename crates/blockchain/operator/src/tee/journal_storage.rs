//! The common durable commit boundary for owner-only operator journals.

use eyre::{Result, WrapErr as _};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

pub(super) struct JournalCommit<'a> {
    pub root: &'a Path,
    pub scratch: &'a Path,
    pub journal: &'a Path,
    pub label: &'static str,
}
impl JournalCommit<'_> {
    pub(super) fn commit_json(
        &self,
        snapshot: &impl Serialize,
        options: &OpenOptions,
        max_bytes: u64,
    ) -> Result<()> {
        let encoded = serde_json::to_vec(snapshot)
            .wrap_err_with(|| format!("encode {} journal", self.label))?;
        if encoded.len() as u64 > max_bytes {
            eyre::bail!("{} journal exceeds its size cap", self.label);
        }
        let mut next = options
            .open(self.scratch)
            .wrap_err_with(|| format!("create {} journal scratch", self.label))?;
        next.write_all(&encoded)
            .wrap_err_with(|| format!("write {} journal scratch", self.label))?;
        next.sync_all()
            .wrap_err_with(|| format!("fsync {} journal scratch", self.label))?;
        fs::rename(self.scratch, self.journal)
            .wrap_err_with(|| format!("commit {} journal", self.label))?;
        sync_directory(self.root)
    }
}
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .wrap_err_with(|| format!("open directory {} for fsync", path.display()))?
        .sync_all()
        .wrap_err_with(|| format!("fsync directory {}", path.display()))
}

#[derive(Clone)]
pub(super) struct JournalPaths {
    pub root: PathBuf,
    pub journal: PathBuf,
    pub next: PathBuf,
    pub lock: PathBuf,
}
impl JournalPaths {
    pub(super) fn new(node_data_dir: &Path, directory: &str) -> Self {
        let root = node_data_dir.join(directory);
        Self {
            journal: root.join("journal.json"),
            next: root.join("journal.next"),
            lock: root.join("state.lock"),
            root,
        }
    }
    pub(super) fn commit(&self, label: &'static str) -> JournalCommit<'_> {
        JournalCommit {
            root: &self.root,
            scratch: &self.next,
            journal: &self.journal,
            label,
        }
    }
}

pub(super) fn private_file_options(create: bool) -> OpenOptions {
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
