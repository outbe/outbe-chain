use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    chain,
    error::{PrecompileError, Result},
    time::{
        date_key_to_utc_timestamp as primitives_date_key_to_timestamp,
        previous_date_key as primitives_previous_date_key, timestamp_to_date_key as utc_date_key,
    },
};
use outbe_tribute::TributeContract;

use crate::constants::*;
use crate::precompile::IMetadosis;
use crate::schema::{status, MetadosisContract, WorldwideDayEntryExt};

fn checked_hours_to_seconds(hours: u64, label: &'static str) -> Result<u64> {
    hours
        .checked_mul(SECONDS_PER_HOUR)
        .ok_or_else(|| PrecompileError::Revert(format!("metadosis {label} seconds overflow")))
}

fn checked_timestamp_add(base: u64, delta: u64, label: &'static str) -> Result<u64> {
    base.checked_add(delta)
        .ok_or_else(|| PrecompileError::Revert(format!("metadosis {label} timestamp overflow")))
}

impl MetadosisContract<'_> {
    /// Returns effective lookback and offering hours for a day created at `now`.
    ///
    /// Devnet/testnet use the accelerated bootstrap schedule, but ONLY while the
    /// chain is still inside its bootstrap window: `now < bootstrap_end_time`
    /// (the boundary is written once on block 1 as `block_ts + BOOTSTRAP_DURATION`).
    /// Past the boundary — or on mainnet, where the field is never set (`0`) —
    /// the normal schedule applies. This makes `bootstrap_end_time` actually bound
    /// the schedule it names instead of being a dead, RPC-only field.
    pub fn effective_hours(&self, chain_id: u64, now: u64) -> Result<(u64, u64)> {
        let in_bootstrap = (chain::is_devnet(chain_id) || chain::is_testnet(chain_id)) && {
            let end = self.get_bootstrap_end_time()?;
            end != 0 && now < end
        };
        if in_bootstrap {
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
/// `worldwideday::advance_worldwide_day` for active WWDs, `process_metadosis`
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

    // Phase 0 — genesis bootstrap (block 1 only, idempotent).
    if ctx.block.block_number == 1 {
        init_genesis_day_inner(&mut metadosis, ctx)?;
    }
    // Phase 1 — ensure today's worldwide day exists.
    create_worldwide_day_if_needed(&mut metadosis, ctx, ctx.block.timestamp)?;

    let mut active = metadosis.active_wwd.read_all()?;
    active.sort_unstable();
    // Phase 2 — advance every active day by the clock.
    advance_all_active_days(ctx, &active)?;
    // Phase 3 — settle the oldest READY day, if a backlog contains several.
    settle_ready_day(ctx, &metadosis, &active)?;

    // Terminal-day cleanup is no longer a per-tick scan: each COMPLETED/FAILED
    // transition retires the day into the bounded `closed_wwd` delete-queue (see
    // `MetadosisContract::mark_wwd_*`), evicting the oldest past `MAX_RECORDS_KEPT`.
    Ok(())
}

/// Phase 2 of [`start_metadosis`]: walk every active day forward to its
/// time-phase. Pure clock progression — no settlement.
fn advance_all_active_days(ctx: &BlockRuntimeContext, active: &[WorldwideDay]) -> Result<()> {
    for wwd in active {
        crate::worldwideday::advance_worldwide_day(ctx, *wwd)?;
    }
    Ok(())
}

/// Phase 3 of [`start_metadosis`]: settle the oldest day that reached READY (at
/// most one per tick), running the full Metadosis flow for it. The caller passes
/// `active` in ascending WorldwideDay order, so storage-set swap ordering cannot
/// influence which backlog item is processed first.
fn settle_ready_day(
    ctx: &BlockRuntimeContext,
    metadosis: &MetadosisContract,
    active: &[WorldwideDay],
) -> Result<()> {
    for wwd in active {
        if metadosis.get_wwd_status(*wwd)? == status::READY {
            crate::worldwideday::process_metadosis(ctx, *wwd)?;
            break;
        }
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
        let end_time = checked_timestamp_add(
            ctx.block.timestamp,
            checked_hours_to_seconds(BOOTSTRAP_DURATION_HOURS, "bootstrap duration")?,
            "bootstrap_end_time",
        )?;
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

    let (lookback_hours, offering_hours) =
        metadosis.effective_hours(ctx.block.chain_id, ctx.block.timestamp)?;
    let forming_start = wwd.start_timestamp();
    metadosis.create_worldwide_day(wwd, forming_start, lookback_hours, offering_hours)?;
    metadosis.add_active_wwd(wwd)?;

    let mut tribute = TributeContract::new(metadosis.storage.clone());
    tribute.seal_day(wwd)?;

    let windows = metadosis.get_day_windows(wwd)?;
    metadosis.emit(IMetadosis::WorldwideDayStarted {
        worldwideDay: wwd.into(),
        formingStart: forming_start,
        formingEnd: windows.forming_end,
        offeringStart: windows.lookback_end,
        offeringEnd: windows.offering_end,
        scheduledTime: windows.scheduled,
    })?;

    Ok(())
}
