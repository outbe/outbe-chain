//! Desis runtime: auction lifecycle and clearing algorithm.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::DESIS_ADDRESS;
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
use crate::sol_ext::{IOriginMessenger, MessagingFee};

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
    config: AuctionConfig,
) -> Result<()> {
    if series_id == 0 {
        return Err(DesisError::InvalidSeriesId(0).into());
    }
    if config.promis_load_minor == 0 || config.cost_amount_minor() == 0 {
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

    contract.write_auction_config(series_id, &config)?;
    contract
        .config_min_bid_quantity
        .write(&series_id, u32::from(min_bid_qty))?;
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
    // floor/call/trigger from the IntexFactory profile; floor/call derive from entry.
    let iparams = outbe_intexfactory::read_params(&storage)?;
    let floor_price = config
        .entry_price
        .checked_mul(U256::from(iparams.floor_price_num))
        .map(|v| v / U256::from(FLOOR_PRICE_DEN))
        .ok_or_else(|| PrecompileError::Revert("entry floor overflow".into()))?;
    let call_price = config
        .entry_price
        .checked_mul(U256::from(iparams.call_price_num))
        .map(|v| v / U256::from(CALL_PRICE_DEN))
        .ok_or_else(|| PrecompileError::Revert("entry call overflow".into()))?;
    let entry_price_u64 = u64::try_from(config.entry_price)
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
    let quote_ret = storage.staticcall(
        ORIGIN_MESSENGER_ADDRESS,
        IOriginMessenger::quoteSendAuctionStageStartCall {
            params: stage_params.clone(),
            extraOptions: Bytes::new(),
            payInLzToken: false,
        }
        .abi_encode()
        .into(),
    )?;
    let start_fee =
        IOriginMessenger::quoteSendAuctionStageStartCall::abi_decode_returns(&quote_ret)
            .map_err(|_| PrecompileError::Revert("quote auction start undecodable".into()))?;
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionStageStartCall {
            params: stage_params,
            extraOptions: Bytes::new(),
            fee: MessagingFee {
                nativeFee: start_fee.nativeFee,
                lzTokenFee: start_fee.lzTokenFee,
            },
            refundAddress: DESIS_ADDRESS,
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

    let quote_ret = storage.staticcall(
        ORIGIN_MESSENGER_ADDRESS,
        IOriginMessenger::quoteSendAuctionStageRevealCall {
            seriesId: series_id,
            isGreenDay: is_green_day,
            extraOptions: Bytes::new(),
            payInLzToken: false,
        }
        .abi_encode()
        .into(),
    )?;
    let reveal_fee =
        IOriginMessenger::quoteSendAuctionStageRevealCall::abi_decode_returns(&quote_ret)
            .map_err(|_| PrecompileError::Revert("quote auction reveal undecodable".into()))?;
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionStageRevealCall {
            seriesId: series_id,
            isGreenDay: is_green_day,
            extraOptions: Bytes::new(),
            fee: MessagingFee {
                nativeFee: reveal_fee.nativeFee,
                lzTokenFee: reveal_fee.lzTokenFee,
            },
            refundAddress: DESIS_ADDRESS,
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

    let quote_ret = storage.staticcall(
        ORIGIN_MESSENGER_ADDRESS,
        IOriginMessenger::quoteSendAuctionStageClearingCall {
            seriesId: series_id,
            extraOptions: Bytes::new(),
            payInLzToken: false,
        }
        .abi_encode()
        .into(),
    )?;
    let clearing_fee =
        IOriginMessenger::quoteSendAuctionStageClearingCall::abi_decode_returns(&quote_ret)
            .map_err(|_| PrecompileError::Revert("quote auction clearing undecodable".into()))?;
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionStageClearingCall {
            seriesId: series_id,
            extraOptions: Bytes::new(),
            fee: MessagingFee {
                nativeFee: clearing_fee.nativeFee,
                lzTokenFee: clearing_fee.lzTokenFee,
            },
            refundAddress: DESIS_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;

    Ok(rounding_remainder)
}

// ---------------------------------------------------------------------------
// Bid ingestion
// ---------------------------------------------------------------------------

/// Accept a relayed bid batch. Bids accumulate while stage is `Revealing`.
/// A higher `generation` flushes all prior bids. The final batch (`is_last`)
/// transitions to `BidsReceived` if any bids exist, else to `Cancelled`.
pub fn process_bids_batch(
    storage: StorageHandle<'_>,
    caller: Address,
    series_id: u32,
    src_eid: u32,
    is_last: bool,
    generation: u32,
    bids: Vec<BidData>,
) -> Result<()> {
    require_origin_messenger(caller)?;
    require_nonzero_series_id(series_id)?;
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
        // New generation: flush old bids by zeroing the count (old entries become unreachable).
        contract.bid_count.write(&series_id, 0)?;
        contract.write_bid_batch_meta(series_id, src_eid, generation)?;
        contract.replace_bids(series_id, &bids)?;
    } else {
        contract.write_bid_batch_meta(series_id, src_eid, generation)?;
        for bid in &bids {
            contract.append_bid(series_id, bid)?;
        }
    }

    if is_last {
        let count = contract.read_bid_count(series_id)?;
        if count > 0 {
            contract.write_stage(series_id, AuctionStage::BidsReceived)?;
            contract.emit(IDesis::BidsReceived {
                seriesId: series_id,
                srcEid: src_eid,
                bidsCount: U256::from(count),
            })?;
        } else {
            contract.write_stage(series_id, AuctionStage::Cancelled)?;
            contract.emit(IDesis::AuctionCancelledNoBids {
                seriesId: series_id,
            })?;
        }
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
    let bids = contract.read_all_bids(series_id)?;
    if bids.is_empty() {
        return Err(DesisError::PendingClearingDataMissing(series_id).into());
    }

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
            entry_price_minor: config.entry_price,
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
    let quote_ret = storage.staticcall(
        ORIGIN_MESSENGER_ADDRESS,
        IOriginMessenger::quoteSendAuctionResultCall {
            seriesId: series_id,
            issuedIntexCount: result.issued_intex_count,
            auctionClearingRate: u64::from(result.clearing_rate),
            wonBidsCount: won_bids_count,
            extraOptions: Bytes::new(),
            payInLzToken: false,
        }
        .abi_encode()
        .into(),
    )?;
    let result_fee =
        IOriginMessenger::quoteSendAuctionResultCall::abi_decode_returns(&quote_ret)
            .map_err(|_| PrecompileError::Revert("quote auction result undecodable".into()))?;
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendAuctionResultCall {
            seriesId: series_id,
            issuedIntexCount: result.issued_intex_count,
            auctionClearingRate: u64::from(result.clearing_rate),
            wonBidsCount: won_bids_count,
            extraOptions: Bytes::new(),
            fee: MessagingFee {
                nativeFee: result_fee.nativeFee,
                lzTokenFee: result_fee.lzTokenFee,
            },
            refundAddress: DESIS_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;

    // Send REFUND_INSTRUCTIONS to BNB (all bidders including losers).
    if !result.all_bidders.is_empty() {
        let quote_ret = storage.staticcall(
            ORIGIN_MESSENGER_ADDRESS,
            IOriginMessenger::quoteSendRefundInstructionsCall {
                seriesId: series_id,
                bidders: result.all_bidders.clone(),
                refundedAmounts: result.refunded_amounts.clone(),
                paidAmounts: result.paid_amounts.clone(),
                extraOptions: Bytes::new(),
                payInLzToken: false,
            }
            .abi_encode()
            .into(),
        )?;
        let refund_fee =
            IOriginMessenger::quoteSendRefundInstructionsCall::abi_decode_returns(&quote_ret)
                .map_err(|_| {
                    PrecompileError::Revert("quote refund instructions undecodable".into())
                })?;
        storage.call(
            ORIGIN_MESSENGER_ADDRESS,
            U256::ZERO,
            IOriginMessenger::sendRefundInstructionsCall {
                seriesId: series_id,
                bidders: result.all_bidders.clone(),
                refundedAmounts: result.refunded_amounts.clone(),
                paidAmounts: result.paid_amounts.clone(),
                extraOptions: Bytes::new(),
                fee: MessagingFee {
                    nativeFee: refund_fee.nativeFee,
                    lzTokenFee: refund_fee.lzTokenFee,
                },
                refundAddress: DESIS_ADDRESS,
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
/// per-Intex `strike`: `qty * strike * rate / RATE_SCALE`, saturating to u64.
fn rate_lock(qty: u64, strike: u64, rate: u32) -> u64 {
    let amount = U256::from(qty)
        .saturating_mul(U256::from(strike))
        .saturating_mul(U256::from(rate))
        / U256::from(RATE_SCALE);
    u64::try_from(amount).unwrap_or(u64::MAX)
}

/// Uniform-rate clearing: allocate sorted bids until `supply` runs out; the
/// clearing rate is the last allocated bid's. lock/pay = qty * strike * rate / RATE_SCALE.
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

    let strike = config.cost_amount_minor();
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
    let mut refunded_amounts: Vec<u64> = Vec::with_capacity(len);
    let mut paid_amounts: Vec<u64> = Vec::with_capacity(len);

    for (i, bid) in bids.iter().enumerate() {
        all_bidders.push(bid.bidder_address);

        // locked = quantity * strike * rate / RATE_SCALE (escrowed at bid time).
        let locked = rate_lock(u64::from(bid.intex_quantity), strike, bid.intex_bid_rate);

        let won = won_by_index[i];
        if won > 0 {
            // Uniform clearing: winners pay at the clearing rate; refund the rest.
            let paid = rate_lock(u64::from(won), strike, clearing_rate);
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
