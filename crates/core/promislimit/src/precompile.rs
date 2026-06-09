use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata};
use outbe_primitives::error::Result;

use crate::schema::PromisLimitContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IPromisLimit.sol"
);

/// Dispatches an ABI-encoded call to the PromisLimit precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IPromisLimit::IPromisLimitCalls::abi_decode, |call| {
        let contract = PromisLimitContract::new(storage);
        use IPromisLimit::IPromisLimitCalls::*;
        match call {
            totalUnallocated(_) => {
                metadata::<IPromisLimit::totalUnallocatedCall>(|| contract.get_total_unallocated())
            }
        }
    })
}
