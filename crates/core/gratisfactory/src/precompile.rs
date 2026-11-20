//! Gratisfactory precompile at `0x2003`. This file does ABI dispatch only. Pledge,
//! cancel, `mineCoen`, and `mineGratis` run in [`crate::runtime`]. `mineCoen` emits
//! `CoenMinedFromGratis`; `mineGratis` is payable and emits `GratisMinedFromCoen`.
//! The caller's Gratis modify key (`mac` and `opNonce`) authorizes pledge and both
//! conversions. Only a pledge's source may cancel it.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};

use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_gratis::api::ModifyAuth;
use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_payable, mutate_void, reject_value_unless_payable, view,
    PayableCallContext,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[IGratisFactory::mineGratisCall::SELECTOR];

sol!("../../../contracts/precompiles/src/IGratisFactory.sol");

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    // A payable route: the boundary credits value to this address before dispatch.
    // Refuse it for every selector except `mineGratis`.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(
        data,
        IGratisFactory::IGratisFactoryCalls::abi_decode,
        |call| {
            use IGratisFactory::IGratisFactoryCalls::*;
            match call {
                pledgeGratis(c) => mutate_void(&storage, c, caller, |sender, c| {
                    runtime::pledge_gratis(
                        storage.clone(),
                        sender,
                        c.reservationId,
                        ModifyAuth {
                            mac: c.auth.mac.0,
                            op_nonce: c.auth.opNonce,
                        },
                    )
                }),
                cancelPledge(c) => mutate_void(&storage, c, caller, |sender, c| {
                    runtime::cancel_pledge(storage.clone(), sender, c.reservationId)
                }),
                collateralOf(c) => view(c, |c| {
                    let collateral = runtime::collateral_of(&storage, c.credisId)?;
                    Ok(IGratisFactory::collateralOfReturn {
                        source: collateral.source,
                        remainingMinor: collateral.remaining_minor,
                    })
                }),
                pledgeOf(c) => view(c, |c| {
                    let pledge = runtime::pledge_of(&storage, c.reservationId)?;
                    Ok(IGratisFactory::pledgeOfReturn {
                        source: pledge.source,
                        gratisMinor: pledge.gratis_minor,
                    })
                }),
                mineCoen(c) => mutate(&storage, c, caller, |sender, c| {
                    let auth = ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_coen(storage.clone(), sender, c.gratisMinor, auth)
                }),
                mineGratis(c) => mutate_payable(
                    &storage,
                    c,
                    PayableCallContext {
                        selectors: PAYABLE_SELECTORS,
                        sender: caller,
                        value,
                    },
                    |sender, c, val| {
                        let auth = ModifyAuth {
                            mac: c.mac.0,
                            op_nonce: c.opNonce,
                        };
                        runtime::mine_gratis(storage.clone(), sender, val, auth)
                    },
                ),
                supportsInterface(c) => view(c, |c| {
                    let id: [u8; 4] = c.interfaceId.0;
                    Ok(id == ERC165_INTERFACE_ID)
                }),
            }
        },
    )
}
