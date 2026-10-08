//! NOD bodies use ID-derived shards. The shared index lists IDs by owner.

use alloy_primitives::U256;
use outbe_compressed_entities::WwdEntityId;
use outbe_offchain_storage::partitioned::routing::{RoutingRegistry, SharedRouting};
use outbe_offchain_storage::partitioned::{PartitionRouting, ReadLocation};
use outbe_offchain_storage::{
    Key, Namespace, PartitionId, PartitionReadSource, ScanRequest, StorageError, StorageScope,
};
use std::sync::Arc;

pub const NOD_SHARD_COUNT: u16 = 256;
pub const NOD_SHARD_FAMILY: &str = "nod-shards";

pub fn item_shard(nod_id: WwdEntityId) -> u32 {
    (nod_id.to_u256() % U256::from(NOD_SHARD_COUNT)).to::<u32>()
}

pub fn item_scope(nod_id: WwdEntityId) -> Result<StorageScope, StorageError> {
    StorageScope::numbered("nod", NOD_SHARD_FAMILY, item_shard(nod_id))
}

pub fn register(registry: &mut RoutingRegistry) -> Result<(), StorageError> {
    registry.register("nods", Arc::new(NodItemRouting))?;
    let shared = Arc::new(SharedRouting(StorageScope::shared("nod")?));
    registry.register("nods_by_owner", shared.clone())?;
    registry.register("nod_buckets", shared)
}

struct NodItemRouting;

impl PartitionRouting for NodItemRouting {
    fn point(
        &self,
        _: &Namespace,
        key: &Key,
        _: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        let nod_id = WwdEntityId::try_from(key.as_bytes())
            .map_err(|_| StorageError::Corruption("invalid NOD primary key width".into()))?;
        Ok(Some(ReadLocation {
            scope: item_scope(nod_id)?,
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
            .list_scopes("nod")?
            .into_iter()
            .filter(|scope| {
                matches!(&scope.partition, PartitionId::Numbered { family, .. } if family == NOD_SHARD_FAMILY)
            })
            .collect())
    }
}
