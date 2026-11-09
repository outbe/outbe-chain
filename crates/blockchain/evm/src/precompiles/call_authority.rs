//! Binds EVM command identity to native Metadosis mutation capabilities.
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS},
    storage::MetadosisMutationEntitlements,
};

pub struct MetadosisMutationCall<'a> {
    pub address: Address,
    pub data: &'a [u8],
    pub caller: Address,
    pub is_static: bool,
    pub value: U256,
    pub chain_id: u64,
    pub block_number: u64,
    pub timestamp: u64,
    pub cycle_active_utc_day: Option<u32>,
}

pub(super) fn metadosis_mutation_entitlements(
    call: MetadosisMutationCall<'_>,
) -> MetadosisMutationEntitlements {
    crate::active_evm_runtime_host().mutation_entitlements(call)
}

/// The Cycle cursor prepares capabilities for a protocol command, not for any
/// user payload that happens to share its encoding. Check identity before decode.
pub(super) fn is_protocol_cycle_call(address: Address, caller: Address, data: &[u8]) -> bool {
    address == OUTBE_SYSTEM_TX_ADDRESS
        && caller == SYSTEM_ADDRESS
        && matches!(
            crate::system_tx::SystemTx::decode(data),
            Ok(crate::system_tx::SystemTx::CycleTick)
        )
}
