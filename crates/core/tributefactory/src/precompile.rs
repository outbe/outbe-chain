use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate};
use outbe_primitives::error::Result;

use crate::schema::TributeFactoryContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ITributeFactory.sol"
);

/// Dispatch for the tribute factory precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        ITributeFactory::ITributeFactoryCalls::abi_decode,
        |call| {
            use ITributeFactory::ITributeFactoryCalls::*;
            match call {
                offerTribute(c) => mutate(c, caller, |sender, c| {
                    let mut factory = TributeFactoryContract::new(storage);
                    factory.offer_tribute(
                        sender,
                        &c.cipherText,
                        &c.nonce,
                        c.ephemeralPubkey,
                        c.referenceCurrency,
                    )
                }),
            }
        },
    )
}
