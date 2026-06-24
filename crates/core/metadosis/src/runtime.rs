use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    block::BlockRuntimeContext,
    chain,
    error::Result,
    storage::StorageHandle,
    time::{
        date_key_to_utc_timestamp as primitives_date_key_to_timestamp,
        previous_date_key as primitives_previous_date_key, timestamp_to_date_key as utc_date_key,
    },
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::constants::*;
use crate::errors::MetadosisError;
use crate::precompile::IMetadosis;
use crate::schema::{day_type, status, MetadosisContract, WorldwideDayEntryExt};

pub struct MetadosisCalculation {
    pub action: &'static str,
    pub day_gratis_demand: U256,
    pub day_gratis_limit: U256,
    pub day_gratis_allocation: U256,
    pub day_metadosis_limit_remainder: U256,
}

impl MetadosisContract<'_> {
    /// Core metadosis calculation for a worldwide day.
    pub fn calculate_metadosis(
        &self,
        wwd: WorldwideDay,
        tribute_nominal_total: U256,
        day_metadosis_limit: U256,
    ) -> Result<MetadosisCalculation> {
        let dtype = self.get_day_type(wwd)?;

        let mut action = "lysis green day";
        let mut demand = tribute_nominal_total * U256::from(SYMBOLIC_RATE) / U256::from(100u64);
        let mut limit = day_metadosis_limit;

        match dtype {
            day_type::GREEN => {}
            day_type::RED => {
                action = "lysis red day";
                demand /= U256::from(RED_DAY_REDUCTION_COEF);
                limit /= U256::from(RED_DAY_REDUCTION_COEF);
            }
            _ => return Err(MetadosisError::UnknownWorldwideDayType.into()),
        }

        let allocation = if demand < limit { demand } else { limit };
        let remainder = day_metadosis_limit - allocation;

        Ok(MetadosisCalculation {
            action,
            day_gratis_demand: demand,
            day_gratis_limit: limit,
            day_gratis_allocation: allocation,
            day_metadosis_limit_remainder: remainder,
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
            Ok((DEFAULT_LOOKBACK_DELAY_HOURS, DEFAULT_OFFERING_PERIOD_HOURS))
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
pub fn start_metadosis(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;

    if ctx.block.block_number == 1 {
        init_genesis_day_inner(&mut metadosis, ctx)?;
    }

    create_worldwide_day_if_needed(&mut metadosis, ctx, timestamp)?;

    let active = metadosis.get_all_active_wwds()?;
    for wwd in &active {
        update_wwd_status_machine(&mut metadosis, ctx, *wwd, timestamp)?;
    }

    let mut total_unallocated = U256::ZERO;
    let active = metadosis.get_all_active_wwds()?;
    for wwd in &active {
        if metadosis.get_status(*wwd)? == status::READY {
            let unallocated = process_metadosis(&mut metadosis, ctx, *wwd)?;
            total_unallocated += unallocated;
        }
    }

    if !total_unallocated.is_zero() {
        let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
        promis_limit.add_to_total_unallocated(total_unallocated)?;
    }

    cleanup_completed_days(&mut metadosis, timestamp)?;

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
    if !metadosis.get_all_active_wwds()?.is_empty() {
        return Ok(());
    }

    let utc_day = timestamp_to_date_key(timestamp);
    create_worldwide_day_for_date(metadosis, ctx, utc_day.into())
}

fn create_worldwide_day_if_needed(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    let wwd = WorldwideDay::from_timestamp(timestamp);
    create_worldwide_day_for_date(metadosis, ctx, wwd)
}

fn create_worldwide_day_for_date(
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

fn update_wwd_status_machine(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
    timestamp: u64,
) -> Result<()> {
    let current_status = metadosis.get_status(wwd)?;

    match current_status {
        status::IN_PROGRESS | status::COMPLETED | status::FAILED | status::READY => {
            return Ok(());
        }
        _ => {}
    }

    let new_status = metadosis.update_wwd_status(wwd, timestamp)?;

    if current_status == status::OFFERING && new_status == status::OFFERING {
        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()?;
        let is_green_day = metadosis.get_day_type(wwd)? == day_type::GREEN;
        outbe_desis::api::dispatch_stage_reveal(ctx.storage.clone(), auction_ts, is_green_day)?;
        return Ok(());
    }

    if new_status == current_status {
        return Ok(());
    }

    let mut tribute = TributeContract::new(metadosis.storage.clone());

    if current_status == status::FORMING && new_status != status::FORMING {
        store_worldwide_day_vwap_snapshot(metadosis, wwd)?;
        load_worldwide_day_rates_from_oracle_snapshots(metadosis, wwd)?;
    }

    if current_status < status::OFFERING && new_status == status::OFFERING {
        tribute.unseal_day(wwd)?;
        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()?;
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

fn store_worldwide_day_vwap_snapshot(
    metadosis: &mut MetadosisContract,
    wwd: WorldwideDay,
) -> Result<()> {
    let forming_start = metadosis.worldwide_days.entry(wwd).forming_start().read()?;
    let forming_end = metadosis.worldwide_days.entry(wwd).forming_end().read()?;
    oracle_store_worldwide_day_vwap_snapshot(
        metadosis.storage.clone(),
        wwd,
        forming_start,
        forming_end,
    )
}

fn load_worldwide_day_rates_from_oracle_snapshots(
    metadosis: &mut MetadosisContract,
    wwd: WorldwideDay,
) -> Result<()> {
    let current_vwap = oracle_worldwide_day_vwap_snapshot_value(metadosis.storage.clone(), wwd);
    if current_vwap.is_zero() {
        metadosis
            .worldwide_days
            .entry(wwd)
            .previous_vwap()
            .write(U256::ZERO)?;
        metadosis
            .worldwide_days
            .entry(wwd)
            .current_vwap()
            .write(U256::ZERO)?;
        metadosis.set_day_type(wwd, day_type::RED)?;
        return Ok(());
    }

    let previous_wwd = wwd.previous_date_key();
    let previous_vwap =
        oracle_worldwide_day_vwap_snapshot_value(metadosis.storage.clone(), previous_wwd);

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
    metadosis.set_day_type(wwd, determine_day_type(previous_vwap, current_vwap))?;

    Ok(())
}

fn process_metadosis(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> Result<U256> {
    // AgentReward unallocated state is no longer drained
    // by Metadosis on the skip / fail paths. The Cycle handler owns
    // the daily AgentReward dispatch; once Metadosis can't run, there
    // is nothing useful for it to merge into the day_limit. Returning
    // a zero remainder keeps the per-WWD accounting consistent with
    // the Cycle handler's already-committed allocation.
    if !metadosis.has_day_limit(wwd)? {
        metadosis.mark_failed(wwd)?;
        metadosis.emit(IMetadosis::MetadosisSkipped {
            worldwideDay: wwd.into(),
            reason: "day_metadosis_limit_not_found".into(),
            status: "SKIPPED".into(),
            blockNumber: ctx.block.block_number,
        })?;
        return Ok(U256::ZERO);
    }

    let day_limit = metadosis.get_day_limit(wwd)?;
    if day_limit.is_zero() {
        metadosis.mark_failed(wwd)?;
        metadosis.mark_day_limit_used(wwd)?;
        metadosis.emit(IMetadosis::MetadosisSkipped {
            worldwideDay: wwd.into(),
            reason: "day_metadosis_limit_is_zero".into(),
            status: "SKIPPED".into(),
            blockNumber: ctx.block.block_number,
        })?;
        return Ok(U256::ZERO);
    }

    if metadosis.get_day_type(wwd)? == day_type::UNKNOWN {
        metadosis.mark_failed(wwd)?;
        metadosis.mark_day_limit_used(wwd)?;
        emit_failed_execution(metadosis, ctx, wwd, U256::ZERO, day_limit)?;
        return Ok(day_limit);
    }

    let tribute = TributeContract::new(metadosis.storage.clone());
    let day_totals = tribute.get_day_totals(wwd)?;
    if day_totals.tribute_count == 0 {
        let dtype = metadosis.get_day_type(wwd)?;
        let auction_ts = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()?;
        let to_promis_limit = settle_auction_clearing(ctx, dtype, auction_ts, day_limit)?;
        metadosis.mark_completed(wwd)?;
        metadosis.mark_day_limit_used(wwd)?;
        metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
            worldwideDay: wwd.into(),
            dayMetadosisLimit: day_limit,
            dayMetadosisLimitRemainder: day_limit,
            status: "COMPLETED".into(),
            dayState: day_state_label(dtype).into(),
            action: "no tributes".into(),
        })?;
        return Ok(to_promis_limit);
    }

    // AgentReward distribution is owned by the daily Cycle
    // handler (`outbe_cycle::handler::run_emission_limit_daily`).
    // Metadosis only consumes the terminal credit that the handler
    // writes via `dispatch_terminal_remainder_at`, so the previous
    // inline `distribute_agent_rewards(...)` call is gone — it would
    // double-distribute on top of the handler's per-pool dispatch.
    let effective_day_limit = day_limit;

    let tribute_nominal_total = day_totals.tribute_nominal_amount;
    let calc = metadosis.calculate_metadosis(wwd, tribute_nominal_total, effective_day_limit)?;

    match outbe_lysis::runtime::lysis(metadosis.storage.clone(), wwd, calc.day_gratis_allocation) {
        Ok(lysis_result) => {
            let remainder = lysis_result.remaining_gratis + calc.day_metadosis_limit_remainder;
            let dtype = metadosis.get_day_type(wwd)?;
            let auction_ts = metadosis
                .worldwide_days
                .entry(wwd)
                .scheduled_process_time()
                .read()?;
            let to_promis_limit = settle_auction_clearing(ctx, dtype, auction_ts, remainder)?;
            metadosis.mark_completed(wwd)?;
            metadosis.mark_day_limit_used(wwd)?;
            metadosis.emit(IMetadosis::MetadosisExecuted {
                worldwideDay: wwd.into(),
                tributeTotals: tribute_nominal_total,
                dayGratisDemand: calc.day_gratis_demand,
                dayGratisLimit: calc.day_gratis_limit,
                dayGratisAllocation: calc.day_gratis_allocation,
                dayGratisAllocationRemainder: lysis_result.remaining_gratis,
                netDayGratisAllocation: calc
                    .day_gratis_allocation
                    .saturating_sub(lysis_result.remaining_gratis),
                dayMetadosisLimitRemainder: remainder,
                status: "COMPLETED".into(),
                blockNumber: ctx.block.block_number,
            })?;
            metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
                worldwideDay: wwd.into(),
                dayMetadosisLimit: effective_day_limit,
                dayMetadosisLimitRemainder: remainder,
                status: "COMPLETED".into(),
                dayState: day_state_label(dtype).into(),
                action: calc.action.into(),
            })?;
            Ok(to_promis_limit)
        }
        Err(_) => {
            let dtype = metadosis.get_day_type(wwd)?;
            let auction_ts = metadosis
                .worldwide_days
                .entry(wwd)
                .scheduled_process_time()
                .read()?;
            let to_promis_limit =
                settle_auction_clearing(ctx, dtype, auction_ts, effective_day_limit)?;
            metadosis.mark_failed(wwd)?;
            metadosis.mark_day_limit_used(wwd)?;
            emit_failed_execution(
                metadosis,
                ctx,
                wwd,
                tribute_nominal_total,
                effective_day_limit,
            )?;
            Ok(to_promis_limit)
        }
    }
}

fn settle_auction_clearing(
    ctx: &BlockRuntimeContext,
    dtype: u8,
    auction_ts: u64,
    supply: U256,
) -> Result<U256> {
    if dtype != day_type::GREEN {
        return Ok(supply);
    }
    let delivered =
        outbe_desis::api::dispatch_stage_clearing(ctx.storage.clone(), auction_ts, supply)?;
    Ok(if delivered { U256::ZERO } else { supply })
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

fn cleanup_completed_days(metadosis: &mut MetadosisContract, timestamp: u64) -> Result<()> {
    const RETENTION_SECONDS: u64 = COMPLETED_WWD_RETENTION_DAYS * 24 * 3600;

    let active = metadosis.get_all_active_wwds()?;
    for wwd in active {
        let wwd_status = metadosis.get_status(wwd)?;
        if wwd_status != status::COMPLETED && wwd_status != status::FAILED {
            continue;
        }

        let scheduled = metadosis
            .worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .read()?;
        if scheduled > 0 && timestamp.saturating_sub(scheduled) > RETENTION_SECONDS {
            metadosis.remove_active_wwd(wwd)?;
            metadosis.delete_worldwide_day(wwd)?;
            metadosis.emit(IMetadosis::WorldwideDayCleanedUp {
                worldwideDay: wwd.into(),
                retentionDays: COMPLETED_WWD_RETENTION_DAYS,
            })?;
        }
    }

    Ok(())
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

fn day_state_label(dtype: u8) -> &'static str {
    match dtype {
        day_type::GREEN => "GREEN",
        day_type::RED => "RED",
        _ => "UNKNOWN",
    }
}

fn oracle_pair_hash(storage: StorageHandle) -> alloy_primitives::B256 {
    let metadosis = MetadosisContract::new(storage);
    let hash = metadosis.config_oracle_pair_hash.read().unwrap_or_default();
    if hash.is_zero() {
        OracleContract::pair_hash("COEN", "0xUSD")
    } else {
        hash
    }
}

fn oracle_store_worldwide_day_vwap_snapshot(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    start_time: u64,
    end_time: u64,
) -> Result<()> {
    let mut oracle = OracleContract::new(storage);
    match oracle.store_worldwide_day_vwap_snapshot(worldwide_day, start_time, end_time) {
        Ok(()) => Ok(()),
        Err(outbe_primitives::error::PrecompileError::Revert(msg))
            if msg.contains("no VWAP data") =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn oracle_worldwide_day_vwap_snapshot_value(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
) -> U256 {
    let oracle = OracleContract::new(storage.clone());
    let pair_hash = oracle_pair_hash(storage);
    let pair_id = oracle.pair_hash_to_id.read(&pair_hash).unwrap_or(0);
    if pair_id == 0 {
        return U256::ZERO;
    }
    oracle
        .get_worldwide_day_vwap_for_pair_id(worldwide_day, pair_id)
        .ok()
        .flatten()
        .unwrap_or(U256::ZERO)
}
