//! IntexFactory runtime use-cases: issuance, settlement, Promis mining.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};

use outbe_intex::{SeriesId, SERIES_ID_LEN};
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

use outbe_intex::{IntexContract, IntexState, SeriesRecord};
use outbe_vaultrouter::settlement::{self, SettlementPayment};

use crate::constants::{INTEX_NFT1155_ADDRESS, MAX_SERIES_PER_MESSAGE, ORIGIN_ROUTER_ADDRESS};
use crate::errors::IntexFactoryError;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::sol_ext::{IIntexNFT1155, IOriginRouter, IERC1155};
use IOriginRouter::IssuanceInstructionsParams;

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Record the series identity and call-price bin with the conditions in `params`.
/// The auction fixed them at its start, so this reads no profile value.
/// Return issuance legs for the caller to broadcast through `send_issuance`.
/// The broadcast creates IntexNFT1155 series on each target chain, including the origin.
pub fn issue_intex(
    storage: &StorageHandle<'_>,
    params: IssuanceParams,
) -> Result<Vec<IssuanceLeg>> {
    if params.issued_units == 0 {
        return Ok(Vec::new());
    }

    let issued_at = storage.now_secs()?;

    let mut intex = IntexContract::new(storage.clone());
    let call_price_minor = params.call_price_minor;
    let trigger = params.call_trigger;

    let entry_price_minor_u64 = to_wire_price(params.entry_price_minor)?;
    let call_price_minor_u64 = to_wire_price(call_price_minor)?;

    let record = outbe_intex::CreateSeriesParams {
        series_id: params.series_id,
        worldwide_day: params.worldwide_day,
        issued_units: params.issued_units,
        promis_load_minor: params.promis_load_minor,
        entry_price_minor: params.entry_price_minor,
        call_price_minor,
        call_trigger: trigger,
        issued_at,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
    };
    outbe_intex::api::create_series(storage, record)?;
    intex.widen_call_terms(
        params.reference_currency,
        trigger.call_window_seconds,
        trigger.call_threshold_seconds,
    )?;

    // Not sent here: only the caller sees the whole day, and a chain's share of it
    // travels in as few messages as the cap allows.
    let legs: Vec<IssuanceLeg> = params
        .snapshot_chains
        .iter()
        .map(|&chain_id| IssuanceLeg {
            chain_id,
            payload: IOriginRouter::IssuanceInstructionsParams {
                seriesId: params.series_id.into(),
                worldwideDay: params.worldwide_day.into(),
                issuedAt: issued_at,
                issuedUnits: params.issued_units,
                promisLoadMinor: params.promis_load_minor,
                entryPriceMinor: entry_price_minor_u64,
                callNoticePeriod: trigger.call_notice_period_seconds,
                issuanceCurrency: params.issuance_currency,
                referenceCurrency: params.reference_currency,
                callWindow: trigger.call_window_seconds,
                callThreshold: trigger.call_threshold_seconds,
                callPriceMinor: call_price_minor_u64,
                transitMargin: trigger.transit_margin_seconds,
            },
        })
        .collect();

    emit_event(
        storage,
        crate::precompile::IIntexFactory::IntexSeriesIssued {
            seriesId: params.series_id.into(),
            issuedUnits: params.issued_units,
            entryPriceMinor: params.entry_price_minor,
            callPriceMinor: call_price_minor,
        },
    )?;

    Ok(legs)
}

/// What one series adds to one chain: the series to create. Every snapshot chain gets it, so
/// Intex can be bridged to any of them.
#[derive(Clone)]
pub struct IssuanceLeg {
    pub chain_id: u32,
    pub payload: IOriginRouter::IssuanceInstructionsParams,
}

/// Pack a day's legs into per-chain messages of up to `MAX_SERIES_PER_MESSAGE` series each.
pub fn pack_issuance_messages(
    legs: Vec<IssuanceLeg>,
) -> Vec<(u32, Vec<IssuanceInstructionsParams>)> {
    let mut per_chain: Vec<(u32, Vec<IssuanceInstructionsParams>)> = Vec::new();
    for leg in legs {
        // Legs arrive series by series, so this chain's open message is not the last one
        // built. Matching on the tail alone would batch nothing.
        let open = per_chain
            .iter_mut()
            .rev()
            .find(|(chain, _)| *chain == leg.chain_id);
        match open {
            Some((_, message)) if message.len() < MAX_SERIES_PER_MESSAGE => {
                message.push(leg.payload);
            }
            _ => per_chain.push((leg.chain_id, vec![leg.payload])),
        }
    }
    per_chain
}

/// Send a day's packed issuance messages, each stamped with its position in the chain's run.
/// Relay-float-funded: value 0. The router quotes and pays the bridge fee from its own float.
pub fn send_issuance(storage: &StorageHandle<'_>, legs: Vec<IssuanceLeg>) -> Result<()> {
    for ((chain_id, worldwide_day), messages) in
        chunk_issuance_messages(pack_issuance_messages(legs))
    {
        let total_chunks = u16::try_from(messages.len())
            .map_err(|_| IntexFactoryError::IssuanceChunkCountExceedsU16)?;
        for (chunk_index, series) in messages.into_iter().enumerate() {
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendIssuanceInstructionsCall {
                    dstChainId: chain_id,
                    worldwideDay: worldwide_day,
                    chunkIndex: chunk_index as u16,
                    totalChunks: total_chunks,
                    series,
                }
                .abi_encode()
                .into(),
            )?;
        }
    }
    Ok(())
}

/// One (chain, worldwide day) run of issuance messages, in send order: what the chunk header
/// numbers.
pub(crate) type IssuanceRun = ((u32, u32), Vec<Vec<IssuanceInstructionsParams>>);

/// Group packed messages into the runs the chunk header numbers: one per (chain, day), the pair
/// the receiver counts chunks against. Callers pass one day's legs (clearing does). This is also
/// what keeps `pack_issuance_messages` from batching two days into one message.
pub(crate) fn chunk_issuance_messages(
    packed: Vec<(u32, Vec<IssuanceInstructionsParams>)>,
) -> Vec<IssuanceRun> {
    let mut runs: Vec<IssuanceRun> = Vec::new();
    for (chain_id, message) in packed {
        let key = (chain_id, message[0].worldwideDay);
        match runs.iter_mut().find(|(run_key, _)| *run_key == key) {
            Some((_, messages)) => messages.push(message),
            None => runs.push((key, vec![message])),
        }
    }
    runs
}

/// `entry x (100 + rate) / 100`, the markup a series seals its call price at.
pub fn marked_up(entry_price: U256, rate: u16) -> Result<U256> {
    outbe_primitives::markup::marked_up(entry_price, rate)
        .ok_or_else(|| IntexFactoryError::MarkedUpPriceOverflow.into())
}

/// Narrows a six-decimal COEN/ISO price to the wire's existing `u64` shape.
pub fn to_wire_price(price_minor: U256) -> Result<u64> {
    u64::try_from(price_minor).map_err(|_| IntexFactoryError::PriceExceedsWireType.into())
}

/// One owner's units of `series_id`: its current balances, Issued ones only until the
/// series is forfeited, and its own mining and Gem Factory history.
pub(crate) fn owner_balances(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
) -> Result<crate::precompile::IIntexFactory::OwnerBalances> {
    let series = outbe_intex::api::get_series(storage, series_id)?;
    let issued = if outbe_intex::api::state(storage, &series)? == IntexState::Forfeited {
        0
    } else {
        nft_units_of(storage, owner, series_id.token_id())?
    };
    let settled = outbe_intex::api::owner_settled_units(storage, series_id, owner)?;
    Ok(crate::precompile::IIntexFactory::OwnerBalances {
        issuedUnits: issued,
        settledUnits: settled,
        minedUnits: outbe_intex::api::owner_mined_units(storage, series_id, owner)?,
        gemFactoryUnits: outbe_intex::api::owner_gem_factory_units(storage, series_id, owner)?,
        ownerUnits: issued
            .checked_add(settled)
            .ok_or(IntexFactoryError::OwnerUnitsExceedU32)?,
    })
}

/// Settle paying the cost from `payer` in `asset` by ERC20 transfer. The settled
/// units stay with `owner`. An issuance-currency payment must name the VWAP
/// snapshot required at this block.
pub fn settle_intex(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: U256,
    payment: SettlementPayment,
) -> Result<()> {
    let SettlementPayment {
        payer,
        asset,
        snapshot_id,
    } = payment;
    if owner.is_zero() || payer.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if units.is_zero() {
        return Err(IntexFactoryError::ZeroUnits.into());
    }

    let series = outbe_intex::api::get_series(storage, series_id)?;
    require_settleable(storage, &series)?;

    let balance = nft_balance_of(storage, owner, series_id.token_id())?;
    if balance.is_zero() {
        return Err(IntexFactoryError::ZeroBalance.into());
    }
    if units > balance {
        return Err(IntexFactoryError::UnitsExceedBalance.into());
    }

    let quote = settlement::quote(
        storage,
        asset,
        pricing::currencies(&series),
        &pricing::settlement_cost(&series, units),
    )?;
    quote.require_snapshot(snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // The units move before payment so a token callback cannot settle them
        // twice. A failed payment rolls the move back.
        storage.call(
            INTEX_NFT1155_ADDRESS,
            U256::ZERO,
            IIntexNFT1155::burnIntexCall {
                owner,
                seriesId: series_id.into(),
                units,
            }
            .abi_encode()
            .into(),
        )?;

        let settled_units =
            u32::try_from(units).map_err(|_| IntexFactoryError::SettledUnitsExceedU32)?;
        outbe_intex::api::record_settled_units(storage, series_id, owner, settled_units)?;

        settlement::collect(
            storage,
            INTEX_FACTORY_ADDRESS,
            payer,
            asset,
            quote.payment_minor,
        )?;

        emit_event(
            storage,
            crate::precompile::IIntexFactory::IntexSettled {
                seriesId: series_id.into(),
                owner,
                units,
            },
        )
    })
}

fn require_settleable(storage: &StorageHandle<'_>, series: &SeriesRecord) -> Result<()> {
    match outbe_intex::api::state(storage, series)? {
        IntexState::Issued | IntexState::Called => Ok(()),
        IntexState::Forfeited => Err(IntexFactoryError::SettlementDeadlinePassed.into()),
    }
}

// --- storage.call helpers (localnet-exercised) ---

fn nft_units_of(storage: &StorageHandle<'_>, account: Address, id: U256) -> Result<u32> {
    u32::try_from(nft_balance_of(storage, account, id)?)
        .map_err(|_| IntexFactoryError::NftBalanceExceedsU32.into())
}

fn nft_balance_of(storage: &StorageHandle<'_>, account: Address, id: U256) -> Result<U256> {
    let ret = storage.staticcall(
        INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall { account, id }.abi_encode().into(),
    )?;
    IERC1155::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| IntexFactoryError::NftBalanceOfUndecodable.into())
}

mod mining;
mod pricing;
mod proceeds;

#[cfg(test)]
pub(crate) use mining::compute_pow_hash;
pub(crate) use mining::validate_pow;
pub use mining::{mine_promis, MiningProof};
pub use pricing::quote_settlement;
#[cfg(test)]
pub(crate) use pricing::settlement_units;
pub use proceeds::distribute;
