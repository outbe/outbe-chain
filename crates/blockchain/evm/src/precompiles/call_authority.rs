//! Binds EVM command identity to native Metadosis mutation capabilities.
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS},
    storage::{
        metadosis_advance_due_binding, metadosis_cycle_allocation_binding,
        metadosis_init_genesis_binding, metadosis_late_settlement_binding,
        metadosis_process_ready_binding, MetadosisMutationEntitlements,
        MetadosisMutationPurposeTag,
    },
};

pub(super) struct MetadosisMutationCall<'a> {
    pub(super) address: Address,
    pub(super) data: &'a [u8],
    pub(super) caller: Address,
    pub(super) is_static: bool,
    pub(super) value: U256,
    pub(super) chain_id: u64,
    pub(super) block_number: u64,
    pub(super) timestamp: u64,
    pub(super) cycle_active_utc_day: Option<u32>,
}

pub(super) fn metadosis_mutation_entitlements(
    call: MetadosisMutationCall<'_>,
) -> MetadosisMutationEntitlements {
    use MetadosisMutationPurposeTag as Purpose;
    if call.is_static
        || !call.value.is_zero()
        || call.address != OUTBE_SYSTEM_TX_ADDRESS
        || call.caller != SYSTEM_ADDRESS
    {
        return MetadosisMutationEntitlements::NONE;
    }
    let Ok(input) = crate::system_tx::SystemTxInputV2::decode(call.data) else {
        return MetadosisMutationEntitlements::NONE;
    };
    match input {
        crate::system_tx::SystemTxInputV2::LateFinalizeCredits { .. } => {
            MetadosisMutationEntitlements::exact(
                Purpose::CertifiedFinality,
                metadosis_late_settlement_binding(call.chain_id, call.block_number, call.timestamp),
            )
        }
        crate::system_tx::SystemTxInputV2::CycleTick => cycle_tick_entitlements(&call),
        _ => MetadosisMutationEntitlements::NONE,
    }
}

fn cycle_tick_entitlements(call: &MetadosisMutationCall<'_>) -> MetadosisMutationEntitlements {
    use MetadosisMutationPurposeTag as Purpose;
    let chain_id = call.chain_id;
    let block_number = call.block_number;
    let timestamp = call.timestamp;
    let cycle_active_utc_day = call.cycle_active_utc_day;
    let mut entitlements = MetadosisMutationEntitlements::exact(
        Purpose::CertifiedFinality,
        metadosis_late_settlement_binding(chain_id, block_number, timestamp),
    );
    if block_number == 1 {
        entitlements = entitlements.union(MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_init_genesis_binding(chain_id, block_number, timestamp),
        ));
    }
    let Some(active_utc_day) = cycle_active_utc_day else {
        return entitlements;
    };
    let block_utc_day = outbe_primitives::time::timestamp_to_date_key(timestamp);
    let Ok(day_action) = outbe_cycle::handler::protocol_day_action(active_utc_day, block_utc_day)
    else {
        return entitlements;
    };
    if let outbe_cycle::handler::ProtocolDayAction::SettlePrevious { day } = day_action {
        if let Ok(day_ts) = outbe_primitives::time::date_key_to_utc_timestamp(day) {
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

/// The Cycle cursor prepares capabilities for a protocol command, not for any
/// user payload that happens to share its encoding. Check identity before decode.
pub(super) fn is_protocol_cycle_call(address: Address, caller: Address, data: &[u8]) -> bool {
    address == OUTBE_SYSTEM_TX_ADDRESS
        && caller == SYSTEM_ADDRESS
        && matches!(
            crate::system_tx::SystemTxInputV2::decode(data),
            Ok(crate::system_tx::SystemTxInputV2::CycleTick)
        )
}
