//! Registration and reusable key/index routing rules, without entity names.

mod located;
mod prefix;
use super::{PartitionReadSource, PartitionRouting, ReadLocation, StorageScope};
use crate::{Key, Namespace, ScanRequest, StorageError};
pub use located::LocatedRouting;
pub use prefix::{DayPrefixRouting, OwnerPrefixRouting, SharedRouting};
use std::{collections::BTreeMap, sync::Arc};

pub struct RoutingRegistry {
    rules: BTreeMap<String, Arc<dyn PartitionRouting>>,
    fallback: Arc<dyn PartitionRouting>,
}
impl RoutingRegistry {
    #[must_use]
    pub fn new(fallback: Arc<dyn PartitionRouting>) -> Self {
        Self {
            rules: BTreeMap::new(),
            fallback,
        }
    }
    pub fn register(
        &mut self,
        namespace: &str,
        routing: Arc<dyn PartitionRouting>,
    ) -> Result<(), StorageError> {
        Namespace::new(namespace)?;
        if self.rules.contains_key(namespace) {
            return Err(StorageError::InvalidArgument(
                "namespace partition rule is already registered".into(),
            ));
        }
        self.rules.insert(namespace.to_owned(), routing);
        Ok(())
    }
    fn rule(&self, namespace: &Namespace) -> &dyn PartitionRouting {
        self.rules
            .get(namespace.as_str())
            .unwrap_or(&self.fallback)
            .as_ref()
    }
}
impl PartitionRouting for RoutingRegistry {
    fn points(
        &self,
        namespace: &Namespace,
        keys: &[Key],
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<Option<ReadLocation>>, StorageError> {
        self.rule(namespace).points(namespace, keys, source)
    }

    fn point(
        &self,
        namespace: &Namespace,
        key: &Key,
        source: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        self.rule(namespace).point(namespace, key, source)
    }
    fn scan(
        &self,
        namespace: &Namespace,
        request: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        self.rule(namespace).scan(namespace, request, source)
    }
}
