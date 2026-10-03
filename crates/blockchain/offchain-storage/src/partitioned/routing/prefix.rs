use super::super::{
    OwnerModuloPartition, PartitionContext, PartitionId, PartitionReadSource, PartitionRouting,
    PartitionStrategy, ReadLocation, StorageScope, WorldwideDayPartition,
};
use crate::{Key, Namespace, ScanRequest, StorageError};

pub struct SharedRouting(pub StorageScope);
impl PartitionRouting for SharedRouting {
    fn point(
        &self,
        _: &Namespace,
        _: &Key,
        _: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        Ok(Some(ReadLocation {
            scope: self.0.clone(),
            require_present: false,
        }))
    }
    fn scan(
        &self,
        _: &Namespace,
        _: ScanRequest<'_>,
        _: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        Ok(vec![self.0.clone()])
    }
}

pub struct DayPrefixRouting {
    domain: String,
    offset: usize,
}
impl DayPrefixRouting {
    pub fn new(domain: &str, offset: usize) -> Result<Self, StorageError> {
        StorageScope::shared(domain)?;
        Ok(Self {
            domain: domain.to_owned(),
            offset,
        })
    }
    fn scope(&self, bytes: &[u8]) -> Result<StorageScope, StorageError> {
        let raw: [u8; 4] = bytes
            .get(self.offset..self.offset + 4)
            .ok_or_else(|| StorageError::Corruption("missing WWD routing prefix".into()))?
            .try_into()
            .map_err(|_| StorageError::Corruption("invalid WWD routing prefix".into()))?;
        StorageScope::new(
            &self.domain,
            WorldwideDayPartition.partition(&PartitionContext {
                entity_id: bytes,
                worldwide_day: Some(u32::from_be_bytes(raw)),
                owner: None,
            })?,
        )
    }
}
impl PartitionRouting for DayPrefixRouting {
    fn point(
        &self,
        _: &Namespace,
        key: &Key,
        _: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        Ok(Some(ReadLocation {
            scope: self.scope(key.as_bytes())?,
            require_present: false,
        }))
    }
    fn scan(
        &self,
        _: &Namespace,
        request: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        if request.prefix().len() >= self.offset + 4 {
            return Ok(vec![self.scope(request.prefix())?]);
        }
        Ok(source
            .list_scopes(&self.domain)?
            .into_iter()
            .filter(
                |s| matches!(&s.partition, PartitionId::Numbered { family, .. } if family == "wwd"),
            )
            .collect())
    }
}

pub struct OwnerPrefixRouting {
    domain: String,
    family: String,
    strategy: OwnerModuloPartition,
}
impl OwnerPrefixRouting {
    pub fn new(domain: &str, family: &str, partitions: u16) -> Result<Self, StorageError> {
        StorageScope::shared(domain)?;
        Ok(Self {
            domain: domain.to_owned(),
            family: family.to_owned(),
            strategy: OwnerModuloPartition::new(family, partitions)?,
        })
    }
    fn scope(&self, bytes: &[u8]) -> Result<StorageScope, StorageError> {
        let owner: [u8; 20] = bytes
            .get(..20)
            .ok_or_else(|| StorageError::Corruption("missing owner routing prefix".into()))?
            .try_into()
            .map_err(|_| StorageError::Corruption("invalid owner routing prefix".into()))?;
        StorageScope::new(
            &self.domain,
            self.strategy.partition(&PartitionContext {
                entity_id: &[],
                worldwide_day: None,
                owner: Some(owner),
            })?,
        )
    }
}
impl PartitionRouting for OwnerPrefixRouting {
    fn point(
        &self,
        _: &Namespace,
        key: &Key,
        _: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        Ok(Some(ReadLocation {
            scope: self.scope(key.as_bytes())?,
            require_present: false,
        }))
    }
    fn scan(
        &self,
        _: &Namespace,
        request: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        if request.prefix().len() >= 20 {
            return Ok(vec![self.scope(request.prefix())?]);
        }
        Ok(source.list_scopes(&self.domain)?.into_iter().filter(|s| matches!(&s.partition, PartitionId::Numbered { family, .. } if family == &self.family)).collect())
    }
}
