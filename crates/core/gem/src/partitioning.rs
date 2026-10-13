//! Gem bodies use id-derived shards. The shared index lists ids by owner.

use alloy_primitives::U256;
use outbe_compressed_entities::WwdEntityId;
use outbe_offchain_storage::partitioned::routing::{RoutingRegistry, SharedRouting};
use outbe_offchain_storage::partitioned::{PartitionRouting, ReadLocation};
use outbe_offchain_storage::{
    Key, Namespace, PartitionId, PartitionReadSource, ScanRequest, StorageError, StorageScope,
};
use std::sync::Arc;

pub const GEM_SHARD_COUNT: u16 = 256;
pub const GEM_SHARD_FAMILY: &str = "gem-shards";

pub fn item_shard(gem_id: WwdEntityId) -> u32 {
    (gem_id.to_u256() % U256::from(GEM_SHARD_COUNT)).to::<u32>()
}

pub fn item_scope(gem_id: WwdEntityId) -> Result<StorageScope, StorageError> {
    StorageScope::numbered("gem", GEM_SHARD_FAMILY, item_shard(gem_id))
}

pub fn register(registry: &mut RoutingRegistry) -> Result<(), StorageError> {
    registry.register("gems", Arc::new(GemItemRouting))?;
    let shared = Arc::new(SharedRouting(StorageScope::shared("gem")?));
    registry.register("gems_by_owner", shared.clone())?;
    registry.register("gem_buckets", shared)
}

struct GemItemRouting;

impl PartitionRouting for GemItemRouting {
    fn point(
        &self,
        _: &Namespace,
        key: &Key,
        _: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        let gem_id = WwdEntityId::try_from(key.as_bytes())
            .map_err(|_| StorageError::Corruption("invalid gem primary key width".into()))?;
        Ok(Some(ReadLocation {
            scope: item_scope(gem_id)?,
            require_present: false,
        }))
    }

    fn scan(
        &self,
        _: &Namespace,
        _: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        Ok(source
            .list_scopes("gem")?
            .into_iter()
            .filter(|scope| {
                matches!(&scope.partition, PartitionId::Numbered { family, .. } if family == GEM_SHARD_FAMILY)
            })
            .collect())
    }
}
