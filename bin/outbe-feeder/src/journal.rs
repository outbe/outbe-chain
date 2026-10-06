//! Durable, single-writer record of a signed vote whose outcome is unresolved.
//!
//! Persist before broadcasting. A persistence error must stop submission: an
//! error after rename can mean that the on-disk result is uncertain.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use eyre::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingVote {
    pub raw: String,
    pub hash: String,
    pub nonce: u64,
    pub max_fee_per_gas: u128,
    pub created_at: u64,
    pub observed_height: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    identity: String,
    pending: Option<PendingVote>,
}

pub struct Journal {
    path: PathBuf,
    identity: String,
    pending: Option<PendingVote>,
    // Never unlink the lock file: replacing its inode would allow two owners.
    _lock: File,
    poisoned: bool,
}

impl Journal {
    pub fn open(path: &Path, identity: &str) -> Result<Self> {
        if identity.is_empty() {
            bail!("pending vote journal identity must not be empty");
        }
        let lock_path = suffixed(path, ".lock");
        reject_symlink(&lock_path)?;
        let lock = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .wrap_err_with(|| format!("open journal lock {}", lock_path.display()))?;
        lock.try_lock()
            .wrap_err("pending vote journal is already locked or cannot be locked")?;
        set_private(&lock)?;

        reject_symlink(path)?;
        let record = match File::open(path) {
            Ok(file) => {
                set_private(&file)?;
                let record: Record = serde_json::from_reader(file)
                    .wrap_err("invalid pending vote journal; refusing to discard it")?;
                if record.version != 1 {
                    bail!(
                        "unsupported pending vote journal version {}",
                        record.version
                    );
                }
                if record.identity != identity {
                    bail!("pending vote journal belongs to a different chain or signer");
                }
                Some(record)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).wrap_err("read pending vote journal"),
        };
        let exists = record.is_some();
        let mut journal = Self {
            path: path.to_owned(),
            identity: identity.to_owned(),
            pending: record.and_then(|record| record.pending),
            _lock: lock,
            poisoned: false,
        };
        if !exists {
            journal.persist(None)?;
        }
        Ok(journal)
    }

    pub fn pending(&self) -> Option<&PendingVote> {
        self.pending.as_ref()
    }

    pub fn store(&mut self, pending: PendingVote) -> Result<()> {
        self.ensure_writable()?;
        if let Some(existing) = &self.pending {
            if existing == &pending {
                return Ok(());
            }
            bail!("cannot replace an unresolved pending vote");
        }
        self.persist(Some(pending))
    }

    pub fn clear(&mut self) -> Result<()> {
        self.persist(None)
    }

    /// Replace only the transaction occupying the same unresolved nonce. An
    /// older signed candidate may still be mined, so its nonce remains reserved.
    pub fn replace(&mut self, pending: PendingVote) -> Result<()> {
        self.ensure_writable()?;
        let Some(existing) = &self.pending else {
            bail!("cannot replace a vote without an unresolved pending transaction");
        };
        if pending.nonce != existing.nonce {
            bail!("replacement must retain the unresolved nonce");
        }
        if pending.hash == existing.hash || pending.raw == existing.raw {
            bail!("replacement must be a different signed transaction");
        }
        if pending.max_fee_per_gas <= existing.max_fee_per_gas {
            bail!("replacement fee cap must increase");
        }
        if pending.created_at != existing.created_at {
            bail!("replacement must preserve the original pending age");
        }
        self.persist(Some(pending))
    }

    fn ensure_writable(&self) -> Result<()> {
        if self.poisoned {
            bail!("journal durability is uncertain; restart and reconcile before submitting");
        }
        Ok(())
    }

    fn persist(&mut self, pending: Option<PendingVote>) -> Result<()> {
        self.ensure_writable()?;
        let record = Record {
            version: 1,
            identity: self.identity.clone(),
            pending,
        };
        let bytes = serde_json::to_vec(&record).wrap_err("encode pending vote journal")?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = File::open(parent).wrap_err("open pending vote journal directory")?;
        let (temporary_path, mut temporary) = create_temporary(&self.path)?;
        let result = (|| -> Result<()> {
            temporary
                .write_all(&bytes)
                .wrap_err("write pending vote journal")?;
            temporary.sync_all().wrap_err("sync pending vote journal")?;
            fs::rename(&temporary_path, &self.path).wrap_err("replace pending vote journal")?;
            // Once renamed, failure is ambiguous. Prevent further writes until
            // restart reconciles the record that survived on disk.
            self.poisoned = true;
            directory
                .sync_all()
                .wrap_err("sync pending vote journal directory")?;
            self.poisoned = false;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result?;
        self.pending = record.pending;
        Ok(())
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn set_private(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .wrap_err("restrict pending vote journal permissions")?;
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("journal path must not be a symlink: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).wrap_err("inspect pending vote journal path"),
    }
}

fn create_temporary(path: &Path) -> Result<(PathBuf, File)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    loop {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_path = suffixed(path, &format!(".tmp.{}.{sequence}", std::process::id()));
        match private_options()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).wrap_err("create pending vote journal temporary file"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            loop {
                let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "outbe-feeder-journal-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create test directory: {error}"),
                }
            }
        }

        fn path(&self) -> PathBuf {
            self.0.join("pending.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn vote() -> PendingVote {
        PendingVote {
            raw: "0x01".into(),
            hash: "0x02".into(),
            nonce: 4,
            max_fee_per_gas: 100,
            created_at: 100,
            observed_height: 80,
        }
    }

    #[test]
    fn restart_recovers_pending_and_clear_preserves_identity() {
        let directory = TestDirectory::new();
        let mut journal = Journal::open(&directory.path(), "chain:genesis:signer").unwrap();
        journal.store(vote()).unwrap();
        drop(journal);
        assert!(Journal::open(&directory.path(), "chain:genesis:other-signer").is_err());
        let mut journal = Journal::open(&directory.path(), "chain:genesis:signer").unwrap();
        assert_eq!(journal.pending(), Some(&vote()));
        journal.clear().unwrap();
        drop(journal);
        let journal = Journal::open(&directory.path(), "chain:genesis:signer").unwrap();
        assert!(journal.pending().is_none());
        drop(journal);
        assert!(Journal::open(&directory.path(), "another-chain:genesis:signer").is_err());
    }

    #[test]
    fn lock_excludes_a_second_writer_and_releases_on_drop() {
        let directory = TestDirectory::new();
        let journal = Journal::open(&directory.path(), "identity").unwrap();
        assert!(Journal::open(&directory.path(), "identity").is_err());
        drop(journal);
        assert!(Journal::open(&directory.path(), "identity").is_ok());
    }

    #[test]
    fn corrupt_record_is_not_discarded() {
        let directory = TestDirectory::new();
        fs::write(directory.path(), "{broken").unwrap();
        assert!(Journal::open(&directory.path(), "identity").is_err());
        assert_eq!(fs::read_to_string(directory.path()).unwrap(), "{broken");
    }

    #[test]
    fn unresolved_vote_cannot_be_overwritten() {
        let directory = TestDirectory::new();
        let mut journal = Journal::open(&directory.path(), "identity").unwrap();
        journal.store(vote()).unwrap();
        let mut other = vote();
        other.nonce += 1;
        assert!(journal.store(other).is_err());
        assert_eq!(journal.pending(), Some(&vote()));
    }

    #[test]
    fn same_nonce_replacement_survives_restart_and_preserves_pending_age() {
        let directory = TestDirectory::new();
        let mut journal = Journal::open(&directory.path(), "identity").unwrap();
        journal.store(vote()).unwrap();
        let mut replacement = vote();
        replacement.raw = "0x03".into();
        replacement.hash = "0x04".into();
        replacement.max_fee_per_gas = 113;
        replacement.observed_height += 8;
        journal.replace(replacement.clone()).unwrap();
        drop(journal);
        let journal = Journal::open(&directory.path(), "identity").unwrap();
        assert_eq!(journal.pending(), Some(&replacement));
        assert_eq!(journal.pending().unwrap().created_at, vote().created_at);
    }

    #[test]
    fn invalid_replacement_leaves_original_vote_intact() {
        let directory = TestDirectory::new();
        let mut journal = Journal::open(&directory.path(), "identity").unwrap();
        let mut replacement = vote();
        replacement.raw = "0x03".into();
        replacement.hash = "0x04".into();
        replacement.max_fee_per_gas = 113;
        assert!(journal.replace(replacement.clone()).is_err());
        journal.store(vote()).unwrap();
        let mut changed_nonce = replacement.clone();
        changed_nonce.nonce += 1;
        let mut unchanged_fee = replacement.clone();
        unchanged_fee.max_fee_per_gas = vote().max_fee_per_gas;
        let mut changed_age = replacement.clone();
        changed_age.created_at += 1;
        let mut unchanged_hash = replacement;
        unchanged_hash.hash = vote().hash;
        for invalid in [changed_nonce, unchanged_fee, changed_age, unchanged_hash] {
            assert!(journal.replace(invalid).is_err());
            assert_eq!(journal.pending(), Some(&vote()));
        }
        drop(journal);
        assert_eq!(
            Journal::open(&directory.path(), "identity")
                .unwrap()
                .pending(),
            Some(&vote())
        );
    }

    #[test]
    fn failed_write_preserves_memory_and_previous_durable_record() {
        let directory = TestDirectory::new();
        let mut journal = Journal::open(&directory.path(), "identity").unwrap();
        journal.store(vote()).unwrap();
        // Move the directory so a subsequent persistence operation cannot
        // open it. This fails reliably even when tests run as root.
        let backup = TestDirectory::new();
        let moved = backup.0.join("moved");
        fs::rename(&directory.0, &moved).unwrap();
        assert!(journal.clear().is_err());
        assert_eq!(journal.pending(), Some(&vote()));
        fs::rename(&moved, &directory.0).unwrap();
        drop(journal);
        let journal = Journal::open(&directory.path(), "identity").unwrap();
        assert_eq!(journal.pending(), Some(&vote()));
    }

    #[cfg(unix)]
    #[test]
    fn journal_and_lock_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = TestDirectory::new();
        let mut journal = Journal::open(&directory.path(), "identity").unwrap();
        journal.store(vote()).unwrap();
        for path in [directory.path(), suffixed(&directory.path(), ".lock")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
