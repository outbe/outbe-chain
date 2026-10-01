//! MongoDB composition: one transaction over scope-addressed collections.

use super::super::{PartitionDataSource, PartitionReadSource, PartitionedBatch, StorageScope};
use super::{CollectionDataSource, ScopeCatalog};
use crate::{MongoStorage, StorageError, StorageReaderHandle};
use std::sync::Arc;

struct MongoCatalog(Arc<MongoStorage>);
impl ScopeCatalog for MongoCatalog {
    fn namespaces(&self, scope: &StorageScope) -> Result<Vec<crate::Namespace>, StorageError> {
        super::codec::namespaces(self.0.namespace_names()?, scope)
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        super::codec::scopes(self.0.namespace_names()?, domain)
    }
}

pub struct MongoPartitionDataSource(CollectionDataSource);
impl MongoPartitionDataSource {
    pub fn open(storage: Arc<MongoStorage>) -> Result<Self, StorageError> {
        storage.reject_legacy_entity_layout()?;
        let names = storage.namespace_names()?;
        let domains: std::collections::BTreeSet<_> = names
            .iter()
            .filter_map(|name| name.split_once("__").map(|(domain, _)| domain))
            .collect();
        for domain in domains {
            super::codec::scopes(names.clone(), domain)?;
        }
        Ok(Self(CollectionDataSource::new(
            storage.clone(),
            storage.clone(),
            Arc::new(MongoCatalog(storage)),
        )))
    }
}
impl PartitionReadSource for MongoPartitionDataSource {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        self.0.open_reader(scope)
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        self.0.list_scopes(domain)
    }
}
impl PartitionDataSource for MongoPartitionDataSource {
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        self.0.commit(batch)
    }
    fn verify_write_capability(&self) -> Result<(), StorageError> {
        self.0.verify_write_capability()
    }
}
