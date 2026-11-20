//! Promisfactory precompile at `0x2337`. This file does ABI dispatch only. Promis
//! mint and burn orchestration lives in [`crate::runtime`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::api::ModifyAuth;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/IPromisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IPromisFactory::IPromisFactoryCalls::abi_decode,
        |call| {
            use IPromisFactory::IPromisFactoryCalls::*;
            match call {
                mineCoen(c) => mutate(&storage, c, caller, |sender, c| {
                    let auth = ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_coen(storage.clone(), sender, c.promisMinor, auth)
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
