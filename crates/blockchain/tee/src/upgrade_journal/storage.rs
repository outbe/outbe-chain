use super::*;

impl UpgradeJournalGuardV1 {
    pub fn acquire(node_data_dir: &Path) -> Result<Self, UpgradeError> {
        let paths = JournalPaths::new(node_data_dir, DIRECTORY);
        create_or_validate_directory(&paths.root)?;
        let lock = open_private_file(&paths.lock, true, MAX_JOURNAL_BYTES)?;
        if let Err(error) =
            rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        {
            let source = std::io::Error::from(error);
            if source.kind() == std::io::ErrorKind::WouldBlock {
                return Err(UpgradeError::JournalLocked);
            }
            return Err(UpgradeError::Io {
                context: "lock upgrade journal",
                source,
            });
        }
        reconcile_scratch(&paths)?;
        Ok(Self { paths, _lock: lock })
    }

    pub fn load(&self) -> Result<Option<UpgradeJournalSnapshotV1>, UpgradeError> {
        read_snapshot(&self.paths.journal)
    }

    pub fn store(&self, mut snapshot: UpgradeJournalSnapshotV1) -> Result<(), UpgradeError> {
        if let Some(current) = self.load()? {
            if is_next_upgrade(&current.lifecycle, &snapshot.lifecycle) {
                // A completed rollout may be followed by the next exact successor.
            } else {
                if snapshot.lifecycle.context() != current.lifecycle.context() {
                    return Err(UpgradeError::ContextChangedAfterPreparation);
                }
                validate_checkpoint_transition(&current.lifecycle, &snapshot.lifecycle)?;
            }
            snapshot.generation = current
                .generation
                .checked_add(1)
                .ok_or(UpgradeError::GenerationExhausted)?;
        }
        snapshot.validate()?;
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW);
        self.paths
            .commit("upgrade")
            .commit_json(&snapshot, &options, MAX_JOURNAL_BYTES)?;
        Ok(())
    }
}

pub(super) fn create_or_validate_directory(path: &Path) -> Result<(), UpgradeError> {
    let mut builder = DirBuilder::new();
    builder.mode(DIRECTORY_MODE);
    match builder.create(path) {
        Ok(()) => {
            sync_directory(path.parent().ok_or(UpgradeError::DirectoryHasNoParent)?)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => validate_directory(path),
        Err(source) => Err(UpgradeError::Io {
            context: "create upgrade journal directory",
            source,
        }),
    }
}

pub(super) fn validate_directory(path: &Path) -> Result<(), UpgradeError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| UpgradeError::PathIo {
        action: "stat private directory",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != DIRECTORY_MODE
    {
        return Err(UpgradeError::InvalidPrivateDirectoryPermissions(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

pub(super) fn open_private_file(
    path: &Path,
    create: bool,
    max_bytes: u64,
) -> Result<File, UpgradeError> {
    let file = crate::journal_storage::private_file_options(create)
        .open(path)
        .map_err(|source| UpgradeError::PathIo {
            action: "open private file",
            path: path.to_path_buf(),
            source,
        })?;
    validate_private_file(path, max_bytes)?;
    Ok(file)
}

pub(super) fn validate_private_file(path: &Path, max_bytes: u64) -> Result<(), UpgradeError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| UpgradeError::PathIo {
        action: "stat private file",
        path: path.to_path_buf(),
        source,
    })?;
    if !private_file_within_bounds(&metadata, max_bytes) {
        return Err(UpgradeError::InvalidPrivateFileBounds(path.to_path_buf()));
    }
    Ok(())
}

pub(super) fn read_private_bounded_file(
    path: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, UpgradeError> {
    let file = open_private_file(path, false, max_bytes)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| UpgradeError::PathIo {
            action: "read private file",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > max_bytes {
        return Err(UpgradeError::PrivateFileSizeCapExceeded(path.to_path_buf()));
    }
    Ok(bytes)
}

pub(super) fn reconcile_scratch(paths: &JournalPaths) -> Result<(), UpgradeError> {
    if paths.next.exists() {
        validate_private_file(&paths.next, MAX_JOURNAL_BYTES)?;
        fs::remove_file(&paths.next).map_err(|source| UpgradeError::Io {
            context: "discard incomplete upgrade journal scratch",
            source,
        })?;
        sync_directory(&paths.root)?;
    }
    Ok(())
}

pub(super) fn read_snapshot(path: &Path) -> Result<Option<UpgradeJournalSnapshotV1>, UpgradeError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = read_private_bounded_file(path, MAX_JOURNAL_BYTES)?;
    let snapshot: UpgradeJournalSnapshotV1 =
        serde_json::from_slice(&bytes).map_err(|source| UpgradeError::Json {
            context: "decode upgrade journal",
            source,
        })?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

fn private_file_within_bounds(metadata: &fs::Metadata, max_bytes: u64) -> bool {
    metadata.file_type().is_file()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.permissions().mode() & 0o777 == FILE_MODE
        && metadata.len() <= max_bytes
}
