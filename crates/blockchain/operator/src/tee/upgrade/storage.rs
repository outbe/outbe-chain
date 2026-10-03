use super::*;

impl UpgradeJournalGuardV1 {
    pub fn acquire(node_data_dir: &Path) -> Result<Self> {
        let paths = JournalPaths::new(node_data_dir, DIRECTORY);
        create_or_validate_directory(&paths.root)?;
        let lock = open_private_file(&paths.lock, true, MAX_JOURNAL_BYTES)?;
        if let Err(error) =
            rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::WouldBlock {
                eyre::bail!("another upgrade operator owns the journal lock");
            }
            return Err(error).wrap_err("lock upgrade journal");
        }
        reconcile_scratch(&paths)?;
        Ok(Self { paths, _lock: lock })
    }

    pub fn load(&self) -> Result<Option<UpgradeJournalSnapshotV1>> {
        read_snapshot(&self.paths.journal)
    }

    pub fn store(&self, mut snapshot: UpgradeJournalSnapshotV1) -> Result<()> {
        if let Some(current) = self.load()? {
            if is_next_upgrade(&current.lifecycle, &snapshot.lifecycle) {
                // A completed rollout may be followed by the next exact successor.
            } else {
                if snapshot.lifecycle.context() != current.lifecycle.context() {
                    eyre::bail!("upgrade journal context cannot change after preparation");
                }
                validate_checkpoint_transition(&current.lifecycle, &snapshot.lifecycle)?;
            }
            snapshot.generation = current
                .generation
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("upgrade journal generation exhausted"))?;
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
            .commit_json(&snapshot, &options, MAX_JOURNAL_BYTES)
    }
}

pub(super) fn create_or_validate_directory(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(DIRECTORY_MODE);
    match builder.create(path) {
        Ok(()) => sync_directory(
            path.parent()
                .ok_or_else(|| eyre::eyre!("upgrade directory has no parent"))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => validate_directory(path),
        Err(error) => Err(error).wrap_err("create upgrade journal directory"),
    }
}

pub(super) fn validate_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("stat private directory {}", path.display()))?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != DIRECTORY_MODE
    {
        eyre::bail!(
            "private directory {} is not owner-only 0700",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn open_private_file(path: &Path, create: bool, max_bytes: u64) -> Result<File> {
    let file = crate::tee::journal_storage::private_file_options(create)
        .open(path)
        .wrap_err_with(|| format!("open private file {}", path.display()))?;
    validate_private_file(path, max_bytes)?;
    Ok(file)
}

pub(super) fn validate_private_file(path: &Path, max_bytes: u64) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("stat private file {}", path.display()))?;
    if !private_file_within_bounds(&metadata, max_bytes) {
        eyre::bail!(
            "private file {} violates owner or size bounds",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn read_private_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let file = open_private_file(path, false, max_bytes)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .wrap_err_with(|| format!("read private file {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        eyre::bail!("private file {} exceeds its size cap", path.display());
    }
    Ok(bytes)
}

pub(super) fn reconcile_scratch(paths: &JournalPaths) -> Result<()> {
    if paths.next.exists() {
        validate_private_file(&paths.next, MAX_JOURNAL_BYTES)?;
        fs::remove_file(&paths.next).wrap_err("discard incomplete upgrade journal scratch")?;
        sync_directory(&paths.root)?;
    }
    Ok(())
}

pub(super) fn read_snapshot(path: &Path) -> Result<Option<UpgradeJournalSnapshotV1>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = read_private_bounded_file(path, MAX_JOURNAL_BYTES)?;
    let snapshot: UpgradeJournalSnapshotV1 =
        serde_json::from_slice(&bytes).wrap_err("decode upgrade journal")?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

fn private_file_within_bounds(metadata: &fs::Metadata, max_bytes: u64) -> bool {
    metadata.file_type().is_file()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.permissions().mode() & 0o777 == FILE_MODE
        && metadata.len() <= max_bytes
}
