use super::super::{
    PartitionId, PartitionReadSource, PartitionRouting, ReadLocation, StorageScope,
};
use crate::{Key, Namespace, ScanRequest, StorageError};

/// Lookup by an entity-owned shared locator; primary scans remain independent.
pub struct LocatedRouting {
    shared: StorageScope,
    namespace: Namespace,
    family: String,
    partitions: u16,
}
impl LocatedRouting {
    pub fn new(
        domain: &str,
        namespace: &str,
        family: &str,
        partitions: u16,
    ) -> Result<Self, StorageError> {
        super::super::OwnerModuloPartition::new(family, partitions)?;
        Ok(Self {
            shared: StorageScope::shared(domain)?,
            namespace: Namespace::new(namespace)?,
            family: family.to_owned(),
            partitions,
        })
    }
}
impl PartitionRouting for LocatedRouting {
    fn point(
        &self,
        _: &Namespace,
        key: &Key,
        source: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        Ok(self
            .points(&self.namespace, std::slice::from_ref(key), source)?
            .pop()
            .flatten())
    }
    fn points(
        &self,
        _: &Namespace,
        keys: &[Key],
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<Option<ReadLocation>>, StorageError> {
        let Some(reader) = source.open_reader(&self.shared)? else {
            return Ok(vec![None; keys.len()]);
        };
        let records = reader.get_records(self.namespace.clone(), keys)?;
        if records.len() != keys.len() {
            return Err(StorageError::Corruption(
                "locator batch cardinality mismatch".into(),
            ));
        }
        records
            .into_iter()
            .map(|record| {
                let Some(record) = record else {
                    return Ok(None);
                };
                let raw: [u8; 4] = record
                    .value
                    .as_bytes()
                    .try_into()
                    .map_err(|_| StorageError::Corruption("malformed entity locator".into()))?;
                let index = u32::from_be_bytes(raw);
                if record.metadata.is_some() || index >= u32::from(self.partitions) {
                    return Err(StorageError::Corruption("invalid entity locator".into()));
                }
                Ok(Some(ReadLocation {
                    scope: StorageScope::numbered(&self.shared.domain, &self.family, index)?,
                    require_present: true,
                }))
            })
            .collect()
    }
    fn scan(
        &self,
        _: &Namespace,
        _: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        Ok(source.list_scopes(&self.shared.domain)?.into_iter().filter(|s| matches!(&s.partition, PartitionId::Numbered { family, .. } if family == &self.family)).collect())
    }
}
