//! Promisfactory precompile at `0x2337`. ABI dispatch only — the promis
//! mint/burn orchestration + Fidelity bookkeeping lives in [`crate::runtime`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_gratisfactory::api::ModifyAuth;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

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
                mineCoen(c) => mutate(c, caller, |sender, c| {
                    runtime::mine_coen(storage.clone(), sender, c.amount)
                }),
                convertToGratis(c) => mutate(c, caller, |sender, c| {
                    let auth = ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::convert_to_gratis(storage.clone(), sender, c.amount, auth)
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
