//! Pure, reusable partition strategies. Domain code supplies routing attributes.

use super::PartitionId;
use crate::StorageError;

pub struct PartitionContext<'a> {
    pub entity_id: &'a [u8],
    pub worldwide_day: Option<u32>,
    pub owner: Option<[u8; 20]>,
}

pub trait PartitionStrategy: Send + Sync {
    fn partition(&self, context: &PartitionContext<'_>) -> Result<PartitionId, StorageError>;
}

pub struct SharedPartition;
impl PartitionStrategy for SharedPartition {
    fn partition(&self, _: &PartitionContext<'_>) -> Result<PartitionId, StorageError> {
        Ok(PartitionId::Shared)
    }
}

pub struct WorldwideDayPartition;
impl PartitionStrategy for WorldwideDayPartition {
    fn partition(&self, context: &PartitionContext<'_>) -> Result<PartitionId, StorageError> {
        let index = context
            .worldwide_day
            .ok_or_else(|| StorageError::InvalidArgument("WWD strategy requires a day".into()))?;
        Ok(PartitionId::Numbered {
            family: "wwd".into(),
            index,
        })
    }
}

pub struct OwnerModuloPartition {
    family: String,
    partitions: u16,
}
impl OwnerModuloPartition {
    pub fn new(family: &str, partitions: u16) -> Result<Self, StorageError> {
        super::StorageScope::numbered("validation", family, 0)?;
        if partitions == 0 {
            return Err(StorageError::InvalidArgument(
                "owner modulo requires nonzero partition count".into(),
            ));
        }
        Ok(Self {
            family: family.to_owned(),
            partitions,
        })
    }
}
impl PartitionStrategy for OwnerModuloPartition {
    fn partition(&self, context: &PartitionContext<'_>) -> Result<PartitionId, StorageError> {
        let owner = context.owner.ok_or_else(|| {
            StorageError::InvalidArgument("owner modulo requires an owner".into())
        })?;
        let modulus = u32::from(self.partitions);
        let index = owner.iter().fold(0_u32, |remainder, byte| {
            (remainder * 256 + u32::from(*byte)) % modulus
        });
        Ok(PartitionId::Numbered {
            family: self.family.clone(),
            index,
        })
    }
}
