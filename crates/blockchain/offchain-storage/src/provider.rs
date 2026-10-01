//! Backend lifecycle is kept outside the node and the domain repositories.

use crate::partitioned::adapters::{
    MongoPartitionDataSource, RocksPartitionDataSource, RocksPartitionReadView,
};
use crate::partitioned::{PartitionRouting, PartitionedStorage};
use std::sync::Arc;

use crate::{
    DayDirectory, MongoStorage, MongoWriterLease, RocksDbCloseWaiter, RocksDbReader,
    RocksDbStorage, StorageBackend, StorageConfig, StorageError, StorageReaderHandle,
    StorageWriterHandle,
};

/// Keeps the writer's backend-specific ownership alive through node shutdown.
pub struct StorageOwnershipGuard {
    _inner: Ownership,
}

impl StorageOwnershipGuard {
    /// Replay a prepared commit after chain/checkpoint validation by the caller.
    pub fn complete_recovery(&self) -> Result<(), StorageError> {
        match &self._inner {
            Ownership::PartitionedRocks { _source } => _source.complete_recovery(),
            _ => Ok(()),
        }
    }
    /// A native close barrier exists only for the process-owned RocksDB writer.
    pub fn rocksdb_close_waiter(&self) -> Option<RocksDbCloseWaiter> {
        match &self._inner {
            Ownership::Mongo { .. } => None,
            Ownership::Rocks { _storage } => Some(_storage.close_waiter()),
            Ownership::PartitionedRocks { _source } => Some(_source.close_waiter()),
        }
    }
}

enum Ownership {
    Mongo {
        _lease: MongoWriterLease,
    },
    Rocks {
        _storage: Arc<RocksDbStorage>,
    },
    PartitionedRocks {
        _source: Arc<RocksPartitionDataSource>,
    },
}

/// One writer and its read capability, referring to the same physical database.
pub struct OpenedStorage {
    pub reader: StorageReaderHandle,
    pub writer: StorageWriterHandle,
    pub ownership: StorageOwnershipGuard,
}

/// Opens capabilities for the configured storage implementation.
#[derive(Clone)]
pub struct StorageProvider {
    config: StorageConfig,
    routing: Option<Arc<dyn PartitionRouting>>,
}

impl StorageProvider {
    pub fn new(config: StorageConfig) -> Result<Self, StorageError> {
        config.validate()?;
        Ok(Self {
            config,
            routing: None,
        })
    }

    #[must_use]
    pub fn with_partition_routing(mut self, routing: Arc<dyn PartitionRouting>) -> Self {
        self.routing = Some(routing);
        self
    }

    pub fn open_writer(&self) -> Result<OpenedStorage, StorageError> {
        match &self.config.backend {
            StorageBackend::MongoDb(config) => {
                let storage = Arc::new(MongoStorage::connect(config.clone())?);
                storage.verify_transaction_support()?;
                let lease = storage.acquire_writer_lease()?;
                let (reader, writer): (StorageReaderHandle, StorageWriterHandle) = match &self
                    .routing
                {
                    Some(routing) => {
                        let source = Arc::new(MongoPartitionDataSource::open(storage.clone())?);
                        let logical = Arc::new(PartitionedStorage::new(source, routing.clone()));
                        (logical.clone(), logical)
                    }
                    None => (storage.clone(), storage),
                };
                Ok(OpenedStorage {
                    reader,
                    writer,
                    ownership: StorageOwnershipGuard {
                        _inner: Ownership::Mongo { _lease: lease },
                    },
                })
            }
            StorageBackend::RocksDb(config) => {
                if let Some(routing) = &self.routing {
                    let source = Arc::new(RocksPartitionDataSource::open(&config.path)?);
                    let logical =
                        Arc::new(PartitionedStorage::new(source.clone(), routing.clone()));
                    return Ok(OpenedStorage {
                        reader: logical.clone(),
                        writer: logical,
                        ownership: StorageOwnershipGuard {
                            _inner: Ownership::PartitionedRocks { _source: source },
                        },
                    });
                }
                let directory = DayDirectory::open(&config.path)?;
                let storage = Arc::new(RocksDbStorage::open(directory.shared_path())?);
                Ok(OpenedStorage {
                    reader: storage.clone(),
                    writer: storage.clone(),
                    ownership: StorageOwnershipGuard {
                        _inner: Ownership::Rocks { _storage: storage },
                    },
                })
            }
        }
    }

    /// Each concurrently active consumer must have its own stable, directory-safe identity.
    pub fn read_source(&self, reader_id: &str) -> Result<StorageReadSource, StorageError> {
        if reader_id.is_empty()
            || reader_id.len() > 128
            || !reader_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(StorageError::invalid_argument(
                "invalid storage reader identity",
            ));
        }
        Ok(StorageReadSource {
            config: self.config.clone(),
            reader_id: reader_id.to_owned(),
            routing: self.routing.clone(),
        })
    }
}

/// Factory for independent export attempts. A session cannot refresh itself.
#[derive(Clone)]
pub struct StorageReadSource {
    config: StorageConfig,
    reader_id: String,
    routing: Option<Arc<dyn PartitionRouting>>,
}

impl StorageReadSource {
    /// Mongo preserves its primary/majority read contract; Rocks pins the caught-up view.
    /// Domain completeness/commitment checks remain required for either backend.
    pub fn open_session(&self) -> Result<StorageReaderHandle, StorageError> {
        match &self.config.backend {
            StorageBackend::MongoDb(config) => {
                let storage = Arc::new(MongoStorage::connect(config.clone())?);
                match &self.routing {
                    Some(routing) => Ok(Arc::new(PartitionedStorage::read_only(
                        Arc::new(MongoPartitionDataSource::open(storage)?),
                        routing.clone(),
                    ))),
                    None => Ok(storage),
                }
            }
            StorageBackend::RocksDb(config) => {
                if let Some(routing) = &self.routing {
                    let source = Arc::new(RocksPartitionReadView::open(
                        &config.path,
                        &config.secondary_path.join(&self.reader_id),
                    )?);
                    return Ok(Arc::new(PartitionedStorage::read_only(
                        source,
                        routing.clone(),
                    )));
                }
                let directory = DayDirectory::open(&config.path)?;
                Ok(Arc::new(RocksDbReader::open(
                    &directory.shared_path(),
                    &config.secondary_path.join(&self.reader_id),
                )?))
            }
        }
    }
}

impl std::fmt::Debug for StorageProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageProvider")
            .field("config", &self.config)
            .field("partitioned", &self.routing.is_some())
            .finish()
    }
}
impl std::fmt::Debug for StorageReadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageReadSource")
            .field("config", &self.config)
            .field("reader_id", &self.reader_id)
            .field("partitioned", &self.routing.is_some())
            .finish()
    }
}
