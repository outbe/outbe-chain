use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, view};
use outbe_primitives::error::Result;

use crate::schema::WorldwideDayContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IWorldwideDay.sol"
);

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IWorldwideDay::IWorldwideDayCalls::abi_decode,
        |call| {
            use IWorldwideDay::IWorldwideDayCalls::*;
            match call {
                getWorldwideDay(c) => view(c, |c| {
                    crate::queries::worldwide_day(&WorldwideDayContract::new(storage.clone()), c)
                }),
                activeWorldwideDays(c) => view(c, |_c| {
                    Ok(crate::api::offering_worldwide_days(storage.clone())?
                        .into_iter()
                        .map(u32::from)
                        .collect::<Vec<_>>())
                }),
            }
        },
    )
}
