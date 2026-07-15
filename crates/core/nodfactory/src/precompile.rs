use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate, preflight_dynamic_bytes_len};
use outbe_primitives::error::Result;

use crate::runtime;
use outbe_compressed_entities::{EntityId36, ExecutionScope, ParentBodySource};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INodFactory.sol"
);

/// Dispatches NodFactory calls through the block-scoped compressed-body lifecycle.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    preflight_entity_id(data)?;
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            mineGratis(c) => mutate(c, caller, |sender, c| {
                runtime::mine_gratis(
                    &storage,
                    scope,
                    parent,
                    sender,
                    parse_entity_id(&c.nodId)?,
                    c.nonce,
                    c.asset,
                )
            }),
        }
    })
}

fn preflight_entity_id(data: &[u8]) -> Result<()> {
    preflight_dynamic_bytes_len(
        data,
        INodFactory::mineGratisCall::SELECTOR,
        0,
        3,
        EntityId36::LEN,
    )
}

fn parse_entity_id(bytes: &Bytes) -> Result<EntityId36> {
    EntityId36::try_from(bytes.as_ref())
        .map_err(|error| outbe_primitives::error::PrecompileError::Revert(error.to_string()))
}
