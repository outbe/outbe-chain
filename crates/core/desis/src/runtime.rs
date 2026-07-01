//! Desis runtime: auction lifecycle and clearing algorithm.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::date_key_to_utc_timestamp;
use outbe_promislimit::PromisLimitContract;

use outbe_intexfactory::constants::{CALL_PRICE_DEN, FLOOR_PRICE_DEN};

use crate::constants::{
    BID_QUANTITY_FLOOR_BPS, ISSUANCE_WINDOW_SECONDS, ORIGIN_MESSENGER_ADDRESS,
    QUALIFIER_ISSUANCE_ISO, QUALIFIER_REFERENCE_ISO, RATE_SCALE, REVEAL_WINDOW_SECONDS,
};
use crate::errors::DesisError;
use crate::precompile::IDesis;
use crate::schema::{AuctionConfig, AuctionStage, BidData, ClearingResult, DesisContract};
use crate::sol_ext::IOriginMessenger;

// ---------------------------------------------------------------------------
// Auction lifecycle
// ---------------------------------------------------------------------------

/// Create a new auction for `series_id` and transition to `Started`.
///
/// Derives minBidQty from the prior clearing (4% of issued count) and
/// validates the config.
pub fn start_auction(
    storage: StorageHandle<'_>,
    series_id: u32,
    mut config: AuctionConfig,
) -> Result<()> {
    if series_id == 0 {
        return Err(DesisError::InvalidSeriesId(0).into());
    }
    if config.promis_load_minor == 0 || config.escrow_basis_minor() == 0 {
        return Err(DesisError::InvalidSeriesId(series_id).into());
    }

    let mut contract = storage.contract::<DesisContract>();

    // Duplicate guard.
    let existing = contract.read_stage(series_id)?;
    if existing != AuctionStage::None {
        return Err(DesisError::InvalidStageTransition.into());
    }

    // Derive minBidQty as 4% of the prior clearing's issued count.
    let min_bid_qty: u16 = {
        let last_series = contract.read_last_cleared_series()?;
        if last_series != 0 {
            let prev_issued = contract.read_last_clearing_issued_count()?;
            let derived =
                (prev_issued as u64).saturating_mul(BID_QUANTITY_FLOOR_BPS as u64) / 10_000;
            derived.min(u16::MAX as u64) as u16
        } else {
            0
        }
    };

    // Genesis IntexFactory profile: floor/call derive from entry; window/threshold/
    // period are the call-trigger params relayed to the target chain. Sourced here
    // (storage in reach) and folded into the config before it is persisted, so the
    // demand-side config carries the same values the wire message ships.
    let iparams = outbe_intexfactory::read_params(&storage)?;
    config.min_intex_bid_quantity = min_bid_qty;
    config.call_trigger = crate::schema::IntexCallTrigger {
        window_days: iparams.call_window_days,
        threshold_days: iparams.call_threshold_days,
        intex_call_period: iparams.intex_call_period_secs,
    };

    contract.write_auction_config(series_id, &config)?;
    contract.write_stage(series_id, AuctionStage::Started)?;
    contract.emit(IDesis::AuctionCreated {
        seriesId: series_id,
    })?;

    // Send AUCTION_STAGE_START to BNB.
    // revealEnd = noon of the series day; commitEnd/issuanceEnd are protocol offsets.
    let noon = u32::try_from(date_key_to_utc_timestamp(series_id) + 12 * 3600)
        .map_err(|_| PrecompileError::Revert("series day noon exceeds u32".into()))?;
    let commit_end = noon.saturating_sub(REVEAL_WINDOW_SECONDS);
    let issuance_end = noon.saturating_add(ISSUANCE_WINDOW_SECONDS);
    let floor_price = config
        .entry_price_minor
        .checked_mul(U256::from(iparams.floor_price_num))
        .map(|v| v / U256::from(FLOOR_PRICE_DEN))
        .ok_or_else(|| PrecompileError::Revert("entry floor overflow".into()))?;
    let call_price = config
        .entry_price_minor
        .checked_mul(U256::from(iparams.call_price_num))
        .map(|v| v / U256::from(CALL_PRICE_DEN))
        .ok_or_else(|| PrecompileError::Revert("entry call overflow".into()))?;
    let entry_price_u64 = u64::try_from(config.entry_price_minor)
        .map_err(|_| PrecompileError::Revert("entry price exceeds u64".into()))?;
    let floor_price_u64 = u64::try_from(floor_price)
        .map_err(|_| PrecompileError::Revert("floor price exceeds u64".into()))?;
    let call_price_u64 = u64::try_from(call_price)
        .map_err(|_| PrecompileError::Revert("call price exceeds u64".into()))?;
    let stage_params = IOriginMessenger::AuctionStageStartParams {
        seriesId: series_id,
        commitEnd: commit_end,
        revealEnd: noon,
        issuanceEnd: issuance_end,
        issuanceCurrency: config.issuance_currency,
        referenceCurrency: config.reference_currency,
        promisLoadMinor: config.promis_load_minor,
        minIntexBidRate: config.min_intex_bid_rate,
        entryPrice: entry_price_u64,
        floorPriceMinor: floor_price_u64,
        callPriceMinor: call_price_u64,
        intexCallPeriod: iparams.intex_call_period_secs,
        callWindowDays: iparams.call_window_days,
        callThresholdDays: iparams.call_threshold_days,
        minIntexBidQuantity: min_bid_qty,
    };
    // Relay-float-funded: value 0, so the messenger self-quotes and pays the bridge fee from its float.
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionStageStartCall {
            params: stage_params,
        }
        .abi_encode()
        .into(),
    )?;

    Ok(())
}

/// Signal `Started` → `Revealing` (green day) or `Started` → `Cancelled` (red day).
pub fn reveal_auction(
    storage: StorageHandle<'_>,
    series_id: u32,
    is_green_day: bool,
) -> Result<()> {
    require_nonzero_series_id(series_id)?;
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, series_id, AuctionStage::Started)?;
    let next = if is_green_day {
        AuctionStage::Revealing
    } else {
        AuctionStage::Cancelled
    };
    contract.write_stage(series_id, next)?;
    if !is_green_day {
        contract.emit(IDesis::AuctionCancelledRedDay {
            seriesId: series_id,
        })?;
    }

    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionStageRevealCall {
            seriesId: series_id,
            isGreenDay: is_green_day,
        }
        .abi_encode()
        .into(),
    )?;

    Ok(())
}

/// Signal `Revealing` → clearing: store supply; returns the Promis rounding
/// remainder (supply_promis % promis_load_minor) to be returned to PromisLimit.
pub fn begin_clearing(
    storage: StorageHandle<'_>,
    series_id: u32,
    supply_promis: u128,
) -> Result<u128> {
    require_nonzero_series_id(series_id)?;
    let contract = storage.contract::<DesisContract>();
    require_stage(&contract, series_id, AuctionStage::Revealing)?;

    let config = contract.read_auction_config(series_id)?;
    if config.promis_load_minor == 0 {
        return Err(DesisError::InvalidSeriesId(series_id).into());
    }

    let supply_intex = supply_promis / config.promis_load_minor;
    let supply_intex32 =
        u32::try_from(supply_intex).map_err(|_| DesisError::InvalidSeriesId(series_id))?;
    let rounding_remainder = supply_promis % config.promis_load_minor;

    contract.clearing_initiated.write(&series_id, 1u8)?;
    contract
        .pending_supply_intex
        .write(&series_id, supply_intex32)?;

    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionStageClearingCall {
            seriesId: series_id,
        }
        .abi_encode()
        .into(),
    )?;

    Ok(rounding_remainder)
}

// ---------------------------------------------------------------------------
// Bid ingestion
// ---------------------------------------------------------------------------

/// Accept a relayed bid batch. Bids of one `generation` accumulate while the stage is `Revealing`; a
/// higher `generation` supersedes all prior bids. Batches may arrive in any order over the unordered
/// bridge, so completeness is tracked by a per-generation bitmap of `batch_index`: once all
/// `total_batches` distinct indices have arrived the stage advances to `BidsReceived` (a zero-bid flush
/// is one empty batch that completes immediately and then clears as a no-sale). A redelivered batch (its
/// bit already set) is an idempotent no-op, so the transport may safely re-deliver.
pub fn process_bids_batch(
    storage: StorageHandle<'_>,
    caller: Address,
    series_id: u32,
    src_chain_id: u32,
    generation: u32,
    batch_index: u16,
    total_batches: u16,
    bids: Vec<BidData>,
) -> Result<()> {
    require_origin_messenger(caller)?;
    require_nonzero_series_id(series_id)?;
    // The arrival bitmap is a U256, so at most 256 batches (batch_index 0..=255) are trackable.
    if total_batches == 0 || total_batches > 256 || batch_index >= total_batches {
        return Err(
            PrecompileError::Revert("processBidsBatch: invalid batch index/total".into()).into(),
        );
    }
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, series_id, AuctionStage::Revealing)?;

    let last_gen = contract.read_last_generation(series_id)?;
    if generation < last_gen {
        return Err(DesisError::StaleBidsGeneration {
            incoming: generation,
            last: last_gen,
        }
        .into());
    }

    if generation > last_gen {
        // New generation supersedes: drop prior bids and reset the completeness tracker.
        contract.bid_count.write(&series_id, 0)?;
        contract.write_bid_batch_meta(series_id, src_chain_id, generation)?;
        contract
            .bids_total_batches
            .write(&series_id, u32::from(total_batches))?;
        contract.bids_arrived_mask.write(&series_id, U256::ZERO)?;
    }

    let bit = U256::from(1u8) << (batch_index as usize);
    let mask = contract.bids_arrived_mask.read(&series_id)?;
    if !(mask & bit).is_zero() {
        // This batch of the current generation was already applied; redelivery is idempotent.
        return Ok(());
    }

    for bid in &bids {
        contract.append_bid(series_id, bid)?;
    }
    let mask = mask | bit;
    contract.bids_arrived_mask.write(&series_id, mask)?;

    // Advance once every batch of the generation has arrived. A zero-bid flush completes here and
    // clears as a no-sale (0 issued, full supply returned to PromisLimit), still reporting the result
    // to the target chain; Cancelled is reserved for red days (see `reveal_auction`).
    let expected = contract.bids_total_batches.read(&series_id)?;
    if mask.count_ones() as u32 == expected {
        let count = contract.read_bid_count(series_id)?;
        contract.write_stage(series_id, AuctionStage::BidsReceived)?;
        contract.emit(IDesis::BidsReceived {
            seriesId: series_id,
            srcChainId: src_chain_id,
            bidsCount: U256::from(count),
        })?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Clearing
// ---------------------------------------------------------------------------

/// Run the clearing algorithm for `series_id`, transition to `Cleared`, hand
/// issuance to IntexFactory, and return unused supply to PromisLimit.
///
/// Returns the `ClearingResult` so the caller (precompile) can dispatch
/// AUCTION_RESULT and REFUND_INSTRUCTIONS messages.
pub fn clear_auction(
    storage: StorageHandle<'_>,
    caller: Address,
    series_id: u32,
) -> Result<ClearingResult> {
    require_origin_messenger(caller)?;
    require_nonzero_series_id(series_id)?;
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, series_id, AuctionStage::BidsReceived)?;

    let supply = contract.pending_supply_intex.read(&series_id)?;
    if contract.clearing_initiated.read(&series_id)? == 0 {
        return Err(DesisError::PendingClearingDataMissing(series_id).into());
    }

    let config = contract.read_auction_config(series_id)?;
    let min_bid_qty = contract.config_min_bid_quantity.read(&series_id)? as u16;
    // A zero-bid batch is valid here: `calculate_clearing` yields 0 issued, the full supply returns
    // to PromisLimit, and a no-sale AuctionResult(0,0,0) is reported to the target chain.
    let bids = contract.read_all_bids(series_id)?;

    let total_demand: u64 = bids.iter().map(|b| u64::from(b.intex_quantity)).sum();
    let mut sorted = bids;
    sort_bids(&mut sorted);

    let result = calculate_clearing(&sorted, &config, supply, min_bid_qty);

    // Persist clearing outcome and transition.
    contract.write_stage(series_id, AuctionStage::Cleared)?;
    contract.write_last_cleared_series(series_id)?;
    contract.write_last_clearing_issued_count(result.issued_intex_count)?;

    // Clear bid working-set and pending inputs (CEI: state writes before external calls).
    contract.bid_count.write(&series_id, 0)?;
    contract.pending_supply_intex.write(&series_id, 0)?;
    contract.clearing_initiated.write(&series_id, 0u8)?;

    if result.issued_intex_count == 0 {
        contract.emit(IDesis::AuctionClearedEmpty {
            seriesId: series_id,
            totalDemand: total_demand,
        })?;
    } else {
        contract.emit(IDesis::AuctionCleared {
            seriesId: series_id,
            issuedIntexCount: result.issued_intex_count,
            clearingRate: result.clearing_rate,
            totalDemand: total_demand,
        })?;
    }

    // Return unused Promis to PromisLimit.
    let remaining_supply = supply - result.issued_intex_count;
    if remaining_supply > 0 {
        let unused_promis =
            U256::from(remaining_supply as u128) * U256::from(config.promis_load_minor);
        contract.emit(IDesis::UnusedSupplyReported {
            seriesId: series_id,
            unusedPromis: unused_promis,
        })?;
        PromisLimitContract::new(storage.clone()).add_to_total_unallocated(unused_promis)?;
    }

    // Hand issuance to IntexFactory; an empty clearing issues nothing and
    // creates no series.
    if result.issued_intex_count > 0 {
        let params = outbe_intexfactory::schema::IssuanceParams {
            series_id,
            issued_intex_count: result.issued_intex_count,
            promis_load_minor: config.promis_load_minor,
            entry_price_minor: config.entry_price_minor,
            issuance_currency: QUALIFIER_ISSUANCE_ISO,
            reference_currency: QUALIFIER_REFERENCE_ISO,
            recipients: result.winners.clone(),
            quantities: result.winner_quantities.clone(),
        };
        outbe_intexfactory::api::issue(&storage, params)?;
    }

    // Send AUCTION_RESULT to BNB.
    let won_bids_count = u32::try_from(result.winners.len())
        .map_err(|_| PrecompileError::Revert("winner count exceeds u32".into()))?;
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionResultCall {
            seriesId: series_id,
            issuedIntexCount: result.issued_intex_count,
            auctionClearingRate: u64::from(result.clearing_rate),
            wonBidsCount: won_bids_count,
        }
        .abi_encode()
        .into(),
    )?;

    // Send REFUND_INSTRUCTIONS to BNB (all bidders including losers).
    if !result.all_bidders.is_empty() {
        storage.call(
            ORIGIN_MESSENGER_ADDRESS,
            U256::ZERO,
            IOriginMessenger::sendRefundInstructionsCall {
                seriesId: series_id,
                bidders: result.all_bidders.clone(),
                refundedAmounts: result.refunded_amounts.clone(),
                paidAmounts: result.paid_amounts.clone(),
            }
            .abi_encode()
            .into(),
        )?;
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Clearing algorithm (pure)
// ---------------------------------------------------------------------------

/// Sort bids: descending rate, ascending timestamp on tie.
fn sort_bids(bids: &mut [BidData]) {
    bids.sort_by(|a, b| {
        b.intex_bid_rate
            .cmp(&a.intex_bid_rate)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });
}

/// Escrow amount for `qty` Intexes at `rate` (1e6 fixed-point) against the
/// per-Intex escrow basis: `qty * basis * rate / RATE_SCALE`, saturating to u128
/// (wCOEN amounts at 18 decimals exceed u64).
fn rate_lock(qty: u64, basis: u128, rate: u32) -> u128 {
    let amount = U256::from(qty)
        .saturating_mul(U256::from(basis))
        .saturating_mul(U256::from(rate))
        / U256::from(RATE_SCALE);
    u128::try_from(amount).unwrap_or(u128::MAX)
}

/// Uniform-rate clearing: allocate sorted bids until `supply` runs out; the
/// clearing rate is the last allocated bid's. lock/pay = qty * escrow_basis * rate / RATE_SCALE.
fn calculate_clearing(
    bids: &[BidData],
    config: &AuctionConfig,
    supply: u32,
    min_qty: u16,
) -> ClearingResult {
    let len = bids.len();
    let mut winners: Vec<Address> = Vec::with_capacity(len);
    let mut winner_quantities: Vec<alloy_primitives::U256> = Vec::with_capacity(len);
    let mut won_by_index: Vec<u32> = vec![0u32; len];

    let escrow_basis = config.escrow_basis_minor();
    let mut total_allocated: u32 = 0;
    let mut clearing_rate: u32 = config.min_intex_bid_rate;

    for (i, bid) in bids.iter().enumerate() {
        if total_allocated >= supply {
            break;
        }
        if bid.intex_bid_rate < config.min_intex_bid_rate {
            continue;
        }
        if bid.intex_quantity < min_qty {
            continue;
        }

        let allocatable = supply - total_allocated;
        let allocated = (bid.intex_quantity as u32).min(allocatable);

        if allocated > 0 {
            winners.push(bid.bidder_address);
            winner_quantities.push(alloy_primitives::U256::from(allocated));
            won_by_index[i] = allocated;
            total_allocated += allocated;
            clearing_rate = bid.intex_bid_rate;
        }
    }

    let mut all_bidders: Vec<Address> = Vec::with_capacity(len);
    let mut refunded_amounts: Vec<u128> = Vec::with_capacity(len);
    let mut paid_amounts: Vec<u128> = Vec::with_capacity(len);

    for (i, bid) in bids.iter().enumerate() {
        all_bidders.push(bid.bidder_address);

        // locked = quantity * escrow_basis * rate / RATE_SCALE (escrowed at bid time).
        let locked = rate_lock(
            u64::from(bid.intex_quantity),
            escrow_basis,
            bid.intex_bid_rate,
        );

        let won = won_by_index[i];
        if won > 0 {
            // Uniform clearing: winners pay at the clearing rate; refund the rest.
            let paid = rate_lock(u64::from(won), escrow_basis, clearing_rate);
            let refunded = locked.saturating_sub(paid);
            paid_amounts.push(paid);
            refunded_amounts.push(refunded);
        } else {
            paid_amounts.push(0);
            refunded_amounts.push(locked);
        }
    }

    ClearingResult {
        issued_intex_count: total_allocated,
        clearing_rate,
        winners,
        winner_quantities,
        all_bidders,
        refunded_amounts,
        paid_amounts,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_origin_messenger(caller: Address) -> Result<()> {
    if caller != ORIGIN_MESSENGER_ADDRESS {
        return Err(DesisError::UnauthorizedOrigin(caller).into());
    }
    Ok(())
}

fn require_nonzero_series_id(series_id: u32) -> Result<()> {
    if series_id == 0 {
        return Err(DesisError::InvalidSeriesId(0).into());
    }
    Ok(())
}

fn require_stage(
    contract: &DesisContract<'_>,
    series_id: u32,
    expected: AuctionStage,
) -> Result<()> {
    let actual = contract.read_stage(series_id)?;
    if actual != expected {
        return Err(DesisError::InvalidStageTransition.into());
    }
    Ok(())
}
