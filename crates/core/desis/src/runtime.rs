//! Desis runtime: auction lifecycle and clearing algorithm.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, SECONDS_PER_DAY};
use outbe_primitives::units::{
    NATIVE_UNITS_PER_PROTOCOL_UNIT, PROTOCOL_AMOUNT_DECIMALS, SCALE_1E6_U64,
};
use outbe_promislimit::PromisLimitContract;

use outbe_intexfactory::schema::IssuanceParams;
use outbe_intexfactory::SeriesId;

use crate::constants::{
    BIDS_FANIN_TIMEOUT_SECS, BID_QUANTITY_FLOOR_BPS, CLEARING_BIDS_PER_CHUNK, CLEARING_BID_GAS,
    CLEARING_CHUNK_GAS, CLEARING_FIXED_GAS, CLEARING_HISTORY_DAYS, CLEARING_MIN_BIDS,
    COMMIT_WINDOW_SECONDS, DAY_STATE_GREEN, DAY_STATE_RED, IGNORED_CONFLICT, IGNORED_NOT_FOUND,
    IGNORED_OBSOLETE, MAX_BIDS_PER_BATCH, MAX_BID_BATCHES, MAX_REFERENCE_PRICES, MAX_REFUND_CHUNKS,
    MIN_COMMIT_WINDOW_SECONDS, ORIGIN_ROUTER_ADDRESS, PROMIS_LOAD_ANCHOR_ISO,
    PROMIS_LOAD_DEADBAND_BPS, PROMIS_LOAD_LAUNCH_EXPONENT, PROMIS_LOAD_OVERRIDE, REFUND_CHUNK_LEN,
    REVEAL_WINDOW_SECONDS, SETTLEMENT_WINDOW_SECONDS,
};
use crate::errors::DesisError;
use crate::precompile::IDesis;
use crate::schema::{
    AuctionConfig, AuctionStage, BidData, ClearingResult, DesisContract, ReferenceCurrencyPrice,
};
use crate::sol_ext::IOriginRouter;

// ---------------------------------------------------------------------------
// Auction lifecycle
// ---------------------------------------------------------------------------

/// Validate every technical prerequisite before the API may classify
/// an oversized Desis Limit as the sole committed business rejection. Returns the
/// schedule anchor: the UTC midnight of `now`.
pub(crate) fn preflight_brief(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    now: u64,
) -> Result<u32> {
    if !worldwide_day.is_valid() {
        return Err(DesisError::InvalidWorldwideDay(worldwide_day).into());
    }
    let contract = storage.contract::<DesisContract>();
    if contract.read_stage(worldwide_day)? != AuctionStage::None {
        return Err(DesisError::InvalidStageTransition.into());
    }
    let midnight = now - now % SECONDS_PER_DAY;
    u32::try_from(midnight).map_err(|_| PrecompileError::Revert("brief anchor exceeds u32".into()))
}

pub(crate) fn record_preflighted_brief(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    desis_limit_minor: u128,
    is_green: bool,
    anchor: u32,
) -> Result<()> {
    let mut contract = storage.contract::<DesisContract>();
    contract.write_stage(worldwide_day, AuctionStage::Briefed)?;
    contract
        .pending_desis_limit_minor
        .write(&worldwide_day, U256::from(desis_limit_minor))?;
    contract
        .brief_green
        .write(&worldwide_day, u8::from(is_green))?;
    contract.auction_at.write(&worldwide_day, anchor)?;
    contract.push_sched_active(worldwide_day)?;
    Ok(())
}

/// Table bound, not a policy: a priced day carries at least one digit, so the rung
/// the ladder can actually reach is `anchor_digits - 1`. Sized past any launch rate
/// the six-decimal scale can carry, so it never truncates that.
const PROMIS_LOAD_MAX_EXPONENT: u32 = 21;

/// One entry past the widest rung: the deadband brackets against the decade above.
const POW10: [u128; PROMIS_LOAD_MAX_EXPONENT as usize + 2] = {
    let mut table = [1u128; PROMIS_LOAD_MAX_EXPONENT as usize + 2];
    let mut i = 1;
    while i < table.len() {
        table[i] = table[i - 1] * 10;
        i += 1;
    }
    table
};

const _: () = assert!(
    POW10[PROMIS_LOAD_LAUNCH_EXPONENT as usize]
        == 100_000 * 10u128.pow(PROTOCOL_AMOUNT_DECIMALS as u32),
    "the launch rung must carry 100 000 PROMIS"
);

pub(crate) fn promis_load_minor(exponent: u32) -> u128 {
    POW10[exponent.min(PROMIS_LOAD_MAX_EXPONENT) as usize]
}

fn decimal_digits(rate: U256) -> u32 {
    if rate.is_zero() {
        return 0;
    }
    let mut digits = 1u32;
    while (digits as usize) < POW10.len() && rate >= U256::from(POW10[digits as usize]) {
        digits += 1;
    }
    digits
}

/// Digits of the launch pair, captured once and never moved again.
pub(crate) fn launch_anchor_digits(rate: U256) -> u32 {
    PROMIS_LOAD_LAUNCH_EXPONENT + decimal_digits(rate)
}

fn anchor_exponent(anchor_digits: u32, rate: U256) -> u32 {
    anchor_digits
        .saturating_sub(decimal_digits(rate))
        .min(PROMIS_LOAD_MAX_EXPONENT)
}

/// The decade a day quoted at `rate` runs on, holding `current` while the rate stays
/// inside it widened by the deadband. Edges are compared scaled up rather than divided
/// down, so the band survives integer division in the narrow decades.
pub(crate) fn promis_load_exponent(anchor_digits: u32, current: Option<u32>, rate: U256) -> u32 {
    let Some(exponent) = current else {
        return anchor_exponent(anchor_digits, rate);
    };
    let exponent = exponent.min(PROMIS_LOAD_MAX_EXPONENT);
    // Independent cells: their difference is not trusted to stay inside the table.
    let decade = anchor_digits
        .saturating_sub(exponent)
        .clamp(1, POW10.len() as u32 - 1) as usize;
    let scaled = rate * U256::from(10_000u32);
    let lo = U256::from(POW10[decade - 1]) * U256::from(10_000 - PROMIS_LOAD_DEADBAND_BPS);
    let hi = U256::from(POW10[decade]) * U256::from(10_000 + PROMIS_LOAD_DEADBAND_BPS);
    if scaled >= lo && scaled < hi {
        exponent
    } else {
        anchor_exponent(anchor_digits, rate)
    }
}

/// Read at auction start, before `choose_reference_prices` trims the table: either
/// of its rules would drop the anchor currency and the ladder with it.
fn step_promis_load(
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
    reference_prices: &[ReferenceCurrencyPrice],
) -> Result<u128> {
    if let Some(fixed) = PROMIS_LOAD_OVERRIDE {
        return Ok(fixed);
    }
    // A stored zero is "never set": no rate reaches the rung it would stand for.
    let stored = contract.promis_load_exponent.read()?;
    let current = (stored != 0).then_some(stored);
    // Without the anchor currency, the ladder holds. Nothing is captured, since the
    // launch pair needs a rate to be a pair.
    let Some(rate) = reference_prices
        .iter()
        .find(|row| row.iso_code == PROMIS_LOAD_ANCHOR_ISO)
        .map(|row| row.entry_price_minor)
    else {
        return Ok(promis_load_minor(
            current.unwrap_or(PROMIS_LOAD_LAUNCH_EXPONENT),
        ));
    };
    let anchor_digits = match contract.promis_load_anchor_digits.read()? {
        0 => {
            let digits = launch_anchor_digits(rate);
            contract.promis_load_anchor_digits.write(digits)?;
            digits
        }
        digits => digits,
    };
    let exponent = promis_load_exponent(anchor_digits, current, rate);
    let load = promis_load_minor(exponent);
    match current {
        Some(previous) if previous == exponent => {}
        Some(previous) => {
            contract.emit(IDesis::PromisLoadStepped {
                worldwideDay: worldwide_day.into(),
                previousLoadMinor: promis_load_minor(previous),
                newLoadMinor: load,
                coenUsdRateMinor: rate,
            })?;
            contract.promis_load_exponent.write(exponent)?;
        }
        // Taking the first position is not a step. The config and START message carry it.
        None => contract.promis_load_exponent.write(exponent)?,
    }
    Ok(load)
}

/// The currencies a day will actually price: one per series-id letter, at most
/// `MAX_REFERENCE_PRICES`. Ordered by currency first, so the two brief paths - which
/// collect prices in different orders - resolve a day to the same table.
fn choose_reference_prices(
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
    mut rows: Vec<ReferenceCurrencyPrice>,
) -> Result<Vec<ReferenceCurrencyPrice>> {
    rows.sort_by_key(|row| row.iso_code);

    let letter_of = |iso_code: u16| SeriesId::currency_code(iso_code).map(|code| code[0]).ok();
    let mut kept: Vec<ReferenceCurrencyPrice> = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(letter) = letter_of(row.iso_code) else {
            continue;
        };
        if let Some(taken) = kept.iter().find(|k| letter_of(k.iso_code) == Some(letter)) {
            contract.emit(IDesis::ReferenceCurrencyLetterTaken {
                worldwideDay: worldwide_day.into(),
                isoCode: row.iso_code,
                takenBy: taken.iso_code,
            })?;
            continue;
        }
        if kept.len() == MAX_REFERENCE_PRICES {
            contract.emit(IDesis::ReferenceCurrencyOverCap {
                worldwideDay: worldwide_day.into(),
                isoCode: row.iso_code,
                cap: MAX_REFERENCE_PRICES as u8,
            })?;
            continue;
        }
        kept.push(row);
    }
    Ok(kept)
}

/// Fold the prior-clearing bid floor and the genesis profile into the config,
/// so the persisted config carries the same values the wire message ships.
fn fold_profile(
    storage: &StorageHandle<'_>,
    contract: &DesisContract<'_>,
    config: &mut AuctionConfig,
) -> Result<outbe_intexfactory::IntexParams> {
    // minBidQty = 4% of the prior clearing's issued count, restated at today's
    // load. The same PROMIS splits into ten times more Intexes one decade down
    // the ladder, so a floor left at yesterday's scale could sit above the whole
    // of today's tirage and clear the day to nothing.
    let min_bid_qty: u16 = {
        let last_worldwide_day = contract.read_last_cleared_worldwide_day()?;
        let today_load = config.promis_load_minor;
        if last_worldwide_day.value() == 0 || today_load == 0 {
            0
        } else {
            let prev_issued = u128::from(contract.read_last_clearing_issued_count()?);
            let prev_load = u128::try_from(
                contract
                    .config_promis_load_minor
                    .read(&last_worldwide_day)?,
            )
            .map_err(|_| DesisError::InvalidWorldwideDay(last_worldwide_day))?;
            let scaled = prev_issued
                .checked_mul(prev_load)
                .and_then(|v| v.checked_mul(u128::from(BID_QUANTITY_FLOOR_BPS)))
                .ok_or_else(|| {
                    PrecompileError::Revert("min bid quantity scaling overflow".into())
                })?;
            let derived = scaled / (10_000 * today_load);
            derived.min(u128::from(u16::MAX)) as u16
        }
    };
    let iparams = outbe_intexfactory::read_params(storage)?;
    config.min_intex_bid_quantity = min_bid_qty;
    config.call_trigger = crate::schema::IntexCallTrigger {
        call_window_seconds: iparams.call_window_seconds,
        call_threshold_seconds: iparams.call_threshold_seconds,
        call_notice_period_seconds: iparams.call_notice_period_seconds,
    };
    config.commit_bond_minor = iparams.commit_bond_minor;
    Ok(iparams)
}

/// What one AUCTION_STAGE_START message announces besides its day state.
#[derive(Clone, Copy)]
struct StageStart<'a> {
    worldwide_day: WorldwideDay,
    config: &'a AuctionConfig,
    iparams: &'a outbe_intexfactory::IntexParams,
    commit_end: u32,
    reveal_end: u32,
    issuance_end: u32,
}

/// Broadcast AUCTION_STAGE_START with the given schedule and day state.
fn send_stage_start(
    storage: &StorageHandle<'_>,
    start: StageStart<'_>,
    day_state: u8,
) -> Result<()> {
    let StageStart {
        worldwide_day,
        config,
        iparams,
        commit_end,
        reveal_end,
        issuance_end,
    } = start;
    let mut prices = Vec::with_capacity(config.reference_prices.len());
    for row in &config.reference_prices {
        let floor = outbe_intexfactory::marked_up(row.entry_price_minor, iparams.floor_rate)?;
        let call = outbe_intexfactory::marked_up(row.entry_price_minor, iparams.call_rate)?;
        prices.push(IOriginRouter::ReferenceCurrencyPrice {
            isoCode: row.iso_code,
            entryPriceMinor: outbe_intexfactory::to_wire_price(row.entry_price_minor)?,
            floorPriceMinor: outbe_intexfactory::to_wire_price(floor)?,
            callPriceMinor: outbe_intexfactory::to_wire_price(call)?,
        });
    }
    let stage_params = IOriginRouter::AuctionStageStartParams {
        worldwideDay: worldwide_day.into(),
        commitEnd: commit_end,
        revealEnd: reveal_end,
        issuanceEnd: issuance_end,
        promisLoadMinor: config.promis_load_minor,
        minIntexBidRate: config.min_intex_bid_rate,
        prices,
        callNoticePeriod: iparams.call_notice_period_seconds,
        callWindow: iparams.call_window_seconds,
        callThreshold: iparams.call_threshold_seconds,
        minIntexBidQuantity: config.min_intex_bid_quantity,
        commitBondMinor: config.commit_bond_minor,
        dayState: day_state,
    };
    // Relay-float-funded: value 0, so the router self-quotes and pays the bridge fee from its float.
    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendAuctionStageStartCall {
            params: stage_params,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}

mod algorithm;
mod clearing;
mod intake;
mod schedule;

#[cfg(test)]
pub(crate) use algorithm::rate_lock;
pub use clearing::tick_gate;
#[cfg(test)]
pub(crate) use clearing::{
    clearing_round_gas, force_clear, refund_chunk_count, refund_chunks, RefundChunk,
};
pub use intake::{process_bids_batch, process_bids_done, Inbound};
#[cfg(test)]
pub(crate) use schedule::schedule_tick;
pub use schedule::tick_schedule;
