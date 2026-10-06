//! Nod owns its placement and lookup rules; datasources only implement scopes.

use alloy_primitives::Address;
use outbe_offchain_storage::partitioned::routing::{
    LocatedRouting, OwnerPrefixRouting, RoutingRegistry, SharedRouting,
};
use outbe_offchain_storage::{
    OwnerModuloPartition, PartitionContext, PartitionId, PartitionStrategy, StorageError,
    StorageScope,
};
use std::sync::Arc;

pub const NOD_LOCATIONS_NAMESPACE: &str = "nod_locations";
pub const NOD_SHARD_COUNT: u16 = 32;
pub const NOD_SHARD_FAMILY: &str = "nod-shards";

pub fn item_scope(owner: Address) -> Result<StorageScope, StorageError> {
    let strategy = OwnerModuloPartition::new(NOD_SHARD_FAMILY, NOD_SHARD_COUNT)?;
    StorageScope::new(
        "nod",
        strategy.partition(&PartitionContext {
            entity_id: &[],
            worldwide_day: None,
            owner: Some(owner.into_array()),
        })?,
    )
}

pub fn owner_shard(owner: Address) -> Result<u32, StorageError> {
    let PartitionId::Numbered { index, .. } = item_scope(owner)?.partition else {
        unreachable!("owner strategy always returns a numbered partition")
    };
    Ok(index)
}

pub fn register(registry: &mut RoutingRegistry) -> Result<(), StorageError> {
    registry.register(
        "nods",
        Arc::new(LocatedRouting::new(
            "nod",
            NOD_LOCATIONS_NAMESPACE,
            NOD_SHARD_FAMILY,
            NOD_SHARD_COUNT,
        )?),
    )?;
    registry.register(
        "nods_by_owner",
        Arc::new(OwnerPrefixRouting::new(
            "nod",
            NOD_SHARD_FAMILY,
            NOD_SHARD_COUNT,
        )?),
    )?;
    let shared = Arc::new(SharedRouting(StorageScope::shared("nod")?));
    registry.register("nod_buckets", shared.clone())?;
    registry.register(NOD_LOCATIONS_NAMESPACE, shared)
}
