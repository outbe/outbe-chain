//! Datasource-independent entity partition orchestration.

pub mod adapters;
mod batch;
mod ports;
mod read;
pub mod routing;
mod scan;
mod scope;
mod strategy;

use parking_lot::RwLock;
use std::sync::Arc;

pub use batch::{PartitionedBatch, PartitionedOperation};
pub use ports::{PartitionDataSource, PartitionReadSource, PartitionRouting, ReadLocation};
pub use scope::{PartitionId, StorageScope};
pub use strategy::{
    OwnerModuloPartition, PartitionContext, PartitionStrategy, SharedPartition,
    WorldwideDayPartition,
};

/// Combines injected domain routing with an injected physical datasource.
/// Neither backend selection nor entity-specific rules live in this module.
pub struct PartitionedStorage {
    source: Arc<dyn PartitionReadSource>,
    writer: Option<Arc<dyn PartitionDataSource>>,
    routing: Arc<dyn PartitionRouting>,
    gate: RwLock<()>,
}

impl PartitionedStorage {
    #[must_use]
    pub fn new(source: Arc<dyn PartitionDataSource>, routing: Arc<dyn PartitionRouting>) -> Self {
        Self {
            source: source.clone(),
            writer: Some(source),
            routing,
            gate: RwLock::new(()),
        }
    }

    /// Read sessions receive no write capability, including recovery or migration.
    #[must_use]
    pub fn read_only(
        source: Arc<dyn PartitionReadSource>,
        routing: Arc<dyn PartitionRouting>,
    ) -> Self {
        Self {
            source,
            writer: None,
            routing,
            gate: RwLock::new(()),
        }
    }

    pub fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, crate::StorageError> {
        self.source.list_scopes(domain)
    }
}
