//! Desis runtime: auction lifecycle and clearing algorithm.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{date_key_to_utc_timestamp, timestamp_to_date_key};
use outbe_promislimit::PromisLimitContract;

use outbe_intexfactory::constants::{CALL_PRICE_DEN, FLOOR_PRICE_DEN};

use crate::constants::{
    BIDS_FANIN_TIMEOUT_SECS, BID_QUANTITY_FLOOR_BPS, COMMIT_WINDOW_SECONDS, DAY_STATE_GREEN,
    DAY_STATE_RED, ISSUANCE_WINDOW_SECONDS, ORIGIN_ROUTER_ADDRESS, QUALIFIER_ISSUANCE_ISO,
    QUALIFIER_REFERENCE_ISO, RATE_SCALE, REVEAL_WINDOW_SECONDS, SETTLEMENT_WINDOW_SECONDS,
};
use crate::errors::DesisError;
use crate::precompile::IDesis;
use crate::schema::{AuctionConfig, AuctionStage, BidData, ClearingResult, DesisContract};
use crate::sol_ext::IOriginRouter;

// ---------------------------------------------------------------------------
// Auction lifecycle
// ---------------------------------------------------------------------------

/// Record the day's auction brief: supply (raw PROMIS), entry price and day
/// type. The schedule anchor is the midnight of `now`.
pub fn record_brief(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    supply_promis: u128,
    entry_price: U256,
    is_green: bool,
    now: u64,
) -> Result<()> {
    if worldwide_day == 0 {
        return Err(DesisError::InvalidWorldwideDay(0).into());
    }
    let mut contract = storage.contract::<DesisContract>();
    if contract.read_stage(worldwide_day)? != AuctionStage::None {
        return Err(DesisError::InvalidStageTransition.into());
    }
    let anchor = u32::try_from(date_key_to_utc_timestamp(timestamp_to_date_key(now)))
        .map_err(|_| PrecompileError::Revert("brief anchor exceeds u32".into()))?;

    contract.write_auction_config(worldwide_day, &AuctionConfig::from_entry_price(entry_price))?;
    contract.write_stage(worldwide_day, AuctionStage::Briefed)?;
    contract
        .pending_supply_promis
        .write(&worldwide_day, U256::from(supply_promis))?;
    contract
        .brief_green
        .write(&worldwide_day, u8::from(is_green))?;
    contract.auction_at.write(&worldwide_day, anchor)?;
    contract.push_sched_active(worldwide_day)?;
    contract.emit(IDesis::AuctionCreated {
        worldwideDay: worldwide_day,
    })?;
    Ok(())
}

/// Create a new auction for `worldwide_day` and transition to `Started`.
///
/// Derives minBidQty from the prior clearing (4% of issued count) and
/// validates the config.
pub fn start_auction(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    auction_timestamp: u64,
    mut config: AuctionConfig,
) -> Result<()> {
    if worldwide_day == 0 {
        return Err(DesisError::InvalidWorldwideDay(0).into());
    }
    if config.promis_load_minor == 0 || config.escrow_basis_minor() == 0 {
        return Err(DesisError::InvalidWorldwideDay(worldwide_day).into());
    }

    let mut contract = storage.contract::<DesisContract>();

    // Duplicate guard.
    let existing = contract.read_stage(worldwide_day)?;
    if existing != AuctionStage::None {
        return Err(DesisError::InvalidStageTransition.into());
    }

    let iparams = fold_profile(&storage, &contract, &mut config)?;

    let auction_at = u32::try_from(auction_timestamp)
        .map_err(|_| PrecompileError::Revert("auction timestamp exceeds u32".into()))?;

    contract.write_auction_config(worldwide_day, &config)?;
    contract.write_stage(worldwide_day, AuctionStage::Started)?;
    contract.auction_at.write(&worldwide_day, auction_at)?;
    contract.emit(IDesis::AuctionCreated {
        worldwideDay: worldwide_day,
    })?;

    // revealEnd = noon of the auction day; commitEnd/issuanceEnd are protocol offsets.
    let noon = auction_noon(auction_timestamp)?;
    let commit_end = noon.saturating_sub(REVEAL_WINDOW_SECONDS);
    let issuance_end = noon.saturating_add(ISSUANCE_WINDOW_SECONDS);
    send_stage_start(
        &storage,
        worldwide_day,
        &config,
        &iparams,
        commit_end,
        noon,
        issuance_end,
        0,
    )
}

/// Fold the prior-clearing bid floor and the genesis profile into the config,
/// so the persisted config carries the same values the wire message ships.
fn fold_profile(
    storage: &StorageHandle<'_>,
    contract: &DesisContract<'_>,
    config: &mut AuctionConfig,
) -> Result<outbe_intexfactory::IntexParams> {
    // minBidQty = 4% of the prior clearing's issued count.
    let min_bid_qty: u16 = {
        let last_worldwide_day = contract.read_last_cleared_worldwide_day()?;
        if last_worldwide_day != 0 {
            let prev_issued = contract.read_last_clearing_issued_count()?;
            let derived =
                (prev_issued as u64).saturating_mul(BID_QUANTITY_FLOOR_BPS as u64) / 10_000;
            derived.min(u16::MAX as u64) as u16
        } else {
            0
        }
    };
    let iparams = outbe_intexfactory::read_params(storage)?;
    config.min_intex_bid_quantity = min_bid_qty;
    config.call_trigger = crate::schema::IntexCallTrigger {
        window_days: iparams.call_window_days,
        threshold_days: iparams.call_threshold_days,
        intex_call_period: iparams.intex_call_period_secs,
    };
    config.commit_bond_minor = iparams.commit_bond_minor;
    Ok(iparams)
}

/// Broadcast AUCTION_STAGE_START with the given schedule and day state.
#[allow(clippy::too_many_arguments)]
fn send_stage_start(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    config: &AuctionConfig,
    iparams: &outbe_intexfactory::IntexParams,
    commit_end: u32,
    reveal_end: u32,
    issuance_end: u32,
    day_state: u8,
) -> Result<()> {
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
    let stage_params = IOriginRouter::AuctionStageStartParams {
        worldwideDay: worldwide_day,
        commitEnd: commit_end,
        revealEnd: reveal_end,
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

/// Noon (12:00 UTC) of the day containing `auction_timestamp`.
pub(crate) fn auction_noon(auction_timestamp: u64) -> Result<u32> {
    u32::try_from(date_key_to_utc_timestamp(timestamp_to_date_key(auction_timestamp)) + 12 * 3600)
        .map_err(|_| PrecompileError::Revert("auction day noon exceeds u32".into()))
}

/// Signal `Started` → `Revealing` (green day) or `Started` → `Cancelled` (red day).
pub fn reveal_auction(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    is_green_day: bool,
) -> Result<()> {
    require_nonzero_worldwide_day(worldwide_day)?;
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, worldwide_day, AuctionStage::Started)?;
    let next = if is_green_day {
        AuctionStage::Revealing
    } else {
        AuctionStage::Cancelled
    };
    contract.write_stage(worldwide_day, next)?;
    if !is_green_day {
        contract.emit(IDesis::AuctionCancelledRedDay {
            worldwideDay: worldwide_day,
        })?;
    }

    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendAuctionStageRevealCall {
            worldwideDay: worldwide_day,
            isGreenDay: is_green_day,
        }
        .abi_encode()
        .into(),
    )?;

    Ok(())
}

/// Signal `Revealing` → clearing: store supply, arm the bid fan-in gate and
/// broadcast the clearing stage; returns the Promis rounding remainder
/// (supply_promis % promis_load_minor) to be returned to PromisLimit.
pub fn begin_clearing(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    supply_promis: u128,
    now: u64,
) -> Result<u128> {
    require_nonzero_worldwide_day(worldwide_day)?;
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, worldwide_day, AuctionStage::Revealing)?;

    let config = contract.read_auction_config(worldwide_day)?;
    if config.promis_load_minor == 0 {
        return Err(DesisError::InvalidWorldwideDay(worldwide_day).into());
    }

    let supply_intex = supply_promis / config.promis_load_minor;
    let supply_intex32 =
        u32::try_from(supply_intex).map_err(|_| DesisError::InvalidWorldwideDay(worldwide_day))?;
    let rounding_remainder = supply_promis % config.promis_load_minor;

    contract.clearing_initiated.write(&worldwide_day, 1u8)?;
    contract
        .pending_supply_intex
        .write(&worldwide_day, supply_intex32)?;
    contract
        .clearing_deadline
        .write(&worldwide_day, now.saturating_add(BIDS_FANIN_TIMEOUT_SECS))?;
    contract.push_gate_active(worldwide_day)?;

    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendAuctionStageClearingCall {
            worldwideDay: worldwide_day,
        }
        .abi_encode()
        .into(),
    )?;

    Ok(rounding_remainder)
}

// ---------------------------------------------------------------------------
// Schedule tick
// ---------------------------------------------------------------------------

/// Cycle `auction_advance` trigger: advance every scheduled auction. Each day
/// runs in its own checkpoint — an Err rolls that day back (retried next slot).
pub fn tick_schedule(ctx: &BlockRuntimeContext) -> Result<()> {
    schedule_tick(&ctx.storage, ctx.block.timestamp)
}

pub(crate) fn schedule_tick(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let count = {
        let contract = storage.contract::<DesisContract>();
        contract.sched_active_count.read()?
    };
    if count == 0 {
        return Ok(());
    }
    // Snapshot the set before iterating: transitions swap-pop it.
    let mut days = Vec::with_capacity(count as usize);
    {
        let contract = storage.contract::<DesisContract>();
        for i in 0..count {
            days.push(contract.sched_active_at.read(&i)?);
        }
    }
    for day in days {
        let res = storage.with_checkpoint(|| advance_day(storage, day, now));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::desis", day, error = ?e, "schedule tick: skipping day");
        }
    }
    Ok(())
}

/// Walk one day's schedule: start at the anchor, flip to Revealing at commit
/// end, arm the clearing gate at reveal end, retire overdue and terminal days.
fn advance_day(storage: &StorageHandle<'_>, worldwide_day: u32, now: u64) -> Result<()> {
    loop {
        let mut contract = storage.contract::<DesisContract>();
        let stage = contract.read_stage(worldwide_day)?;
        let anchor = u64::from(contract.auction_at.read(&worldwide_day)?);
        let commit_end = anchor.saturating_add(COMMIT_WINDOW_SECONDS);
        let reveal_end = commit_end.saturating_add(u64::from(REVEAL_WINDOW_SECONDS));
        let issuance_end = reveal_end.saturating_add(SETTLEMENT_WINDOW_SECONDS);
        match stage {
            AuctionStage::Cleared | AuctionStage::Cancelled => {
                return contract.remove_sched_active(worldwide_day);
            }
            _ if now >= issuance_end => {
                contract.emit(IDesis::AuctionOverdue {
                    worldwideDay: worldwide_day,
                })?;
                return contract.remove_sched_active(worldwide_day);
            }
            AuctionStage::Briefed if now >= anchor => {
                let mut config = contract.read_auction_config(worldwide_day)?;
                let iparams = fold_profile(storage, &contract, &mut config)?;
                contract.write_auction_config(worldwide_day, &config)?;
                let ends = (ts32(commit_end)?, ts32(reveal_end)?, ts32(issuance_end)?);
                if contract.brief_green.read(&worldwide_day)? == 0 {
                    send_stage_start(
                        storage,
                        worldwide_day,
                        &config,
                        &iparams,
                        ends.0,
                        ends.1,
                        ends.2,
                        DAY_STATE_RED,
                    )?;
                    contract.write_stage(worldwide_day, AuctionStage::Cancelled)?;
                    contract.emit(IDesis::AuctionCancelledRedDay {
                        worldwideDay: worldwide_day,
                    })?;
                    return contract.remove_sched_active(worldwide_day);
                }
                if now >= commit_end {
                    contract.emit(IDesis::AuctionDispatchFailed {
                        worldwideDay: worldwide_day,
                        stage: "auction_stage_start".into(),
                        reason: "commit window elapsed".into(),
                    })?;
                    contract.write_stage(worldwide_day, AuctionStage::Cancelled)?;
                    return contract.remove_sched_active(worldwide_day);
                }
                send_stage_start(
                    storage,
                    worldwide_day,
                    &config,
                    &iparams,
                    ends.0,
                    ends.1,
                    ends.2,
                    DAY_STATE_GREEN,
                )?;
                contract.write_stage(worldwide_day, AuctionStage::Started)?;
            }
            AuctionStage::Started if now >= commit_end => {
                contract.write_stage(worldwide_day, AuctionStage::Revealing)?;
            }
            AuctionStage::Revealing if now >= reveal_end => {
                if contract.clearing_initiated.read(&worldwide_day)? != 0 {
                    return Ok(());
                }
                return arm_clearing(storage, worldwide_day, now);
            }
            _ => return Ok(()),
        }
    }
}

/// u32 wire timestamp (bounded until 2106).
fn ts32(ts: u64) -> Result<u32> {
    u32::try_from(ts).map_err(|_| PrecompileError::Revert("schedule timestamp exceeds u32".into()))
}

/// Arm the clearing from the brief supply: convert raw PROMIS to whole Intex
/// units, start the fan-in gate and broadcast the clearing stage.
fn arm_clearing(storage: &StorageHandle<'_>, worldwide_day: u32, now: u64) -> Result<()> {
    let mut contract = storage.contract::<DesisContract>();
    let config = contract.read_auction_config(worldwide_day)?;
    if config.promis_load_minor == 0 {
        return Err(DesisError::InvalidWorldwideDay(worldwide_day).into());
    }
    let supply_promis = u128::try_from(contract.pending_supply_promis.read(&worldwide_day)?)
        .map_err(|_| DesisError::InvalidWorldwideDay(worldwide_day))?;
    let supply_intex = (supply_promis / config.promis_load_minor).min(u128::from(u32::MAX)) as u32;

    contract.clearing_initiated.write(&worldwide_day, 1u8)?;
    contract
        .pending_supply_intex
        .write(&worldwide_day, supply_intex)?;
    contract
        .clearing_deadline
        .write(&worldwide_day, now.saturating_add(BIDS_FANIN_TIMEOUT_SECS))?;
    contract.push_gate_active(worldwide_day)?;

    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendAuctionStageClearingCall {
            worldwideDay: worldwide_day,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Bid ingestion
// ---------------------------------------------------------------------------

/// Accept a relayed bid batch. Bids accumulate per source chain while the stage is `Revealing`; a
/// higher `generation` supersedes that chain's prior bids. Batches may arrive in any order over the
/// unordered bridge, so completeness is tracked by a per-(chain, generation) bitmap of `batch_index`;
/// the chain finalizes once its BIDS_DONE marker and every batch have arrived (see
/// `try_finalize_chain`). A redelivered batch (its bit already set) is an idempotent no-op, so the
/// transport may safely re-deliver.
#[allow(clippy::too_many_arguments)]
pub fn process_bids_batch(
    storage: StorageHandle<'_>,
    caller: Address,
    worldwide_day: u32,
    src_chain_id: u32,
    generation: u32,
    batch_index: u16,
    total_batches: u16,
    bids: Vec<BidData>,
) -> Result<()> {
    require_origin_router(caller)?;
    require_nonzero_worldwide_day(worldwide_day)?;
    // The arrival bitmap is a U256, so at most 256 batches (batch_index 0..=255) are trackable.
    if total_batches == 0 || total_batches > 256 || batch_index >= total_batches {
        return Err(PrecompileError::Revert(
            "processBidsBatch: invalid batch index/total".into(),
        ));
    }
    let mut contract = storage.contract::<DesisContract>();

    // Intake is open only while Revealing. Past intake a late/re-flushed batch is redundant → no-op (else the
    // transport redelivers forever); before intake it's premature → revert so it redelivers after reveal.
    let stage = contract.read_stage(worldwide_day)?;
    if stage != AuctionStage::Revealing {
        return match stage {
            AuctionStage::BidsReceived | AuctionStage::Cleared | AuctionStage::Cancelled => Ok(()),
            _ => Err(DesisError::InvalidStageTransition.into()),
        };
    }

    let chain_key = DesisContract::chain_key(worldwide_day, src_chain_id);
    let last_gen = contract.chain_last_generation.read(&chain_key)?;
    if generation < last_gen {
        return Err(DesisError::StaleBidsGeneration {
            incoming: generation,
            last: last_gen,
        }
        .into());
    }

    if generation > last_gen {
        // New generation supersedes: drop the chain's bids and reset its completeness tracking
        // (including a stale marker and the done flag).
        contract.reset_chain_intake(worldwide_day, src_chain_id)?;
        contract
            .chain_last_generation
            .write(&chain_key, generation)?;
        contract
            .chain_total_batches
            .write(&chain_key, u32::from(total_batches))?;
    }

    // All batches of a generation must agree on total_batches and stay in range, else a bad peer could set an
    // out-of-range bit and false-complete the set with a real batch missing.
    let stored_total = contract.chain_total_batches.read(&chain_key)?;
    if u32::from(total_batches) != stored_total || u32::from(batch_index) >= stored_total {
        return Err(PrecompileError::Revert(
            "processBidsBatch: batch total/index mismatch for generation".into(),
        ));
    }

    let bit = U256::from(1u8) << (batch_index as usize);
    let mask = contract.chain_arrived_mask.read(&chain_key)?;
    if !(mask & bit).is_zero() {
        // This batch of the current generation was already applied; redelivery is idempotent.
        return Ok(());
    }

    for bid in &bids {
        contract.append_bid(worldwide_day, src_chain_id, bid)?;
    }
    contract.chain_arrived_mask.write(&chain_key, mask | bit)?;

    try_finalize_chain(&mut contract, worldwide_day, src_chain_id)
}

/// Accept a chain's BIDS_DONE completeness marker: the source relayed `total_batches` batches with
/// `total_bids` bids for this day/generation. Stage/generation semantics mirror `process_bids_batch`;
/// a marker whose generation is ahead of the chain's batches reverts so the transport redelivers it
/// once the batches have arrived.
pub fn process_bids_done(
    storage: StorageHandle<'_>,
    caller: Address,
    worldwide_day: u32,
    src_chain_id: u32,
    relay_generation: u32,
    total_batches: u16,
    total_bids: u32,
) -> Result<()> {
    require_origin_router(caller)?;
    require_nonzero_worldwide_day(worldwide_day)?;
    if total_batches == 0 || total_batches > 256 {
        return Err(PrecompileError::Revert(
            "processBidsDone: invalid total batches".into(),
        ));
    }
    let mut contract = storage.contract::<DesisContract>();

    let stage = contract.read_stage(worldwide_day)?;
    if stage != AuctionStage::Revealing {
        return match stage {
            AuctionStage::BidsReceived | AuctionStage::Cleared | AuctionStage::Cancelled => Ok(()),
            _ => Err(DesisError::InvalidStageTransition.into()),
        };
    }

    let chain_key = DesisContract::chain_key(worldwide_day, src_chain_id);
    let last_gen = contract.chain_last_generation.read(&chain_key)?;
    if relay_generation < last_gen {
        return Err(DesisError::StaleBidsGeneration {
            incoming: relay_generation,
            last: last_gen,
        }
        .into());
    }
    if relay_generation > last_gen {
        return Err(PrecompileError::Revert(
            "processBidsDone: marker generation ahead of its batches".into(),
        ));
    }

    contract
        .chain_done_batches
        .write(&chain_key, u32::from(total_batches))?;
    contract.chain_done_bids.write(&chain_key, total_bids)?;

    try_finalize_chain(&mut contract, worldwide_day, src_chain_id)
}

/// Mark the chain done once its BIDS_DONE marker and every batch have arrived with matching totals.
/// Invoked from both arrival paths — either side may land last over the unordered bridge. An
/// integrity mismatch (batch totals vs marker claims) keeps the chain not-done, so the deadline
/// skip excludes it.
fn try_finalize_chain(
    contract: &mut DesisContract<'_>,
    worldwide_day: u32,
    chain_id: u32,
) -> Result<()> {
    let key = DesisContract::chain_key(worldwide_day, chain_id);
    if contract.chain_done.read(&key)? != 0 {
        return Ok(());
    }
    let claimed_batches = contract.chain_done_batches.read(&key)?;
    if claimed_batches == 0 {
        return Ok(()); // no marker yet
    }
    let total = contract.chain_total_batches.read(&key)?;
    let mask = contract.chain_arrived_mask.read(&key)?;
    let bid_count = contract.chain_bid_count.read(&key)?;
    if total != claimed_batches
        || mask.count_ones() as u32 != total
        || bid_count != contract.chain_done_bids.read(&key)?
    {
        return Ok(());
    }
    contract.chain_done.write(&key, 1u8)?;
    contract.emit(IDesis::ChainBidsDone {
        worldwideDay: worldwide_day,
        srcChainId: chain_id,
        bidsCount: bid_count,
    })
}

// ---------------------------------------------------------------------------
// Clearing
// ---------------------------------------------------------------------------

/// Tick entry for the fan-in gate: clear once every snapshot chain has finalized,
/// or once the deadline passes (missing chains are excluded and reported via
/// `ChainSkipped`). Returns `None` while the gate is not ready.
pub fn force_clear(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    now: u64,
) -> Result<Option<ClearingResult>> {
    let snapshot = fetch_targets(&storage, worldwide_day)?;
    let (included, skipped) = {
        let contract = storage.contract::<DesisContract>();
        let parts = partition_chains(&contract, worldwide_day, &snapshot)?;
        if !parts.1.is_empty() && now < contract.clearing_deadline.read(&worldwide_day)? {
            return Ok(None);
        }
        parts
    };
    clear_inner(storage, worldwide_day, &snapshot, &included, &skipped).map(Some)
}

/// Begin-block tick: attempt to clear every day awaiting the fan-in gate. Each
/// day runs in its own checkpoint — an Err rolls that day back (retried next
/// block) and never escapes into the block hook chain.
pub fn tick_gate(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let count = {
        let contract = storage.contract::<DesisContract>();
        contract.gate_active_count.read()?
    };
    if count == 0 {
        return Ok(());
    }
    let now = ctx.block.timestamp;
    // Snapshot the set before iterating: a successful clear swap-pops it.
    let mut days = Vec::with_capacity(count as usize);
    {
        let contract = storage.contract::<DesisContract>();
        for i in 0..count {
            days.push(contract.gate_active_at.read(&i)?);
        }
    }
    for day in days {
        let res = storage.with_checkpoint(|| force_clear(storage.clone(), day, now));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::desis", day, error = ?e, "clearing gate: skipping day");
        }
    }
    Ok(())
}

/// The day's frozen target snapshot, read from the OriginRouter registry
/// (deterministic: frozen at STAGE_START).
fn fetch_targets(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<Vec<u32>> {
    let ret = storage.staticcall(
        ORIGIN_ROUTER_ADDRESS,
        IOriginRouter::targetsOfCall {
            worldwideDay: worldwide_day,
        }
        .abi_encode()
        .into(),
    )?;
    IOriginRouter::targetsOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("targetsOf undecodable".into()))
}

/// Split the snapshot into chains whose intake finalized and chains still missing.
fn partition_chains(
    contract: &DesisContract<'_>,
    worldwide_day: u32,
    snapshot: &[u32],
) -> Result<(Vec<u32>, Vec<u32>)> {
    let mut included = Vec::with_capacity(snapshot.len());
    let mut skipped = Vec::new();
    for &chain_id in snapshot {
        if contract
            .chain_done
            .read(&DesisContract::chain_key(worldwide_day, chain_id))?
            != 0
        {
            included.push(chain_id);
        } else {
            skipped.push(chain_id);
        }
    }
    Ok((included, skipped))
}

/// Run the clearing algorithm over the included chains' bids, transition to
/// `Cleared`, hand issuance to IntexFactory, return unused supply to PromisLimit
/// and send the per-chain AUCTION_RESULT / REFUND_INSTRUCTIONS messages.
fn clear_inner(
    storage: StorageHandle<'_>,
    worldwide_day: u32,
    snapshot: &[u32],
    included: &[u32],
    skipped: &[u32],
) -> Result<ClearingResult> {
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, worldwide_day, AuctionStage::Revealing)?;

    let supply = contract.pending_supply_intex.read(&worldwide_day)?;
    if contract.clearing_initiated.read(&worldwide_day)? == 0 {
        return Err(DesisError::PendingClearingDataMissing(worldwide_day).into());
    }

    let config = contract.read_auction_config(worldwide_day)?;
    let min_bid_qty = contract.config_min_bid_quantity.read(&worldwide_day)? as u16;
    // Zero bids are valid here: `calculate_clearing` yields 0 issued, the full supply returns
    // to PromisLimit, and a no-sale AuctionResult(0,0,0) is reported to every snapshot chain.
    let bids = contract.read_chains_bids(worldwide_day, included)?;

    let total_demand: u64 = bids.iter().map(|(_, b)| u64::from(b.intex_quantity)).sum();
    let mut sorted = bids;
    sort_bids(&mut sorted);

    let result = calculate_clearing(&sorted, &config, supply, min_bid_qty);

    // Persist clearing outcome and transition.
    contract.write_stage(worldwide_day, AuctionStage::Cleared)?;
    contract.write_last_cleared_worldwide_day(worldwide_day)?;
    contract.write_last_clearing_issued_count(result.issued_intex_count)?;

    // Clear the bid working-set, pending inputs and the gate (CEI: state writes before external calls).
    for &chain_id in snapshot {
        contract.reset_chain_intake(worldwide_day, chain_id)?;
    }
    contract.day_bid_count.write(&worldwide_day, 0)?;
    contract.pending_supply_intex.write(&worldwide_day, 0)?;
    contract.clearing_initiated.write(&worldwide_day, 0u8)?;
    contract.clearing_deadline.clear(&worldwide_day)?;
    contract.remove_gate_active(worldwide_day)?;

    for &chain_id in skipped {
        contract.emit(IDesis::ChainSkipped {
            worldwideDay: worldwide_day,
            srcChainId: chain_id,
        })?;
    }

    if result.issued_intex_count == 0 {
        contract.emit(IDesis::AuctionClearedEmpty {
            worldwideDay: worldwide_day,
            totalDemand: total_demand,
        })?;
    } else {
        contract.emit(IDesis::AuctionCleared {
            worldwideDay: worldwide_day,
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
            worldwideDay: worldwide_day,
            unusedPromis: unused_promis,
        })?;
        PromisLimitContract::new(storage.clone()).add_to_total_unallocated(unused_promis)?;
    }

    // Hand issuance to IntexFactory. A zero-winner clearing creates no series;
    // issue() then only discards the day's never-to-distribute creator rewards.
    let params = outbe_intexfactory::schema::IssuanceParams {
        series_id: derive_series_id(worldwide_day),
        worldwide_day,
        issued_intex_count: result.issued_intex_count,
        promis_load_minor: config.promis_load_minor,
        entry_price_minor: config.entry_price_minor,
        issuance_currency: QUALIFIER_ISSUANCE_ISO,
        reference_currency: QUALIFIER_REFERENCE_ISO,
        recipients: result.winners.clone(),
        quantities: result.winner_quantities.clone(),
        recipient_chains: result.winner_chains.clone(),
        snapshot_chains: snapshot.to_vec(),
    };
    outbe_intexfactory::api::issue(&storage, params)?;

    // Send AUCTION_RESULT to every snapshot chain; skipped/zero-winner chains get
    // wonBidsCount 0 so their local auction still completes.
    for &chain_id in snapshot {
        let won_bids_count = result
            .winner_chains
            .iter()
            .filter(|&&c| c == chain_id)
            .count() as u32;
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendAuctionResultCall {
                dstChainId: chain_id,
                worldwideDay: worldwide_day,
                issuedIntexCount: result.issued_intex_count,
                auctionClearingRate: u64::from(result.clearing_rate),
                wonBidsCount: won_bids_count,
            }
            .abi_encode()
            .into(),
        )?;
    }

    // Send REFUND_INSTRUCTIONS per included chain with bidders (winners and losers alike);
    // a skipped chain's bidders reclaim on their own chain via the escrow timeout path.
    for &chain_id in included {
        let mut bidders = Vec::new();
        let mut refunded = Vec::new();
        let mut paid = Vec::new();
        for (i, &bidder_chain) in result.bidder_chains.iter().enumerate() {
            if bidder_chain == chain_id {
                bidders.push(result.all_bidders[i]);
                refunded.push(result.refunded_amounts[i]);
                paid.push(result.paid_amounts[i]);
            }
        }
        if bidders.is_empty() {
            continue;
        }
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendRefundInstructionsCall {
                dstChainId: chain_id,
                worldwideDay: worldwide_day,
                bidders,
                refundedAmounts: refunded,
                paidAmounts: paid,
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

/// Sort chain-tagged bids: descending rate, ascending timestamp on tie. The sort is
/// stable, so remaining ties keep the snapshot's chain order — deterministic.
fn sort_bids(bids: &mut [(u32, BidData)]) {
    bids.sort_by(|(_, a), (_, b)| {
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
    bids: &[(u32, BidData)],
    config: &AuctionConfig,
    supply: u32,
    min_qty: u16,
) -> ClearingResult {
    let len = bids.len();
    let mut winners: Vec<Address> = Vec::with_capacity(len);
    let mut winner_quantities: Vec<alloy_primitives::U256> = Vec::with_capacity(len);
    let mut winner_chains: Vec<u32> = Vec::with_capacity(len);
    let mut won_by_index: Vec<u32> = vec![0u32; len];

    let escrow_basis = config.escrow_basis_minor();
    let mut total_allocated: u32 = 0;
    let mut clearing_rate: u32 = config.min_intex_bid_rate;

    for (i, (chain_id, bid)) in bids.iter().enumerate() {
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
            winner_chains.push(*chain_id);
            won_by_index[i] = allocated;
            total_allocated += allocated;
            clearing_rate = bid.intex_bid_rate;
        }
    }

    let mut all_bidders: Vec<Address> = Vec::with_capacity(len);
    let mut refunded_amounts: Vec<u128> = Vec::with_capacity(len);
    let mut paid_amounts: Vec<u128> = Vec::with_capacity(len);
    let mut bidder_chains: Vec<u32> = Vec::with_capacity(len);

    for (i, (chain_id, bid)) in bids.iter().enumerate() {
        all_bidders.push(bid.bidder_address);
        bidder_chains.push(*chain_id);

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
        winner_chains,
        all_bidders,
        refunded_amounts,
        paid_amounts,
        bidder_chains,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The single point where a series id is derived from the day; identity until multi-currency (then 1 -> N).
fn derive_series_id(worldwide_day: u32) -> u32 {
    worldwide_day
}

fn require_origin_router(caller: Address) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(DesisError::UnauthorizedOrigin(caller).into());
    }
    Ok(())
}

fn require_nonzero_worldwide_day(worldwide_day: u32) -> Result<()> {
    if worldwide_day == 0 {
        return Err(DesisError::InvalidWorldwideDay(0).into());
    }
    Ok(())
}

fn require_stage(
    contract: &DesisContract<'_>,
    worldwide_day: u32,
    expected: AuctionStage,
) -> Result<()> {
    let actual = contract.read_stage(worldwide_day)?;
    if actual != expected {
        return Err(DesisError::InvalidStageTransition.into());
    }
    Ok(())
}
