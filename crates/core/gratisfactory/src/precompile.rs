//! Gratisfactory precompile at `0x2003`. ABI dispatch only - the Gratis balance
//! movement + Fidelity bookkeeping lives in [`crate::runtime`]. Writes are
//! authorized by the caller's Gratis modify key (`mac` + `opNonce`).

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_gratis::api::ModifyAuth;
use outbe_primitives::dispatch::{dispatch_call, mutate, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/IGratisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IGratisFactory::IGratisFactoryCalls::abi_decode,
        |call| {
            use IGratisFactory::IGratisFactoryCalls::*;
            match call {
                createPledgeNote(c) => mutate(c, caller, |_sender, c| {
                    runtime::create_pledge_note(storage.clone(), &c.request).map(Bytes::from)
                }),
                cancelPledgeNote(c) => mutate(c, caller, |_sender, c| {
                    runtime::cancel_pledge_note(storage.clone(), c.encryptedAuth.to_vec())
                        .map(Bytes::from)
                }),
                mineCoen(c) => mutate(c, caller, |sender, c| {
                    let auth = ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_coen(storage.clone(), sender, c.amount, auth)
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
