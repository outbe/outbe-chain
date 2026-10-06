//! Gratisfactory precompile at `0x2003`. This file does ABI dispatch only. The Gratis balance
//! movement and Fidelity bookkeeping live in [`crate::runtime`]. The caller's Gratis modify key
//! (`mac` and `opNonce`) authorizes pledge and mining writes.
//! A pledge-note proof authorizes `unpledgeGratis`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_gratis::api::ModifyAuth;
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
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
                pledgeGratis(c) => mutate(c, caller, |sender, c| {
                    runtime::pledge_gratis(
                        storage.clone(),
                        sender,
                        c.gratisMinor,
                        ModifyAuth {
                            mac: c.auth.mac.0,
                            op_nonce: c.auth.opNonce,
                        },
                    )
                }),
                unpledgeGratis(c) => mutate_void(c, caller, |_, c| {
                    runtime::unpledge_gratis(storage.clone(), &c.proof).map(|_| ())
                }),
                pledgeRoot(c) => view(c, |_| {
                    outbe_gratis::pledge::PledgePool::new(storage.clone())
                        .current_root
                        .read()
                }),
                pledgeLeafCount(c) => view(c, |_| {
                    outbe_gratis::pledge::PledgePool::new(storage.clone())
                        .leaf_count
                        .read()
                }),
                pledgeSpent(c) => view(c, |c| {
                    outbe_gratis::pledge::PledgePool::new(storage.clone())
                        .spent_nullifiers
                        .read(&c.nullifier)
                }),
                mineCoen(c) => mutate(c, caller, |sender, c| {
                    let auth = ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_coen(storage.clone(), sender, c.gratisMinor, auth)
                }),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
