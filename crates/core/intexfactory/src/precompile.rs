//! ABI dispatch for the IntexFactory precompile at `INTEX_FACTORY_ADDRESS`.
//!
//! Routing only: decode -> runtime -> encode. `settle` / `minePromis` /
//! `setAuthorizedSettler` are user-facing with `caller = msg.sender`. None
//! accept value, except `distribute`, which credits auction proceeds.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};

use outbe_primitives::dispatch::{
    dispatch_call, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[IIntexFactory::distributeCall::SELECTOR];

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
    // IntexFactory is a payable route, so the boundary credits value to this
    // address; every selector the module has not published refuses it here.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(
        data,
        IIntexFactory::IIntexFactoryCalls::abi_decode,
        |call| {
            use IIntexFactory::IIntexFactoryCalls::*;
            match call {
                settle(c) => mutate_void(c, caller, |sender, c| {
                    runtime::settle(
                        &storage,
                        c.seriesId,
                        c.intexHolder,
                        sender,
                        c.amount,
                        c.paymentToken,
                    )
                }),
                quoteCostAmount(c) => view(c, |c| {
                    runtime::quote_cost_amount(&storage, c.seriesId, c.paymentToken)
                }),
                // Off-chain the holder brute-forces `nonce` so the work hash
                // SHA256(hex(holder ++ promisAmount ++ seriesId ++ seq) ++ nonce_be8)
                // has POW_DIFFICULTY leading zero bytes; `seq` is the on-chain
                // per-(series, holder) counter.
                minePromis(c) => mutate(c, caller, |sender, c| {
                    let auth = outbe_promisfactory::api::ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_promis(&storage, c.seriesId, sender, c.amount, c.nonce, auth)
                }),
                setAuthorizedSettler(c) => mutate_void(c, caller, |sender, c| {
                    runtime::set_authorized_settler(&storage, sender, c.seriesId, c.settler)
                }),
                // The only payable selector: credits auction proceeds (msg.value)
                // from the source chain into the day's pot.
                distribute(c) => {
                    mutate_void_payable(c, PAYABLE_SELECTORS, caller, value, |sender, c, val| {
                        runtime::distribute(
                            &storage,
                            sender,
                            c.worldwideDay.into(),
                            c.srcChainId,
                            val,
                        )
                    })
                }
                // Permissionless: the merkle proof is the authorization, so the
                // sender is irrelevant to the outcome.
                payContributorBatch(c) => mutate_void(c, caller, |_sender, c| {
                    runtime::pay_contributor_batch(
                        &storage,
                        c.worldwideDay,
                        c.startIndex,
                        &c.leaves,
                        &c.proof,
                    )
                }),
                contributorPayoutRound(c) => view(c, |c| {
                    runtime::contributor_payout_round(&storage, c.worldwideDay)
                }),
                contributorPaidWord(c) => view(c, |c| {
                    outbe_intex::api::paid_leaves_word(&storage, c.worldwideDay, c.wordIndex)
                }),
            }
        },
    )
}
