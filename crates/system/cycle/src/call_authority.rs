//! Metadosis mutation entitlements for protocol system transactions.

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::storage::{
    metadosis_advance_due_binding, metadosis_cycle_allocation_binding,
    metadosis_init_genesis_binding, metadosis_late_settlement_binding,
    metadosis_process_ready_binding, MetadosisMutationEntitlements, MetadosisMutationPurposeTag,
};
use outbe_primitives::system_tx::SystemTx;
use outbe_primitives::time::{date_key_to_utc_timestamp, timestamp_to_date_key};

use crate::handler::{self, ProtocolDayAction};

/// Identity of one precompile call that may mutate Metadosis.
pub struct MetadosisMutationCall<'a> {
    /// Precompile address.
    pub address: Address,
    /// Calldata.
    pub data: &'a [u8],
    /// Recovered caller.
    pub caller: Address,
    /// Static-call flag.
    pub is_static: bool,
    /// Native value attached to the call.
    pub value: U256,
    /// Chain id.
    pub chain_id: u64,
    /// Block height.
    pub block_number: u64,
    /// Block timestamp.
    pub timestamp: u64,
    /// Active UTC day when the caller already derived it.
    pub cycle_active_utc_day: Option<u32>,
}

/// Entitlements for a system-transaction call.
///
/// A call that is static, carries value, or is not the system caller at the
/// system-tx address receives no entitlements.
pub fn metadosis_mutation_entitlements(
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
    let Ok(input) = SystemTx::decode(call.data) else {
        return MetadosisMutationEntitlements::NONE;
    };
    match input {
        SystemTx::LateFinalizeCredits { .. } => MetadosisMutationEntitlements::exact(
            Purpose::CertifiedFinality,
            metadosis_late_settlement_binding(call.chain_id, call.block_number, call.timestamp),
        ),
        SystemTx::CycleTick => cycle_tick_entitlements(&call),
        SystemTx::NodFactory
        | SystemTx::GemFactory
        | SystemTx::IntexFactory
        | SystemTx::CredisFactory
        | SystemTx::GemCleanup => MetadosisMutationEntitlements::exact(
            Purpose::CycleLifecycle,
            metadosis_process_ready_binding(call.chain_id, call.block_number, call.timestamp),
        ),
        _ => MetadosisMutationEntitlements::NONE,
    }
}

/// Entitlements carried by `CycleTick`.
pub fn cycle_tick_entitlements(call: &MetadosisMutationCall<'_>) -> MetadosisMutationEntitlements {
    use MetadosisMutationPurposeTag as Purpose;
    let chain_id = call.chain_id;
    let block_number = call.block_number;
    let timestamp = call.timestamp;
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
    let Some(active_utc_day) = call.cycle_active_utc_day else {
        return entitlements;
    };
    let block_utc_day = timestamp_to_date_key(timestamp);
    let Ok(day_action) = handler::protocol_day_action(active_utc_day, block_utc_day) else {
        return entitlements;
    };
    if let ProtocolDayAction::SettlePrevious { day } = day_action {
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

/// True when `data` is a `CycleTick` from the system caller at the system-tx address.
pub fn is_protocol_cycle_call(address: Address, caller: Address, data: &[u8]) -> bool {
    address == OUTBE_SYSTEM_TX_ADDRESS
        && caller == SYSTEM_ADDRESS
        && matches!(SystemTx::decode(data), Ok(SystemTx::CycleTick))
}
