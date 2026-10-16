//! IntexFactory runtime use-cases: issuance, settlement, Promis mining.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};

use outbe_intex::{SeriesId, SERIES_ID_LEN};
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

use outbe_intex::{IntexContract, IntexState, SeriesRecord};
use outbe_vaultrouter::settlement;

use crate::constants::{
    INTEX_NFT1155_ADDRESS, MAX_RECIPIENTS_PER_ISSUANCE, MAX_SERIES_PER_MESSAGE,
    ORIGIN_ROUTER_ADDRESS, SETTLED_TAG,
};
use crate::errors::IntexFactoryError;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::sol_ext::{IIntexNFT1155, IOriginRouter, IERC1155};
use IOriginRouter::IssuanceInstructionsParams;

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Record the series identity and call-price bin.
/// Return issuance legs for the caller to broadcast through `send_issuance`.
/// The broadcast creates IntexNFT1155 series on each target chain, including the origin.
pub fn issue_intex(
    storage: &StorageHandle<'_>,
    params: IssuanceParams,
) -> Result<Vec<IssuanceLeg>> {
    if params.issued_units == 0 {
        return Ok(Vec::new());
    }

    // u32 timestamp. It is bounded until 2106.
    let issued_at = u32::try_from(storage.timestamp()?.to::<u64>())
        .map_err(|_| IntexFactoryError::BlockTimestampExceedsU32)?;

    let mut intex = IntexContract::new(storage.clone());
    let cfg = outbe_intex::config::read(storage)?;

    let call_price_minor = marked_up(params.entry_price_minor, cfg.call_rate)?;

    let entry_price_minor_u64 = to_wire_price(params.entry_price_minor)?;
    let call_price_minor_u64 = to_wire_price(call_price_minor)?;

    let record = outbe_intex::CreateSeriesParams {
        series_id: params.series_id,
        worldwide_day: params.worldwide_day,
        issued_units: params.issued_units,
        promis_load_minor: params.promis_load_minor,
        entry_price_minor: params.entry_price_minor,
        call_price_minor,
        call_trigger: outbe_intex::IntexCallTrigger {
            call_window_seconds: cfg.call_window_seconds,
            call_threshold_seconds: cfg.call_threshold_seconds,
            call_notice_period_seconds: cfg.call_notice_period_seconds,
        },
        issued_at,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
    };
    outbe_intex::api::create_series(storage, record)?;
    intex.widen_call_terms(
        params.reference_currency,
        cfg.call_window_seconds,
        cfg.call_threshold_seconds,
    )?;

    // Not sent here: only the caller sees the whole day, and a chain's share of it
    // travels in as few messages as the caps allow.
    let legs: Vec<IssuanceLeg> = issuance_legs(&params)
        .into_iter()
        .map(|(chain_id, recipients, units)| IssuanceLeg {
            chain_id,
            payload: IOriginRouter::IssuanceInstructionsParams {
                seriesId: params.series_id.into(),
                worldwideDay: params.worldwide_day.into(),
                issuedAt: issued_at,
                issuedUnits: params.issued_units,
                promisLoadMinor: params.promis_load_minor,
                entryPriceMinor: entry_price_minor_u64,
                callNoticePeriod: cfg.call_notice_period_seconds,
                issuanceCurrency: params.issuance_currency,
                referenceCurrency: params.reference_currency,
                callWindow: cfg.call_window_seconds,
                callThreshold: cfg.call_threshold_seconds,
                callPriceMinor: call_price_minor_u64,
                recipients,
                units,
            },
        })
        .collect();

    // Enroll into the call-price bin index the daily Called scan walks.
    intex.insert_call_bin(
        params.series_id,
        params.reference_currency,
        call_price_minor,
    )?;

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

/// What one series adds to one chain: the series to create and that chain's winners
/// (empty on a chain with none, which still needs the series for bridging).
#[derive(Clone)]
pub struct IssuanceLeg {
    pub chain_id: u32,
    pub payload: IOriginRouter::IssuanceInstructionsParams,
}

/// Pack a day's legs into per-chain messages, up to `MAX_SERIES_PER_MESSAGE` series and
/// `MAX_RECIPIENTS_PER_ISSUANCE` recipients each. A series with more winners spans several,
/// which the receiver's create-if-absent makes safe.
pub fn pack_issuance_messages(
    legs: Vec<IssuanceLeg>,
) -> Vec<(u32, Vec<IssuanceInstructionsParams>)> {
    let mut per_chain: Vec<(u32, Vec<IssuanceInstructionsParams>)> = Vec::new();
    for leg in legs {
        for slice in split_recipients(leg.payload) {
            // Legs arrive series by series, so this chain's open message is not the last one
            // built. Matching on the tail alone would batch nothing.
            let open = per_chain
                .iter_mut()
                .rev()
                .find(|(chain, _)| *chain == leg.chain_id);
            match open {
                Some((_, message))
                    if message.len() < MAX_SERIES_PER_MESSAGE
                        && recipient_count(message) + slice.recipients.len()
                            <= MAX_RECIPIENTS_PER_ISSUANCE =>
                {
                    message.push(slice);
                }
                _ => per_chain.push((leg.chain_id, vec![slice])),
            }
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

fn recipient_count(message: &[IssuanceInstructionsParams]) -> usize {
    message.iter().map(|item| item.recipients.len()).sum()
}

/// One series' instructions cut into pieces a message can carry. Only the winners differ.
fn split_recipients(payload: IssuanceInstructionsParams) -> Vec<IssuanceInstructionsParams> {
    if payload.recipients.len() <= MAX_RECIPIENTS_PER_ISSUANCE {
        return vec![payload];
    }
    (0..payload.recipients.len())
        .step_by(MAX_RECIPIENTS_PER_ISSUANCE)
        .map(|start| {
            let end = (start + MAX_RECIPIENTS_PER_ISSUANCE).min(payload.recipients.len());
            IssuanceInstructionsParams {
                recipients: payload.recipients[start..end].to_vec(),
                units: payload.units[start..end].to_vec(),
                ..payload.clone()
            }
        })
        .collect()
}

/// One `(chain, recipients, units)` issuance leg per snapshot chain. Winners land on their
/// own chain, and every other chain gets an empty leg. So the series is created there too
/// (needed for user NFT bridging).
pub(crate) fn issuance_legs(params: &IssuanceParams) -> Vec<(u32, Vec<Address>, Vec<U256>)> {
    params
        .snapshot_chains
        .iter()
        .map(|&chain_id| {
            let mut recipients = Vec::new();
            let mut units = Vec::new();
            for (i, &c) in params.recipient_chains.iter().enumerate() {
                if c == chain_id {
                    recipients.push(params.recipients[i]);
                    units.push(params.units[i]);
                }
            }
            (chain_id, recipients, units)
        })
        .collect()
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
/// series expires, and its own exercise and Gem Factory history.
pub(crate) fn owner_balances(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
) -> Result<crate::precompile::IIntexFactory::OwnerBalances> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    let now = storage.timestamp()?.to::<u64>();
    let issued = if series.effective_state(now)? == IntexState::Expired {
        0
    } else {
        nft_units_of(storage, owner, issued_token_id(series_id))?
    };
    let settled = nft_units_of(storage, owner, settled_token_id(series_id))?;
    Ok(crate::precompile::IIntexFactory::OwnerBalances {
        issuedUnits: issued,
        settledUnits: settled,
        exercisedUnits: outbe_intex::api::owner_exercised_units(storage, series_id, owner)?,
        gemFactoryUnits: outbe_intex::api::owner_gem_factory_units(storage, series_id, owner)?,
        ownerUnits: issued
            .checked_add(settled)
            .ok_or(IntexFactoryError::OwnerUnitsExceedU32)?,
    })
}

/// Who pays a settlement, in which asset, and against which VWAP snapshot.
pub struct SettlementPayment {
    pub settler: Address,
    pub asset: Address,
    pub snapshot_id: U256,
}

/// Settle paying the cost from `settler` in `asset` by ERC20 transfer. The settled
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
        settler,
        asset,
        snapshot_id,
    } = payment;
    if owner.is_zero() || settler.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if units.is_zero() {
        return Err(IntexFactoryError::ZeroUnits.into());
    }

    let series = outbe_intex::api::read_series(storage, series_id)?;
    require_settleable(storage, &series)?;

    let balance = nft_balance_of(storage, owner, issued_token_id(series_id))?;
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
        &pricing::obligation(&series, units),
    )?;
    quote.require_snapshot(snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // The units move before payment so a token callback cannot settle them
        // twice. A failed payment rolls the move back.
        storage.call(
            INTEX_NFT1155_ADDRESS,
            U256::ZERO,
            IIntexNFT1155::settleIntexCall {
                seriesId: series_id.into(),
                owner,
                units,
            }
            .abi_encode()
            .into(),
        )?;

        let settled_units =
            u32::try_from(units).map_err(|_| IntexFactoryError::SettledUnitsExceedU32)?;
        outbe_intex::api::record_settled_units(storage, series_id, settled_units)?;

        settlement::collect(
            storage,
            INTEX_FACTORY_ADDRESS,
            settler,
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
    match series.lifecycle_state()? {
        IntexState::Called => {
            let now = storage.timestamp()?.to::<u64>();
            let deadline =
                u64::from(series.called_at) + u64::from(series.call_notice_period_seconds);
            if now > deadline {
                return Err(IntexFactoryError::DeadlineExpired.into());
            }
            Ok(())
        }
        IntexState::Issued => Ok(()),
        _ => Err(IntexFactoryError::NotSettleable(series.state).into()),
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
pub(crate) use mining::{issued_token_id, settled_token_id, validate_pow};
pub use mining::{mine_promis, MiningProof};
pub use pricing::quote_settlement;
#[cfg(test)]
pub(crate) use pricing::settlement_units;
pub use proceeds::distribute;
