use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_nod::NodRepositoryReader;
use outbe_primitives::{
    block::BlockRuntimeContext,
    chain,
    error::Result,
    time::{
        date_key_to_utc_timestamp as primitives_date_key_to_timestamp,
        previous_date_key as primitives_previous_date_key, timestamp_to_date_key as utc_date_key,
    },
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::{TributeContract, TributeRepositoryReader};

use crate::constants::*;
use crate::errors::MetadosisError;
use crate::precompile::IMetadosis;
use crate::schema::{day_type, status, MetadosisContract, WorldwideDayEntryExt};

pub struct MetadosisCalculation {
    pub action: &'static str,
    pub gratis_demand: U256,
    pub gratis_supply: U256,
    pub gratis_allocation: U256,
    pub metadosis_limit_remainder: U256,
}

impl MetadosisContract<'_> {
    /// Core metadosis calculation for a worldwide day.
    pub fn calculate_metadosis(
        &self,
        wwd: WorldwideDay,
        tribute_nominal_total: U256,
        wwd_metadosis_limit: U256,
    ) -> Result<MetadosisCalculation> {
        let wwd_type = self.get_wwd_day_type(wwd)?;

        let mut action = "lysis green day";
        let mut demand = tribute_nominal_total * U256::from(SYMBOLIC_RATE) / U256::from(100u64);
        let mut supply = wwd_metadosis_limit;

        match wwd_type {
            day_type::GREEN => {}
            day_type::RED => {
                action = "lysis red day";
                demand /= U256::from(RED_DAY_REDUCTION_COEF);
                supply /= U256::from(RED_DAY_REDUCTION_COEF);
            }
            _ => return Err(MetadosisError::UnknownWorldwideDayType.into()),
        }

        let allocation = if demand < supply { demand } else { supply };
        let remainder = wwd_metadosis_limit - allocation;

        Ok(MetadosisCalculation {
            action,
            gratis_demand: demand,
            gratis_supply: supply,
            gratis_allocation: allocation,
            metadosis_limit_remainder: remainder,
        })
    }

    /// Returns effective lookback and offering hours based on chain identity.
    pub fn effective_hours(&self, chain_id: u64) -> Result<(u64, u64)> {
        if chain::is_devnet(chain_id) || chain::is_testnet(chain_id) {
            Ok((
                BOOTSTRAP_LOOKBACK_DELAY_HOURS,
                BOOTSTRAP_OFFERING_PERIOD_HOURS,
            ))
        } else {
            Ok((LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS))
        }
    }
}

/// Converts a unix timestamp to a yyyymmdd date key (UTC).
pub fn timestamp_to_date_key(timestamp: u64) -> u32 {
    utc_date_key(timestamp)
}

/// Returns the unix timestamp for midnight UTC of a yyyymmdd date key.
///
/// Re-export of [`outbe_primitives::time::date_key_to_utc_timestamp`] for
/// backward compatibility with existing call sites in this crate. New
/// code should depend on `outbe_primitives::time` directly.
pub fn date_key_to_timestamp(date_key: u32) -> u64 {
    primitives_date_key_to_timestamp(date_key)
}

/// Returns the previous calendar day key for a yyyymmdd date key.
///
/// Re-export of [`outbe_primitives::time::previous_date_key`].
pub fn previous_date_key(date_key: u32) -> u32 {
    primitives_previous_date_key(date_key)
}

/// Public entry point invoked by the daily Cycle handler
/// (`outbe_cycle::handler::run_emission_limit_daily`) AFTER the
/// terminal Metadosis credit has been written to `day_metadosis_limit`
/// for the previous UTC day. Runs the full WWD lifecycle:
/// bootstrap (block 1 only), `create_worldwide_day_if_needed`,
/// `update_wwd_status_machine` for active WWDs, `process_metadosis`
/// for any READY WWD, `PromisLimit.add_to_total_unallocated`, and
/// `cleanup_completed_days`.
///
/// Renamed from `run_begin_block` (Phase 5.1 of the
/// Cycle epic): the function used to be wired into a dedicated
/// `MetadosisLifecycle::begin_block` lifecycle hook running on every
/// block; with the Cycle epic the only legitimate caller is the
/// Cycle handler at UTC midnight. The `MetadosisLifecycle` wrapper
/// was deleted altogether in the follow-up cleanup; tests that drive
/// the WWD state machine sub-day call this function directly.
pub fn start_metadosis(
    ctx: &BlockRuntimeContext,
    tribute_bodies: &TributeRepositoryReader,
    nod_bodies: &NodRepositoryReader,
) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;

    if ctx.block.block_number == 1 {
        init_genesis_day_inner(&mut metadosis, ctx)?;
    }

    create_worldwide_day_if_needed(&mut metadosis, ctx, timestamp)?;

    advance_active_worldwide_days(ctx)?;

    let active = metadosis.active_wwd.read_all()?;

    for wwd in &active {
        if metadosis.get_wwd_status(*wwd)? == status::READY {
            process_metadosis(&mut metadosis, ctx, tribute_bodies, nod_bodies, *wwd)?;
            break;
        }
    }

    // Terminal-day cleanup is no longer a per-tick scan: each COMPLETED/FAILED
    // transition retires the day into the bounded `closed_wwd`
    // delete-queue (see `MetadosisContract::mark_wwd_*`), which evicts and
    // deletes the oldest record past `MAX_RECORDS_KEPT`.

    Ok(())
}

/// Walk every active WorldwideDay's status machine forward to the phase
/// dictated by `ctx.block.timestamp`. Pure window/status logic plus the
/// transition side effects owned by [`update_wwd_status_machine`] (day-rate
/// resolve, tribute seal/unseal, best-effort auction dispatch) — no day
/// creation and no READY settlement; those stay midnight-owned in
/// [`start_metadosis`].
///
/// Two callers:
/// * [`start_metadosis`] — the 00:00 UTC Cycle trigger, before settlement,
///   so a day crossing WAITING→READY at midnight settles in the same tick;
/// * the `wwd_advance_noon` Cycle trigger (12:00 UTC,
///   `outbe_cycle::triggers`) — the forming/offering window edges land at
///   12:00 UTC (`forming_end = forming_start + 50h` with `forming_start` at
///   10:00 UTC of the previous day), so with only the midnight tick every
///   12:00 transition was applied ~12h late and `offerTribute` reverted
///   `not in OFFERING status` for the whole gap.
pub fn advance_active_worldwide_days(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;
    for wwd in metadosis.active_wwd.read_all()? {
        update_wwd_status_machine(&mut metadosis, ctx, wwd, timestamp)?;
    }
    Ok(())
}

/// Genesis-block (block 1) metadosis bootstrap: engage the testnet/devnet
/// bootstrap window and create the first worldwide day. Idempotent.
///
/// Wired into the begin-zone CycleTick phase at block 1 via
/// `outbe_cycle::lifecycle::CycleLifecycle::begin_block`. This is required
/// because the daily Cycle trigger only *anchors* `last_executed_at` on its
/// first encounter (block 1) and therefore never invokes [`start_metadosis`]
/// there; without this entry point the first worldwide day would not exist
/// until the first block after the next UTC midnight.
pub fn init_genesis_day(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    init_genesis_day_inner(&mut metadosis, ctx)
}

/// Shared genesis-init body so the production block-1 path
/// ([`init_genesis_day`]) and the test/direct path inside [`start_metadosis`]
/// stay byte-for-byte identical.
fn init_genesis_day_inner(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
) -> Result<()> {
    initialize_bootstrap_if_needed(metadosis, ctx)?;
    create_initial_worldwide_day_if_needed(metadosis, ctx, ctx.block.timestamp)
}

fn initialize_bootstrap_if_needed(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
) -> Result<()> {
    if (chain::is_devnet(ctx.block.chain_id) || chain::is_testnet(ctx.block.chain_id))
        && metadosis.get_bootstrap_end_time()? == 0
    {
        let end_time = ctx.block.timestamp + BOOTSTRAP_DURATION_HOURS * SECONDS_PER_HOUR;
        metadosis.set_bootstrap_end_time(end_time)?;
    }
    Ok(())
}

fn create_initial_worldwide_day_if_needed(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    if !metadosis.active_wwd.is_empty()? {
        return Ok(());
    }

    let utc_day = timestamp_to_date_key(timestamp);
    create_worldwide_day_for_date(metadosis, ctx, utc_day.into())
}

pub fn create_worldwide_day_if_needed(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    let wwd = WorldwideDay::from_timestamp(timestamp);
    create_worldwide_day_for_date(metadosis, ctx, wwd)
}

pub fn create_worldwide_day_for_date(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> Result<()> {
    let existing_forming_start = metadosis.worldwide_days.entry(wwd).forming_start().read()?;
    if existing_forming_start != 0 {
        return Ok(());
    }

    let (lookback_hours, offering_hours) = metadosis.effective_hours(ctx.block.chain_id)?;
    let forming_start = wwd.start_timestamp();
    metadosis.create_worldwide_day(wwd, forming_start, lookback_hours, offering_hours)?;
    metadosis.add_active_wwd(wwd)?;

    let mut tribute = TributeContract::new(metadosis.storage.clone());
    tribute.seal_day(wwd)?;

    let forming_end = metadosis.worldwide_days.entry(wwd).forming_end().read()?;
    let lookback_end = metadosis.worldwide_days.entry(wwd).lookback_end().read()?;
    let offering_end = metadosis.worldwide_days.entry(wwd).offering_end().read()?;
    let scheduled = metadosis
        .worldwide_days
        .entry(wwd)
        .scheduled_process_time()
        .read()?;
    metadosis.emit(IMetadosis::WorldwideDayStarted {
        worldwideDay: wwd.into(),
        formingStart: forming_start,
        formingEnd: forming_end,
        offeringStart: lookback_end,
        offeringEnd: offering_end,
        scheduledTime: scheduled,
    })?;

    Ok(())
}

/// Auction timestamp (scheduled process time) for a worldwide day.
fn scheduled_auction_ts(metadosis: &MetadosisContract, wwd: WorldwideDay) -> Result<u64> {
    metadosis
        .worldwide_days
        .entry(wwd)
        .scheduled_process_time()
        .read()
}

fn update_wwd_status_machine(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
    timestamp: u64,
) -> Result<()> {
    let current_status = metadosis.get_wwd_status(wwd)?;

    match current_status {
        status::IN_PROGRESS | status::COMPLETED | status::FAILED | status::READY => {
            return Ok(());
        }
        _ => {}
    }

    let new_status = metadosis.update_wwd_status(wwd, timestamp)?;

    if current_status == status::OFFERING && new_status == status::OFFERING {
        let auction_ts = scheduled_auction_ts(metadosis, wwd)?;
        let is_green_day = metadosis.get_wwd_day_type(wwd)? == day_type::GREEN;
        outbe_desis::api::dispatch_stage_reveal(ctx.storage.clone(), auction_ts, is_green_day)?;
        return Ok(());
    }

    if new_status == current_status {
        return Ok(());
    }

    let mut tribute = TributeContract::new(metadosis.storage.clone());

    if current_status == status::FORMING && new_status != status::FORMING {
        store_worldwide_day_vwap_snapshot(metadosis, wwd)?;
        resolve_day_rate(metadosis, wwd)?;
    }

    if current_status < status::OFFERING && new_status == status::OFFERING {
        tribute.unseal_day(wwd)?;
        let auction_ts = scheduled_auction_ts(metadosis, wwd)?;
        let coen_price = metadosis.worldwide_days.entry(wwd).current_vwap().read()?;
        outbe_desis::api::dispatch_stage_start(ctx.storage.clone(), auction_ts, coen_price)?;
    }
    if current_status == status::OFFERING {
        tribute.seal_day(wwd)?;
    }

    metadosis.emit(IMetadosis::WorldwideDayStatusChange {
        worldwideDay: wwd.into(),
        oldStatus: current_status,
        newStatus: new_status,
        blockNumber: ctx.block.block_number,
    })?;

    Ok(())
}

/// Snapshot the day's forming-window VWAPs into the Oracle. A window with no
/// oracle data is a deterministic no-op (the Oracle reports `false`); the day
/// then resolves to RED via [`resolve_day_rate`] reading `None`.
fn store_worldwide_day_vwap_snapshot(
    metadosis: &mut MetadosisContract,
    wwd: WorldwideDay,
) -> Result<()> {
    let forming_start = metadosis.worldwide_days.entry(wwd).forming_start().read()?;
    let forming_end = metadosis.worldwide_days.entry(wwd).forming_end().read()?;
    outbe_oracle::api::store_worldwide_day_vwap_snapshot(
        metadosis.storage.clone(),
        wwd,
        forming_start,
        forming_end,
    )?;
    Ok(())
}

/// Resolve and persist the day's current/previous VWAP and GREEN/RED type from
/// the Oracle's `COEN/0xUSD` snapshots. Missing data (`None`) reads as zero; the
/// zero-VWAP⇒RED rule lives solely in [`determine_day_type`]. Genuine Oracle
/// faults propagate (no silent zero-fallback).
fn resolve_day_rate(metadosis: &mut MetadosisContract, wwd: WorldwideDay) -> Result<()> {
    let current_vwap = outbe_oracle::api::day_type_pair_vwap(metadosis.storage.clone(), wwd)?
        .unwrap_or(U256::ZERO);

    // When today has no VWAP the day is already RED regardless of yesterday, so
    // skip the second Oracle read.
    let previous_vwap = if current_vwap.is_zero() {
        U256::ZERO
    } else {
        outbe_oracle::api::day_type_pair_vwap(metadosis.storage.clone(), wwd.previous_date_key())?
            .unwrap_or(U256::ZERO)
    };

    metadosis
        .worldwide_days
        .entry(wwd)
        .previous_vwap()
        .write(previous_vwap)?;
    metadosis
        .worldwide_days
        .entry(wwd)
        .current_vwap()
        .write(current_vwap)?;
    metadosis.set_wwd_day_type(wwd, determine_day_type(previous_vwap, current_vwap))?;

    Ok(())
}

/// Settle a READY worldwide day. The five terminal outcomes are intentionally
/// **not** uniform — each has a distinct PROMIS interaction, so do not collapse
/// them into one shared "settle" helper. PromisLimit is a cross-day accumulator;
/// `set` replaces the whole pool, `add` contributes to it.
///
/// | branch        | auction clearing        | PromisLimit                  | mark      |
/// |---------------|-------------------------|------------------------------|-----------|
/// | limit == 0    | clear(0) to close it     | —                            | FAILED    |
/// | day = UNKNOWN | none (not GREEN)         | `add(limit)` so it isn't lost| FAILED    |
/// | no tributes   | clear(limit), close      | `add(remainder)`             | COMPLETED |
/// | lysis Ok      | add day remainder, then  | `set(clearing remainder)` —  | COMPLETED |
/// |               | clear the **whole** pool | the pool minus what the      |           |
/// |               |                          | auction consumed             |           |
/// | lysis Err     | none                     | none (propagates, reverts)   | —         |
///
/// `dispatch_auction_clearing` returns the PROMIS the auction could not consume
/// (rounding dust on success, whole supply on best-effort failure); that value —
/// not zero — is what the success branch writes back via `set`. Do not re-add the
/// dust inside Desis; that double-counts and the `set` here would wipe it.
fn process_metadosis(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    tribute_bodies: &TributeRepositoryReader,
    nod_bodies: &NodRepositoryReader,
    wwd: WorldwideDay,
) -> Result<()> {
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());

    let limit_amount = metadosis
        .worldwide_days
        .entry(wwd)
        .metadosis_limit_amount()
        .read()?;

    let wwd_type = metadosis.get_wwd_day_type(wwd)?;

    if limit_amount.is_zero() {
        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()?;
        dispatch_auction_clearing(ctx, wwd_type, auction_ts, U256::ZERO)?;
        metadosis.mark_wwd_failed(wwd)?;
        metadosis.emit(IMetadosis::MetadosisSkipped {
            worldwideDay: wwd.into(),
            reason: "day_metadosis_limit_is_zero".into(),
            status: "SKIPPED".into(),
            blockNumber: ctx.block.block_number,
        })?;
        return Ok(());
    }

    if metadosis.get_wwd_day_type(wwd)? == day_type::UNKNOWN {
        metadosis.mark_wwd_failed(wwd)?;
        emit_failed_execution(metadosis, ctx, wwd, U256::ZERO, limit_amount)?;
        promis_limit.add_to_total_unallocated(limit_amount)?;

        return Ok(());
    }

    let tribute = TributeContract::new(metadosis.storage.clone());
    let tribute_day_totals = tribute.get_day_totals(wwd)?;

    if tribute_day_totals.tribute_count == 0 {
        // No tributes, but the day still opened a GREEN auction at
        // FORMING->OFFERING (see `update_wwd_status_machine`). Close it so no
        // started auction is left dangling on a terminal day; whatever the
        // clearing does not deliver is routed to PROMIS as unallocated.
        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()?;
        let to_promis = dispatch_auction_clearing(ctx, wwd_type, auction_ts, limit_amount)?;
        metadosis.mark_wwd_completed(wwd)?;
        metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
            worldwideDay: wwd.into(),
            dayMetadosisLimit: limit_amount,
            dayMetadosisLimitRemainder: to_promis,
            status: "COMPLETED".into(),
            dayState: wwd_state_label(wwd_type).into(),
            action: "no tributes".into(),
        })?;
        promis_limit.add_to_total_unallocated(to_promis)?;
        return Ok(());
    }

    let tribute_nominal_total = tribute_day_totals.tribute_nominal_amount;
    let metadosis_parameters =
        metadosis.calculate_metadosis(wwd, tribute_nominal_total, limit_amount)?;

    // Same timestamp the auction stages dispatch with: lysis keys the
    // contributor map by its date key, i.e. the auction series id.
    let auction_ts = metadosis
        .worldwide_days
        .entry(wwd)
        .scheduled_process_time()
        .read()?;

    match outbe_lysis::runtime::lysis(
        metadosis.storage.clone(),
        tribute_bodies,
        nod_bodies,
        wwd,
        auction_ts,
        metadosis_parameters.gratis_allocation,
    ) {
        Ok(lysis_result) => {
            let remainder =
                lysis_result.remaining_gratis + metadosis_parameters.metadosis_limit_remainder;

            promis_limit.add_to_total_unallocated(remainder)?;

            let promis_total_unallocated = promis_limit.get_total_unallocated()?;

            let auction_ts = metadosis
                .worldwide_days
                .entry(wwd)
                .scheduled_process_time()
                .read()?;

            let clearing_reminder =
                dispatch_auction_clearing(ctx, wwd_type, auction_ts, promis_total_unallocated)?;

            //TODO: ADD GUARD
            // if (clearing_reminder > promis_total_unallocated) {
            //     //drop error
            // }

            promis_limit.set_total_unallocated(clearing_reminder)?;

            metadosis.mark_wwd_completed(wwd)?;

            metadosis.emit(IMetadosis::MetadosisExecuted {
                worldwideDay: wwd.into(),
                tributeTotals: tribute_nominal_total,
                dayGratisDemand: metadosis_parameters.gratis_demand,
                dayGratisLimit: metadosis_parameters.gratis_supply,
                dayGratisAllocation: metadosis_parameters.gratis_allocation,
                dayGratisAllocationRemainder: lysis_result.remaining_gratis,
                netDayGratisAllocation: metadosis_parameters
                    .gratis_allocation
                    .saturating_sub(lysis_result.remaining_gratis),
                dayMetadosisLimitRemainder: remainder,
                status: "COMPLETED".into(),
                blockNumber: ctx.block.block_number,
            })?;

            Ok(())
        }
        // A lysis failure here is a genuine corruption (e.g. NOD-id collision),
        // not a normal terminal outcome: the day already passed FORMING/OFFERING,
        // so the failure means execution state is inconsistent. Record the FAILED
        // transition for observability, then propagate the original error so the
        // begin-zone system transaction reverts with the real reason. (On the
        // production path the revert rolls back this FAILED write; it is only
        // observable in tests, which do not revert.)
        Err(err) => {
            metadosis.mark_wwd_failed(wwd)?;
            emit_failed_execution(metadosis, ctx, wwd, tribute_nominal_total, limit_amount)?;
            Err(err)
        }
    }
}

fn dispatch_auction_clearing(
    ctx: &BlockRuntimeContext,
    dtype: u8,
    auction_ts: u64,
    supply: U256,
) -> Result<U256> {
    if dtype != day_type::GREEN {
        return Ok(supply);
    }
    // Returns the PROMIS remainder the auction could not consume: the rounding
    // remainder on a delivered clearing, or the whole `supply` on a best-effort
    // Desis failure. The caller writes this back into the PromisLimit accumulator.
    outbe_desis::api::dispatch_stage_clearing(ctx.storage.clone(), auction_ts, supply)
}

fn emit_failed_execution(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
    tribute_totals: U256,
    day_metadosis_limit_remainder: U256,
) -> Result<()> {
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: wwd.into(),
        tributeTotals: tribute_totals,
        dayGratisDemand: U256::ZERO,
        dayGratisLimit: U256::ZERO,
        dayGratisAllocation: U256::ZERO,
        dayGratisAllocationRemainder: U256::ZERO,
        netDayGratisAllocation: U256::ZERO,
        dayMetadosisLimitRemainder: day_metadosis_limit_remainder,
        status: "FAILED".into(),
        blockNumber: ctx.block.block_number,
    })
}

fn determine_day_type(previous_vwap: U256, current_vwap: U256) -> u8 {
    if previous_vwap.is_zero() || current_vwap.is_zero() {
        return day_type::RED;
    }

    if current_vwap > previous_vwap {
        day_type::GREEN
    } else {
        day_type::RED
    }
}

fn wwd_state_label(dtype: u8) -> &'static str {
    match dtype {
        day_type::GREEN => "GREEN",
        day_type::RED => "RED",
        _ => "UNKNOWN",
    }
}
