use crate::ocomp::retention::*;

pub(super) mod codec;

pub(in crate::ocomp::retention) const JOURNAL_FILENAME: &str = "pin.v1";

const JOURNAL_TEMP_FILENAME: &str = "pin.v1.tmp";

pub(crate) trait JournalDurability: Send + Sync {
    fn sync_file(&self, file: &File) -> std::io::Result<()>;
    fn sync_directory(&self, directory: &File) -> std::io::Result<()>;
}

#[derive(Debug, Default)]
pub(in crate::ocomp::retention) struct OsJournalDurability;

impl JournalDurability for OsJournalDurability {
    fn sync_file(&self, file: &File) -> std::io::Result<()> {
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> std::io::Result<()> {
        directory.sync_all()
    }
}

pub(in crate::ocomp::retention) struct JournalStore {
    root: PathBuf,
    journal: PathBuf,
    temporary: PathBuf,
    durability: Arc<dyn JournalDurability>,
}

impl JournalStore {
    pub(in crate::ocomp::retention) fn new(
        root: PathBuf,
        durability: Arc<dyn JournalDurability>,
    ) -> Self {
        Self {
            journal: root.join(JOURNAL_FILENAME),
            temporary: root.join(JOURNAL_TEMP_FILENAME),
            root,
            durability,
        }
    }

    pub(in crate::ocomp::retention) fn initialize(
        &self,
    ) -> Result<Option<JobRegistryV1>, RetentionError> {
        fs::create_dir_all(&self.root)
            .map_err(|source| self.io("create directory", &self.root, source))?;
        self.recover_temporary()?;
        let registry = self.read_registry_at(&self.journal)?;
        if registry.is_some() {
            let journal = File::open(&self.journal)
                .map_err(|source| self.io("open authoritative journal", &self.journal, source))?;
            self.durability
                .sync_file(&journal)
                .map_err(|source| self.io("fsync authoritative journal", &self.journal, source))?;
            File::open(&self.root)
                .and_then(|directory| self.durability.sync_directory(&directory))
                .map_err(|source| self.io("fsync journal directory", &self.root, source))?;
        }
        Ok(registry)
    }

    pub(in crate::ocomp::retention) fn recover_and_load(
        &self,
    ) -> Result<Option<JobRegistryV1>, RetentionError> {
        self.initialize()
    }

    fn read_registry_at(&self, path: &Path) -> Result<Option<JobRegistryV1>, RetentionError> {
        if !path
            .try_exists()
            .map_err(|source| self.io("check existence", path, source))?
        {
            return Ok(None);
        }
        let metadata =
            fs::symlink_metadata(path).map_err(|source| self.io("stat", path, source))?;
        if !metadata.file_type().is_file() {
            return Err(RetentionError::AmbiguousJournal(
                "journal is not a regular file",
            ));
        }
        if metadata.len() > JOURNAL_MAX_BYTES as u64 {
            return Err(RetentionError::MalformedJournal("journal exceeds byte cap"));
        }
        let mut file = File::open(path).map_err(|source| self.io("open", path, source))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|source| self.io("read", path, source))?;
        decode_registry(&bytes).map(Some)
    }

    fn recover_temporary(&self) -> Result<(), RetentionError> {
        let pending = match self.read_registry_at(&self.temporary) {
            Ok(Some(pending)) => pending,
            Ok(None) => return Ok(()),
            Err(
                RetentionError::MalformedJournal(_)
                | RetentionError::UnsupportedJournalVersion { .. },
            ) => {
                // The temp name is never published authority. A crash before
                // its fsync may leave arbitrary/truncated bytes; discard those
                // and replay from the last durable frame/journal generation.
                self.discard_temporary()?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let current = self.read_registry_at(&self.journal)?;
        let valid_successor = match current.as_ref() {
            None => {
                pending.generation == 1
                    && pending.records.len() == 1
                    && pending.records.contains_key(&pending.last_updated)
            }
            Some(current) => journal_successor_is_exact(current, &pending),
        };
        if !valid_successor {
            return Err(RetentionError::AmbiguousJournal(
                "temporary write is not the exact next journal generation",
            ));
        }
        let temporary = File::open(&self.temporary)
            .map_err(|source| self.io("open temporary for recovery", &self.temporary, source))?;
        self.durability
            .sync_file(&temporary)
            .map_err(|source| self.io("fsync temporary for recovery", &self.temporary, source))?;
        fs::rename(&self.temporary, &self.journal)
            .map_err(|source| self.io("recover temporary", &self.temporary, source))?;
        File::open(&self.root)
            .and_then(|directory| self.durability.sync_directory(&directory))
            .map_err(|source| self.io("fsync recovered directory", &self.root, source))?;
        Ok(())
    }

    fn discard_temporary(&self) -> Result<(), RetentionError> {
        fs::remove_file(&self.temporary)
            .map_err(|source| self.io("discard torn temporary", &self.temporary, source))?;
        File::open(&self.root)
            .and_then(|directory| self.durability.sync_directory(&directory))
            .map_err(|source| self.io("fsync discarded temporary", &self.root, source))
    }

    pub(in crate::ocomp::retention) fn persist(
        &self,
        registry: &JobRegistryV1,
        changed: PinRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let encoded = encode_registry(registry);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.temporary)
            .map_err(|source| self.io("create temporary", &self.temporary, source))?;
        file.write_all(&encoded)
            .map_err(|source| self.io("write temporary", &self.temporary, source))?;
        self.durability
            .sync_file(&file)
            .map_err(|source| self.io("fsync temporary", &self.temporary, source))?;
        fs::rename(&self.temporary, &self.journal)
            .map_err(|source| self.io("publish", &self.journal, source))?;
        File::open(&self.root)
            .and_then(|directory| self.durability.sync_directory(&directory))
            .map_err(|source| self.io("fsync directory", &self.root, source))?;
        Ok(ack_for(changed))
    }

    fn io(&self, operation: &'static str, path: &Path, source: std::io::Error) -> RetentionError {
        RetentionError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

pub(in crate::ocomp::retention) fn journal_successor_is_exact(
    current: &JobRegistryV1,
    pending: &JobRegistryV1,
) -> bool {
    if current.generation.checked_add(1) != Some(pending.generation)
        || !pending.records.contains_key(&pending.last_updated)
    {
        return false;
    }
    current.records.iter().all(|(key, record)| {
        *key == pending.last_updated
            || pending.records.get(key) == Some(record)
            || (!pending.records.contains_key(key)
                && matches!(record.state, PinStateV1::Released { .. }))
    }) && pending
        .records
        .keys()
        .all(|key| current.records.contains_key(key) || *key == pending.last_updated)
}
