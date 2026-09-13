use super::*;

pub(super) fn checkpoint(number: u64, byte: u8) -> ProjectionCheckpoint {
    ProjectionCheckpoint {
        block_number: number,
        block_hash: B256::repeat_byte(byte),
    }
}

pub(super) fn add_empty_block(provider: &MockEthProvider, number: u64) -> B256 {
    let header = Header {
        number,
        ..Default::default()
    };
    let hash = header.hash_slow();
    provider.add_block(hash, Block::new(header, Default::default()));
    provider.add_receipts(number, Vec::new());
    hash
}

pub(super) fn initialized_runtime(start_block: u64) -> Mutex<ProjectionRuntime> {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block,
    };
    let projector =
        OffchainDataProjection::open(projection_config, reader.clone(), writer.clone()).unwrap();
    let (readiness_publisher, _readiness) = outbe_offchain_data::projection_readiness(
        outbe_offchain_data::ProjectionCheckpoint {
            block_number: 0,
            block_hash: B256::repeat_byte(0x11),
        },
        outbe_offchain_data::ProjectionStatus::Starting,
    );
    let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
    Mutex::new(ProjectionRuntime {
        projector,
        readiness_publisher,
        projection_config,
        _reader: reader,
        overlay: None,
        writer,
        _writer_lease: None,
        runtime_failure_sender: Some(runtime_failure_tx),
        runtime_failure_receiver: Some(runtime_failure_rx),
    })
}

pub(super) struct BlockingWriteStorage {
    inner: MemoryStorage,
    pub(super) block_next_write: AtomicBool,
    write_started: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    release_write: Mutex<std::sync::mpsc::Receiver<()>>,
    write_finished: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
}

impl BlockingWriteStorage {
    pub(super) fn new() -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
        (
            Arc::new(Self {
                inner: MemoryStorage::new(),
                block_next_write: AtomicBool::new(false),
                write_started: Mutex::new(Some(started_tx)),
                release_write: Mutex::new(release_rx),
                write_finished: Mutex::new(Some(finished_tx)),
            }),
            started_rx,
            release_tx,
            finished_rx,
        )
    }
}

impl StorageReader for BlockingWriteStorage {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.inner.get_record(namespace, key)
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        self.inner.get_records(namespace, keys)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.inner.scan_prefix(namespace, request)
    }
}

impl StorageWriter for BlockingWriteStorage {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        let blocked = self.block_next_write.swap(false, Ordering::AcqRel);
        if blocked {
            if let Some(started) = self.write_started.lock().unwrap().take() {
                let _ = started.send(());
            }
            self.release_write.lock().unwrap().recv().unwrap();
        }
        let result = self.inner.apply_atomic(batch);
        if blocked {
            if let Some(finished) = self.write_finished.lock().unwrap().take() {
                let _ = finished.send(());
            }
        }
        result
    }
}

#[derive(Debug, Default)]
pub(super) struct FailAfterStartupStorage {
    inner: MemoryStorage,
    pub(super) fail_reads: AtomicBool,
    pub(super) fail_writes: AtomicBool,
    pub(super) fail_writes_unavailable: AtomicBool,
    pub(super) lose_writer_lease: AtomicBool,
    pub(super) failed_writes: AtomicUsize,
}

impl StorageReader for FailAfterStartupStorage {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable {
                source: std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "injected unavailable read",
                )
                .into(),
            });
        }
        self.inner.get_record(namespace, key)
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable {
                source: std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "injected unavailable read",
                )
                .into(),
            });
        }
        self.inner.get_records(namespace, keys)
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(StorageError::Unavailable {
                source: std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "injected unavailable read",
                )
                .into(),
            });
        }
        self.inner.scan_prefix(namespace, request)
    }
}

impl StorageWriter for FailAfterStartupStorage {
    fn verify_transaction_capability(&self) -> Result<(), StorageError> {
        if self.lose_writer_lease.load(Ordering::SeqCst) {
            return Err(StorageError::WriterLeaseLost);
        }
        if self.fail_reads.load(Ordering::SeqCst)
            || self.fail_writes_unavailable.load(Ordering::SeqCst)
        {
            return Err(StorageError::Unavailable {
                source: Box::new(std::io::Error::other(
                    "injected unavailable transaction capability",
                )),
            });
        }
        Ok(())
    }

    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        if self.lose_writer_lease.load(Ordering::SeqCst) {
            return Err(StorageError::WriterLeaseLost);
        }
        if self.fail_writes_unavailable.load(Ordering::SeqCst) {
            self.failed_writes.fetch_add(1, Ordering::SeqCst);
            return Err(StorageError::Unavailable {
                source: Box::new(std::io::Error::other(
                    "injected unavailable projection write",
                )),
            });
        }
        if self.fail_writes.load(Ordering::SeqCst) {
            self.failed_writes.fetch_add(1, Ordering::SeqCst);
            return Err(StorageError::Corruption(
                "injected post-startup deterministic failure".to_owned(),
            ));
        }
        self.inner.apply_atomic(batch)
    }
}
