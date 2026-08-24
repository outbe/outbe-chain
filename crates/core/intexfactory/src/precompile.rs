//! ABI dispatch for the IntexFactory precompile at `INTEX_FACTORY_ADDRESS`.
//!
//! Routing only: decode -> runtime -> encode. `settle` / `minePromis` /
//! `setAuthorizedSettler` are user-facing with `caller = msg.sender`. None
//! accept value, except `distribute`, which credits auction proceeds.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};

use outbe_intex::SeriesId;
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

// Arming the proceeds fan-in is production work of the issuance leg, which a
// payout e2e never reaches: it runs no auction, so it issues nothing. This
// stages that one precondition and exists only in a throwaway build.
#[cfg(feature = "e2e-test")]
sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexFactoryTestArming {
        function armProceedsForTest(uint32 worldwideDay, uint32[] chains, uint64 deadline) external;
        function issueForTest(
            bytes14 seriesId,
            uint32 worldwideDay,
            uint32 issuedIntexCount,
            uint128 promisLoadMinor,
            uint256 entryPriceMinor,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            address[] recipients,
            uint256[] quantities,
            uint32[] recipientChains,
            uint32[] snapshotChains
        ) external;
    }
}

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    // IntexFactory is a payable route, so the boundary credits value to this
    // address; every selector the module has not published refuses it here.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    #[cfg(feature = "e2e-test")]
    if let Ok(call) = IIntexFactoryTestArming::issueForTestCall::abi_decode(data) {
        // The same two calls the clearing engine makes, so the series is indexed for
        // the qualify sweep and its mints travel as real issuance instructions.
        let legs = crate::api::issue(
            &storage,
            crate::schema::IssuanceParams {
                series_id: SeriesId::from(call.seriesId),
                worldwide_day: call.worldwideDay.into(),
                issued_intex_count: call.issuedIntexCount,
                promis_load_minor: call.promisLoadMinor,
                entry_price_minor: call.entryPriceMinor,
                issuance_currency: call.issuanceCurrency,
                reference_currency: call.referenceCurrency,
                recipients: call.recipients,
                quantities: call.quantities,
                recipient_chains: call.recipientChains,
                snapshot_chains: call.snapshotChains,
            },
        )?;
        crate::api::send_issuance(&storage, legs)?;
        return Ok(Bytes::new());
    }
    #[cfg(feature = "e2e-test")]
    if let Ok(call) = IIntexFactoryTestArming::armProceedsForTestCall::abi_decode(data) {
        outbe_intex::api::arm_proceeds(
            &storage,
            call.worldwideDay.into(),
            &call.chains,
            call.deadline,
        )?;
        return Ok(Bytes::new());
    }
    dispatch_call(
        data,
        IIntexFactory::IIntexFactoryCalls::abi_decode,
        |call| {
            use IIntexFactory::IIntexFactoryCalls::*;
            match call {
                settle(c) => mutate_void(c, caller, |sender, c| {
                    runtime::settle(
                        &storage,
                        SeriesId::from(c.seriesId),
                        c.intexHolder,
                        sender,
                        c.amount,
                        c.paymentToken,
                    )
                }),
                quoteCostAmount(c) => view(c, |c| {
                    runtime::quote_cost_amount(&storage, SeriesId::from(c.seriesId), c.paymentToken)
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
                    runtime::mine_promis(
                        &storage,
                        SeriesId::from(c.seriesId),
                        sender,
                        c.amount,
                        c.nonce,
                        auth,
                    )
                }),
                setAuthorizedSettler(c) => mutate_void(c, caller, |sender, c| {
                    runtime::set_authorized_settler(
                        &storage,
                        sender,
                        SeriesId::from(c.seriesId),
                        c.settler,
                    )
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
