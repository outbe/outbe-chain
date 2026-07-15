use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate};
use outbe_primitives::error::Result;

use crate::runtime;
use outbe_nod::NodRepositoryReader;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/INodFactory.sol"
);

/// Dispatches an ABI-encoded call to the NodFactory precompile.
///
/// Only `mineGratis` is exposed on the ABI; issuance is a privileged
/// cross-module call from Lysis through [`crate::api::issue_nod`].
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            mineGratis(c) => mutate(c, caller, |sender, c| {
                runtime::mine_gratis(&storage, sender, c.nodId, c.nonce, c.asset)
            }),
        }
    })
}

/// Dispatches NodFactory calls with the least-authority off-chain body reader.
pub fn dispatch_with_reader(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
    reader: &NodRepositoryReader,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, INodFactory::INodFactoryCalls::abi_decode, |call| {
        use INodFactory::INodFactoryCalls::*;
        match call {
            mineGratis(c) => mutate(c, caller, |sender, c| {
                runtime::mine_gratis_with_reader(
                    &storage, reader, sender, c.nodId, c.nonce, c.asset,
                )
            }),
        }
    })
}
