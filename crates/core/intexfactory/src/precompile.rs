//! ABI dispatch for the IntexFactory precompile at `INTEX_FACTORY_ADDRESS`.
//!
//! Routing only: decode -> runtime -> encode. The settle calls / `minePromis` name the
//! owner they act for, so `caller = msg.sender` only binds the payment.
//! None accept value.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_vaultrouter::settlement::SettlementPayment;

use outbe_intex::SeriesId;
use outbe_primitives::dispatch::{
    dispatch_call, metadata, mutate, mutate_void, reject_value, view,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IIntexFactory.sol"
    );
}
pub use abi::IIntexFactory;

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        IIntexFactory::IIntexFactoryCalls::abi_decode,
        |call| {
            use IIntexFactory::IIntexFactoryCalls::*;
            match call {
                settleIntex(c) => mutate_void(&storage, c, caller, |sender, c| {
                    runtime::settle_intex(
                        &storage,
                        runtime::OwnerUnits {
                            series_id: series_of(&storage, c.intexId)?,
                            owner: c.owner,
                            units: c.units,
                        },
                        SettlementPayment {
                            payer: sender,
                            asset: c.asset,
                            snapshot_id: c.snapshotId,
                        },
                        c.amountCapMinor,
                    )
                }),
                quoteSettlement(c) => metadata::<IIntexFactory::quoteSettlementCall>(|| {
                    let (settlement_currency, amount_minor, snapshot_id) =
                        runtime::quote_settlement(
                            &storage,
                            series_of(&storage, c.intexId)?,
                            c.asset,
                            c.units,
                        )?;
                    Ok(IIntexFactory::quoteSettlementReturn {
                        settlementCurrency: settlement_currency,
                        amountMinor: amount_minor,
                        snapshotId: snapshot_id,
                    })
                }),
                // Off-chain the owner brute-forces `nonce` so the work hash
                // SHA256(tag ++ intexId_be32 ++ owner ++ seq_be8 ++ nonce_be8 ++ promisAmount_be32)
                // has the protocol's leading zero bytes. `seq` is the on-chain per-(series, owner)
                // counter.
                minePromis(c) => mutate(&storage, c, caller, |_sender, c| {
                    let auth = outbe_promisfactory::api::ModifyAuth {
                        mac: c.mac.0,
                        op_nonce: c.opNonce,
                    };
                    runtime::mine_promis(
                        &storage,
                        series_of(&storage, c.intexId)?,
                        c.owner,
                        c.units,
                        runtime::MiningProof {
                            nonce: c.nonce,
                            auth,
                        },
                    )
                }),
                powDifficulty(_) => metadata::<IIntexFactory::powDifficultyCall>(|| {
                    Ok(outbe_core_common::pow::POW_DIFFICULTY)
                }),
                ownerBalances(c) => view(c, |c| {
                    runtime::owner_balances(&storage, series_of(&storage, c.intexId)?, c.owner)
                }),
                getCallProgress(c) => view(c, |c| {
                    crate::call_progress::call_progress(&storage, c.intexId)
                }),
                targetChains(_) => metadata::<IIntexFactory::targetChainsCall>(|| {
                    let (chain_ids, modes) = crate::targets::target_chains(&storage)?
                        .into_iter()
                        .map(|(chain_id, mode)| (chain_id, mode as u8))
                        .unzip();
                    Ok(IIntexFactory::targetChainsReturn {
                        chainIds: chain_ids,
                        modes,
                    })
                }),
                targetsOf(c) => view(c, |c| {
                    crate::targets::day_targets(&storage, c.emissionDay.into())
                }),
                onCrosschainTokensReceived(c) => mutate(&storage, c, caller, |sender, c| {
                    runtime::receive_crosschain_proceeds(
                        &storage,
                        sender,
                        runtime::CrosschainProceeds {
                            source_domain: c.sourceDomain,
                            from: &c.from,
                            amount: c.amount,
                            extra_data: &c.extraData,
                        },
                    )?;
                    Ok(alloy_primitives::FixedBytes(
                        IIntexFactory::onCrosschainTokensReceivedCall::SELECTOR,
                    ))
                }),
                receiveLocalProceeds(c) => mutate_void(&storage, c, caller, |sender, c| {
                    runtime::receive_local_proceeds(
                        &storage,
                        sender,
                        c.emissionDay,
                        c.pageIndex,
                        c.amount,
                    )
                }),
                flush(c) => mutate_void(&storage, c, caller, |_, _| crate::outbox::flush(&storage)),
                outboxHead(c) => metadata::<IIntexFactory::outboxHeadCall>(|| {
                    crate::outbox::outbox_head(&storage, c.chainId)
                }),
            }
        },
    )
}

/// The series an intex id names.
fn series_of(storage: &StorageHandle<'_>, intex_id: U256) -> Result<SeriesId> {
    outbe_intex::api::find_series_by_id(storage, intex_id)?
        .map(|record| record.series_id)
        .ok_or_else(|| outbe_intex::IntexError::SeriesNotFound.into())
}
