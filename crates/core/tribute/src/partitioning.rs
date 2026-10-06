//! Tribute owns WWD placement and the independently retained shared population.

use outbe_offchain_storage::partitioned::routing::{
    DayPrefixRouting, RoutingRegistry, SharedRouting,
};
use outbe_offchain_storage::{
    PartitionContext, PartitionStrategy, StorageError, StorageScope, WorldwideDayPartition,
};
use std::sync::Arc;

pub fn register(registry: &mut RoutingRegistry) -> Result<(), StorageError> {
    let day = Arc::new(DayPrefixRouting::new("tribute", 0)?);
    registry.register("tributes", day.clone())?;
    registry.register("tributes_by_day", day)?;
    registry.register(
        "tributes_by_owner",
        Arc::new(DayPrefixRouting::new("tribute", 20)?),
    )?;
    let shared = Arc::new(SharedRouting(StorageScope::shared("tribute")?));
    for namespace in [
        crate::TRIBUTE_DAY_MARK_NAMESPACE,
        crate::OCOMP_RETAINED_TRIBUTES_NAMESPACE,
        crate::OCOMP_RETAINED_TRIBUTES_BY_DAY_NAMESPACE,
    ] {
        registry.register(namespace, shared.clone())?;
    }
    Ok(())
}

pub fn day_scope(day: u32) -> Result<StorageScope, StorageError> {
    StorageScope::new(
        "tribute",
        WorldwideDayPartition.partition(&PartitionContext {
            entity_id: &[],
            worldwide_day: Some(day),
            owner: None,
        })?,
    )
}
