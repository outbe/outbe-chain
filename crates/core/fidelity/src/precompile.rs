use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::math::DECIMALS;
use crate::runtime::{MAX_LEAGUE, MIN_LEAGUE};
use crate::schema::FidelityContract;

sol!("../../../contracts/precompiles/src/IFidelity.sol");

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
            getRcfi(c) => view(c, |c| contract.get_rcfi_scaled(c.account)),
            getRcfiAt(c) => view(c, |c| contract.compute_rcfi_scaled(c.account, c.timestamp)),
            decimals(_) => metadata::<IFidelity::decimalsCall>(|| Ok(DECIMALS)),
            maxRcfiAt(c) => view(c, |c| contract.max_rcfi_at(c.timestamp)),
            minLeague(_) => metadata::<IFidelity::minLeagueCall>(|| Ok(MIN_LEAGUE)),
            maxLeague(_) => metadata::<IFidelity::maxLeagueCall>(|| Ok(MAX_LEAGUE)),
            league(c) => view(c, |c| contract.league(c.account)),
        }
    })
}
