//! ABI dispatch for the IntexFactory precompile at `INTEX_FACTORY_ADDRESS`.
//!
//! Routing only: decode -> runtime -> encode. `settle` / `minePromis` /
//! `setAuthorizedSettler` are user-facing with `caller = msg.sender`. None
//! accept value.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IIntexFactory.sol"
);

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IIntexFactory::IIntexFactoryCalls::abi_decode,
        |call| {
            use IIntexFactory::IIntexFactoryCalls::*;
            match call {
                settle(c) => mutate_void(c, caller, |sender, c| {
                    runtime::settle(&storage, c.seriesId, c.intexHolder, sender, c.amount)
                }),
                // Off-chain the holder brute-forces `nonce` so the work hash
                // SHA256(hex(holder ++ promisAmount ++ seriesId ++ seq) ++ nonce_be8)
                // has POW_DIFFICULTY leading zero bytes; `seq` is the on-chain
                // per-(series, holder) counter.
                minePromis(c) => mutate(c, caller, |sender, c| {
                    runtime::mine_promis(&storage, c.seriesId, sender, c.amount, c.nonce)
                }),
                setAuthorizedSettler(c) => mutate_void(c, caller, |sender, c| {
                    runtime::set_authorized_settler(&storage, sender, c.seriesId, c.settler)
                }),
            }
        },
    )
}
