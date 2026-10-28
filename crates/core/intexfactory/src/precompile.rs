//! ABI dispatch for the IntexFactory precompile at `INTEX_FACTORY_ADDRESS`.
//!
//! Routing only: decode -> runtime -> encode. The settle calls / `minePromis` name the
//! owner they act for, so `caller = msg.sender` only binds the payment.
//! None accept value, except `distribute`, which burns auction proceeds.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_vaultrouter::settlement::SettlementPayment;

use outbe_intex::SeriesId;
use outbe_primitives::dispatch::{
    dispatch_call, metadata, mutate, mutate_void, mutate_void_payable, reject_value_unless_payable,
    view, PayableCallContext,
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
    // address. Every selector the module has not published refuses it here.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    #[cfg(feature = "e2e-test")]
    if let Some(result) = crate::test_arming_e2e::dispatch(&storage, data) {
        return result;
    }
    dispatch_call(
        data,
        IIntexFactory::IIntexFactoryCalls::abi_decode,
        |call| {
            use IIntexFactory::IIntexFactoryCalls::*;
            match call {
                maintain(c) => mutate_void(&storage, c, caller, |_sender, c| {
                    runtime::maintain(
                        &storage,
                        outbe_oracle::call_sweep::Selection {
                            process_call: c.process_call,
                            process_forfeit: c.process_forfeit,
                            process_cleanup: c.process_cleanup,
                        },
                    )
                }),
                hasMaintenanceWork(c) => view(c, |_| {
                    let (call_pending, forfeit_pending, cleanup_pending) =
                        runtime::has_maintenance_work(&storage)?;
                    Ok(IIntexFactory::hasMaintenanceWorkReturn {
                        call_pending,
                        forfeit_pending,
                        cleanup_pending,
                    })
                }),
                settleIntex(c) => mutate_void(&storage, c, caller, |sender, c| {
                    runtime::settle_intex(
                        &storage,
                        SeriesId::from(c.seriesId),
                        c.owner,
                        c.units,
                        SettlementPayment {
                            payer: sender,
                            asset: c.asset,
                            snapshot_id: c.snapshotId,
                        },
                    )
                }),
                quoteSettlement(c) => metadata::<IIntexFactory::quoteSettlementCall>(|| {
                    let (settlement_currency, payment_minor, snapshot_id) =
                        runtime::quote_settlement(
                            &storage,
                            SeriesId::from(c.seriesId),
                            c.asset,
                            c.units,
                        )?;
                    Ok(IIntexFactory::quoteSettlementReturn {
                        settlementCurrency: settlement_currency,
                        paymentMinor: payment_minor,
                        snapshotId: snapshot_id,
                    })
                }),
                // Off-chain the owner brute-forces `nonce` so the work hash
                // SHA256(owner ++ promisAmount_be32 ++ seriesId ++ seq_be4 ++ nonce_be8)
                // has the protocol's leading zero bytes. `seq` is the on-chain
                // per-(series, owner) counter.
                minePromis(c) => mutate(&storage, c, caller, |_sender, c| {
                    let auth = outbe_promisfactory::api::ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_promis(
                        &storage,
                        SeriesId::from(c.seriesId),
                        c.owner,
                        c.units,
                        runtime::MiningProof {
                            nonce: c.nonce,
                            auth,
                        },
                    )
                }),
                powDifficulty(_) => metadata::<IIntexFactory::powDifficultyCall>(|| {
                    Ok(outbe_common::pow::POW_DIFFICULTY)
                }),
                // The only payable selector: burns auction proceeds (msg.value)
                // into the Promis limit pool.
                distribute(c) => mutate_void_payable(
                    &storage,
                    c,
                    PayableCallContext {
                        selectors: PAYABLE_SELECTORS,
                        sender: caller,
                        value,
                    },
                    |sender, c, val| {
                        runtime::distribute(
                            &storage,
                            sender,
                            c.worldwideDay.into(),
                            c.srcChainId,
                            val,
                        )
                    },
                ),
                ownerBalances(c) => view(c, |c| {
                    runtime::owner_balances(&storage, c.seriesId.into(), c.owner)
                }),
            }
        },
    )
}
