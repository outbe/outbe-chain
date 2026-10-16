use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _},
};

pub(super) struct Journal {
    pub root: PathBuf,
    pub chain: outbe_radicle::endpoint::ChainIdentity,
    pub packages: ReviewPackageStore,
    _lock: File,
}

impl Journal {
    pub fn open(root: PathBuf, chain: outbe_radicle::endpoint::ChainIdentity) -> Result<Self> {
        private_directory(&root)?;
        let path = root.join("collector.lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            return Err("invalid collector lock".into());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        lock.try_lock()?;
        put(
            &root.join("chain"),
            &[
                chain.chain_id.to_be_bytes().as_slice(),
                chain.genesis_hash.as_slice(),
            ]
            .concat(),
            false,
        )?;
        let packages = ReviewPackageStore::open(root.join("packages"))?;
        Ok(Self {
            root,
            chain,
            packages,
            _lock: lock,
        })
    }

    pub fn select(
        &self,
        body: &PublicationBody,
        descriptor: &GovernmentsDescriptor,
    ) -> Result<PathBuf> {
        if let Some(old) = read(&self.root.join("current"), 477)? {
            let old = PublicationBody::from_bytes(&old)?;
            if old.repo_id != body.repo_id
                || old.chain_id != body.chain_id
                || old.genesis_hash != body.genesis_hash
                || old.descriptor_hash != body.descriptor_hash
            {
                return Err("collector chain, descriptor or initial owner changed".into());
            }
            if old.receipt_hash() != body.receipt_hash() {
                // Only unfinished bootstrap permission reaches this method; accepted archives stay.
                let path = self.root.join(hex::encode(old.receipt_hash()));
                if path.exists() {
                    fs::remove_dir_all(path)?;
                }
            }
        }
        let directory = self.root.join(hex::encode(body.receipt_hash()));
        private_directory(&directory)?;
        put(
            &self.root.join("descriptor"),
            &descriptor.to_bytes()?,
            false,
        )?;
        put(&self.root.join("current"), &body.to_bytes(), true)?;
        Ok(directory)
    }
}

pub(super) fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err("collector directory must be private".into());
    }
    Ok(())
}

pub(super) fn read(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err("invalid collector journal file".into());
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err("oversized collector journal file".into());
    }
    Ok(Some(bytes))
}

pub(super) fn put(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    if let Some(old) = read(path, bytes.len())? {
        if old == bytes {
            return Ok(());
        }
        if !replace {
            return Err("conflicting collector journal record".into());
        }
    }
    let temporary = path.with_extension("pending");
    if fs::symlink_metadata(&temporary).is_ok() {
        fs::remove_file(&temporary)?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(path.parent().ok_or("collector journal parent missing")?)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_radicle::endpoint::ChainIdentity;
    use std::os::unix::fs::symlink;

    #[test]
    fn one_writer_and_chain_binding_survive_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("collector");
        let chain = ChainIdentity {
            chain_id: 2026,
            genesis_hash: [7; 32].into(),
        };
        let journal = Journal::open(root.clone(), chain).unwrap();
        assert!(Journal::open(root.clone(), chain).is_err());
        put(&root.join("installed"), b"accepted", false).unwrap();
        drop(journal);

        let reopened = Journal::open(root.clone(), chain).unwrap();
        assert_eq!(
            read(&root.join("installed"), 8).unwrap().unwrap(),
            b"accepted"
        );
        drop(reopened);
        assert!(Journal::open(
            root.clone(),
            ChainIdentity {
                genesis_hash: [8; 32].into(),
                ..chain
            }
        )
        .is_err());
        assert!(Journal::open(root, chain).is_ok());
    }

    #[test]
    fn interrupted_write_preserves_committed_record_and_conflicts_do_not_replace_it() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("partial");
        put(&path, b"first", false).unwrap();
        fs::write(path.with_extension("pending"), b"incomplete").unwrap();
        assert_eq!(read(&path, 5).unwrap().unwrap(), b"first");
        assert!(put(&path, b"other", false).is_err());
        assert_eq!(read(&path, 5).unwrap().unwrap(), b"first");
        put(&path, b"other", true).unwrap();
        assert_eq!(read(&path, 5).unwrap().unwrap(), b"other");
        assert!(!path.with_extension("pending").exists());
        put(&path, b"other", false).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn nonprivate_directory_symlink_and_oversized_record_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("collector");
        private_directory(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&root).is_err());

        let target = temporary.path().join("target");
        fs::write(&target, b"oversized").unwrap();
        assert!(read(&target, 8).is_err());
        let link = temporary.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(read(&link, 64).is_err());
        assert!(put(&link, b"replacement", true).is_err());
        assert_eq!(fs::read(target).unwrap(), b"oversized");
    }
}
