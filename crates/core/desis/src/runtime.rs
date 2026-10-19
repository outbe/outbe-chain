//! Desis runtime: auction lifecycle and clearing algorithm.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, SECONDS_PER_DAY};
use outbe_primitives::units::{PROTOCOL_AMOUNT_DECIMALS, SCALE_1E6_U64};
use outbe_promislimit::PromisLimitContract;

use outbe_intexfactory::schema::IssuanceParams;
use outbe_intexfactory::SeriesId;

use crate::constants::{
    BIDS_FANIN_TIMEOUT_SECS, BID_QUANTITY_FLOOR_BPS, CLEARING_BIDS_PER_CHUNK, CLEARING_BID_GAS,
    CLEARING_CHUNK_GAS, CLEARING_FIXED_GAS, CLEARING_HISTORY_DAYS, CLEARING_MIN_BIDS,
    COMMIT_WINDOW_SECONDS, DAY_STATE_GREEN, DAY_STATE_RED, IGNORED_CONFLICT, IGNORED_NOT_FOUND,
    IGNORED_OBSOLETE, MAX_BIDS_PER_BATCH, MAX_BID_BATCHES, MAX_REFUND_CHUNKS,
    MIN_COMMIT_WINDOW_SECONDS, ORIGIN_ROUTER_ADDRESS, PROMIS_LOAD_LAUNCH_EXPONENT,
    PROMIS_LOAD_OVERRIDE, REFUND_CHUNK_LEN, REVEAL_WINDOW_SECONDS, SETTLEMENT_WINDOW_SECONDS,
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
    u32::try_from(midnight).map_err(|_| DesisError::BriefAnchorExceedsU32.into())
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

/// Table bound, not a policy: the widest rung the load can climb to.
const PROMIS_LOAD_MAX_EXPONENT: u32 = 21;

/// A stored zero reads as "never set", so the load never steps onto it.
const PROMIS_LOAD_MIN_EXPONENT: u32 = 1;

const POW10: [u128; PROMIS_LOAD_MAX_EXPONENT as usize + 1] = {
    let mut table = [1u128; PROMIS_LOAD_MAX_EXPONENT as usize + 1];
    let mut i = 1;
    while i < table.len() {
        table[i] = table[i - 1] * 10;
        i += 1;
    }
    table
};

const _: () = assert!(
    POW10[PROMIS_LOAD_LAUNCH_EXPONENT as usize]
        == 1_000_000 * 10u128.pow(PROTOCOL_AMOUNT_DECIMALS as u32),
    "the launch rung must carry 1 000 000 PROMIS"
);

pub(crate) fn promis_load_minor(exponent: u32) -> u128 {
    POW10[exponent.min(PROMIS_LOAD_MAX_EXPONENT) as usize]
}

/// Whole decades `rate` has moved from `reference`: positive up, negative down.
pub(crate) fn decades_moved(rate: U256, reference: U256) -> i32 {
    let (high, low, sign) = if rate >= reference {
        (rate, reference, 1)
    } else {
        (reference, rate, -1)
    };
    let ten = U256::from(10u8);
    let mut decades = 0;
    let mut next = low.saturating_mul(ten);
    while next <= high && decades < PROMIS_LOAD_MAX_EXPONENT as i32 {
        decades += 1;
        next = next.saturating_mul(ten);
    }
    sign * decades
}

/// One checked currency: today's rate against the price it was captured at.
struct LoadMove {
    iso_code: u16,
    rate: U256,
    reference: U256,
    decades: i32,
}

/// The currency that moved furthest, the lowest ISO on a tie. Moves both ways hold.
fn load_trigger(moves: &[LoadMove]) -> Option<&LoadMove> {
    if moves.iter().any(|m| m.decades > 0) && moves.iter().any(|m| m.decades < 0) {
        return None;
    }
    moves
        .iter()
        .filter(|m| m.decades != 0)
        .fold(None, |best: Option<&LoadMove>, m| match best {
            Some(b) if b.decades.abs() >= m.decades.abs() => Some(b),
            _ => Some(m),
        })
}

/// COEN dearer by a decade means ten times fewer PROMIS per Intex.
pub(crate) fn stepped_exponent(current: u32, decades: i32) -> u32 {
    let shift = decades.unsigned_abs();
    if decades > 0 {
        current.saturating_sub(shift).max(PROMIS_LOAD_MIN_EXPONENT)
    } else {
        current.saturating_add(shift).min(PROMIS_LOAD_MAX_EXPONENT)
    }
}

/// The row's move against its capture, or `None` when the capture is missing or stale.
fn captured_move(
    contract: &DesisContract<'_>,
    row: &ReferenceCurrencyPrice,
    current: u32,
) -> Result<Option<LoadMove>> {
    let reference = contract.promis_load_reference_minor.read(&row.iso_code)?;
    if reference.is_zero()
        || contract
            .promis_load_reference_exponent
            .read(&row.iso_code)?
            != current
    {
        return Ok(None);
    }
    Ok(Some(LoadMove {
        iso_code: row.iso_code,
        rate: row.entry_price_minor,
        reference,
        decades: decades_moved(row.entry_price_minor, reference),
    }))
}

fn capture_reference(
    contract: &mut DesisContract<'_>,
    row: &ReferenceCurrencyPrice,
    exponent: u32,
) -> Result<()> {
    contract
        .promis_load_reference_minor
        .write(&row.iso_code, row.entry_price_minor)?;
    contract
        .promis_load_reference_exponent
        .write(&row.iso_code, exponent)
}

/// Read before `choose_reference_prices` trims the table, so every priced currency is
/// checked. A currency without a valid capture takes one and sits the day out.
pub(crate) fn step_promis_load(
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
    reference_prices: &[ReferenceCurrencyPrice],
) -> Result<u128> {
    if let Some(fixed) = PROMIS_LOAD_OVERRIDE {
        return Ok(fixed);
    }
    let stored = contract.promis_load_exponent.read()?;
    let current = match stored {
        0 => PROMIS_LOAD_LAUNCH_EXPONENT,
        k => k.min(PROMIS_LOAD_MAX_EXPONENT),
    };
    // A day nobody priced captures nothing: the chain has not taken a position yet.
    if reference_prices.is_empty() {
        return Ok(promis_load_minor(current));
    }
    // Taking the first position is not a step. The config and START message carry it.
    if stored == 0 {
        contract.promis_load_exponent.write(current)?;
    }
    let mut moves = Vec::with_capacity(reference_prices.len());
    let mut uncaptured = Vec::new();
    for row in reference_prices {
        match captured_move(contract, row, current)? {
            Some(m) => moves.push(m),
            None => uncaptured.push(row),
        }
    }
    let stepped = load_trigger(&moves)
        .map(|t| (t, stepped_exponent(current, t.decades)))
        .filter(|(_, exponent)| *exponent != current);
    let Some((trigger, exponent)) = stepped else {
        for row in uncaptured {
            capture_reference(contract, row, current)?;
        }
        return Ok(promis_load_minor(current));
    };
    contract.emit(IDesis::PromisLoadStepped {
        worldwideDay: worldwide_day.into(),
        previousLoadMinor: promis_load_minor(current),
        newLoadMinor: promis_load_minor(exponent),
        isoCode: trigger.iso_code,
        rateMinor: trigger.rate,
        referenceMinor: trigger.reference,
    })?;
    contract.promis_load_exponent.write(exponent)?;
    for row in reference_prices {
        capture_reference(contract, row, exponent)?;
    }
    Ok(promis_load_minor(exponent))
}

/// The currencies a day will actually price: one per series-id letter. Ordered by currency first, so the two brief paths - which
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
) -> Result<outbe_intex::IntexParams> {
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
                .ok_or(DesisError::MinBidQuantityScalingOverflow)?;
            let derived = scaled / (10_000 * today_load);
            derived.min(u128::from(u16::MAX)) as u16
        }
    };
    let iparams = outbe_intex::config::read(storage)?;
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
    iparams: &'a outbe_intex::IntexParams,
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
        let call = outbe_intexfactory::marked_up(row.entry_price_minor, iparams.call_rate)?;
        prices.push(IOriginRouter::ReferenceCurrencyPrice {
            isoCode: row.iso_code,
            entryPriceMinor: outbe_intexfactory::to_wire_price(row.entry_price_minor)?,
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
