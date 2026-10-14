use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::errors::surface_query_error;
use crate::math::{DECIMALS, MAX_LEAGUE, MIN_LEAGUE};
use crate::schema::FidelityContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/IFidelity.sol");

/// Dispatches an ABI-encoded call to the Fidelity precompile.
///
/// `getFidelityIndex`/`getFidelityIndexAt` are owner-authorized reads over the
/// encrypted cohort ledger (the enclave verifies the signed authorization).
/// `maxFidelityIndexAt`/`decimals`/`minLeague`/`maxLeague` are plaintext.
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
            getFidelityIndex(c) => view(c, |c| {
                contract
                    .query_index_now(c.account, c.expiry, c.signature.to_vec())
                    .map(|r| r.rcfi)
                    .map_err(surface_query_error)
            }),
            getFidelityIndexAt(c) => view(c, |c| {
                contract
                    .query_index_at(c.account, c.timestamp, c.expiry, c.signature.to_vec())
                    .map(|r| r.rcfi)
                    .map_err(surface_query_error)
            }),
            decimals(_) => metadata::<IFidelity::decimalsCall>(|| Ok(DECIMALS)),
            maxFidelityIndexAt(c) => view(c, |c| contract.max_rcfi_at(c.timestamp)),
            minLeague(_) => metadata::<IFidelity::minLeagueCall>(|| Ok(MIN_LEAGUE)),
            maxLeague(_) => metadata::<IFidelity::maxLeagueCall>(|| Ok(MAX_LEAGUE)),
        }
    })
}
