//! Backend lifecycle is kept outside the node and the domain repositories.

use crate::partitioned::adapters::{RocksPartitionDataSource, RocksPartitionReadView};
use crate::partitioned::{PartitionRouting, PartitionedStorage};
use std::sync::Arc;

use crate::{
    DayDirectory, OpenedStorage, RocksDbReader, RocksDbStorage, StorageBackend, StorageConfig,
    StorageError, StorageReaderHandle,
};

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
            StorageBackend::RocksDb(config) => {
                if let Some(routing) = &self.routing {
                    let source = Arc::new(RocksPartitionDataSource::open(&config.path)?);
                    let logical =
                        Arc::new(PartitionedStorage::new(source.clone(), routing.clone()));
                    return Ok(OpenedStorage::new(
                        logical.clone(),
                        logical,
                        source.lifecycle(),
                    ));
                }
                let directory = DayDirectory::open(&config.path)?;
                let storage = Arc::new(RocksDbStorage::open(directory.shared_path())?);
                Ok(OpenedStorage::new(
                    storage.clone(),
                    storage.clone(),
                    Box::new(crate::rocks::lifecycle::RocksLifecycle::new(storage)),
                ))
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
    /// Pin the caught-up RocksDB view for this session.
    /// Domain completeness and commitment checks remain required.
    pub fn open_session(&self) -> Result<StorageReaderHandle, StorageError> {
        match &self.config.backend {
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
