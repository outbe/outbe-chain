use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, view};
use outbe_primitives::error::Result;

use crate::schema::FidelityContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IFidelity.sol"
);

/// Dispatches an ABI-encoded call to the Fidelity precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IFidelity::IFidelityCalls::abi_decode, |call| {
        let contract = FidelityContract::new(storage);
        use IFidelity::IFidelityCalls::*;
        match call {
            getFidelityIndex(c) => view(c, |c| contract.get_fidelity_index(c.account)),
            getRcfi(c) => view(c, |c| contract.get_rcfi(c.account)),
        }
    })
}
