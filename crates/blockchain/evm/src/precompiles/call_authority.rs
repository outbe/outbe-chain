//! Binds EVM command identity to native scoped mutation capabilities.
use alloy_primitives::{Address, U256};
use outbe_primitives::storage::MutationEntitlements;

pub struct PrecompileMutationCall<'a> {
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

pub(super) fn precompile_mutation_entitlements(
    host: &dyn crate::EvmRuntimeHost,
    call: PrecompileMutationCall<'_>,
) -> MutationEntitlements {
    host.mutation_entitlements(call)
}
