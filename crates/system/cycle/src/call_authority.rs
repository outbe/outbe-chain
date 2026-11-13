//! Metadosis mutation entitlements for protocol system transactions.

use outbe_primitives::error::Result;
use outbe_primitives::storage::{
    metadosis_advance_due_binding, metadosis_cycle_allocation_binding,
    metadosis_init_genesis_binding, metadosis_process_ready_binding, MetadosisMutationEntitlements,
    MetadosisMutationPurposeTag,
};
use outbe_system_common::time::{date_key_to_utc_timestamp, timestamp_to_date_key};

use crate::handler;

/// Entitlements carried by the protocol cycle step.
pub fn cycle_tick_entitlements(
    chain_id: u64,
    block_number: u64,
    timestamp: u64,
    cycle_active_utc_day: Option<u32>,
) -> MetadosisMutationEntitlements {
    use MetadosisMutationPurposeTag as Purpose;
    let mut entitlements = MetadosisMutationEntitlements::NONE;
    if block_number == 1 {
        entitlements = entitlements.union(MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_init_genesis_binding(chain_id, block_number, timestamp),
        ));
    }
    let Some(active_utc_day) = cycle_active_utc_day else {
        return entitlements;
    };
    let block_utc_day = timestamp_to_date_key(timestamp);
    let Ok(day_to_allocate) = handler::day_to_allocate(active_utc_day, block_utc_day) else {
        return entitlements;
    };
    if let Some(day) = day_to_allocate {
        if let Ok(day_ts) = date_key_to_utc_timestamp(day) {
            entitlements = entitlements.union(MetadosisMutationEntitlements::exact(
                Purpose::CycleLifecycle,
                metadosis_cycle_allocation_binding(chain_id, block_number, day_ts),
            ));
        }
    }
    entitlements
        .union(MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_advance_due_binding(chain_id, block_number, timestamp),
        ))
        .union(MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_process_ready_binding(chain_id, block_number, timestamp),
        ))
}

/// Active UTC day for a protocol-cycle precompile call.
pub fn cycle_call_active_utc_day(
    storage: outbe_primitives::storage::StorageHandle<'_>,
    genesis_timestamp: u64,
    block_timestamp: u64,
) -> Result<u32> {
    handler::derive_active_utc_day(storage, genesis_timestamp, block_timestamp)
}
