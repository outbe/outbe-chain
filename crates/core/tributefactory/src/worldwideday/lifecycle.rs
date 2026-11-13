//! Authoritative `WorldwideDay` lifecycle orchestration (`OIP-00343`).

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::{
        metadosis_advance_due_binding, metadosis_init_genesis_binding, MetadosisCycleLifecycle,
        StorageHandle,
    },
    time::{
        date_key_to_utc_timestamp, next_date_key, timestamp_to_date_key, WorldwideDay,
        SECONDS_PER_DAY, UTC_PLUS_14_OFFSET,
    },
};

use crate::schema::TributeFactoryContract;

use super::{
    aggregate::{
        close_worldwide_day, create_worldwide_day, NewWwdSchedule, ValidatedWwdAggregate,
        WwdProjection,
    },
    constants::{MAX_PIPELINE_WWDS, SECONDS_PER_HOUR},
    errors::{WwdCallerError, WwdStorageCorruptionError},
};

const GENESIS_CALENDAR_FLOOR_DATE_KEY: u32 = 20200101;

/// Computes the first `WorldwideDay` `D` whose `forming_end` is strictly after `genesis_timestamp`:
/// `first_wwd = timestamp_to_date_key(genesis_timestamp - 12h)`.
#[must_use]
pub fn first_wwd_for_genesis(genesis_timestamp: u64) -> WorldwideDay {
    WorldwideDay::new(timestamp_to_date_key(
        genesis_timestamp.saturating_sub(12 * SECONDS_PER_HOUR),
    ))
}

pub fn validate_wwd_timestamp(timestamp: u64) -> Result<()> {
    timestamp
        .checked_add(UTC_PLUS_14_OFFSET)
        .ok_or(WwdCallerError::UtcPlus14TimestampOverflow)?;
    Ok(())
}

pub fn compute_wwd_schedule(wwd: WorldwideDay) -> Result<(NewWwdSchedule, u64)> {
    let forming_start = wwd
        .start_timestamp()
        .map_err(super::errors::date_step_error)?;
    let forming_end = forming_start
        .checked_add(outbe_chain_constants::get_metadosis_forming_period_seconds())
        .ok_or(WwdCallerError::FormingWindowOverflow)?;
    let offerings_start = forming_end
        .checked_add(outbe_chain_constants::get_metadosis_lookback_delay_seconds())
        .ok_or(WwdCallerError::LookbackWindowOverflow)?;
    let offering_end = offerings_start
        .checked_add(outbe_chain_constants::get_metadosis_offering_period_seconds())
        .ok_or(WwdCallerError::OfferingWindowOverflow)?;
    let day = timestamp_to_date_key(offering_end);
    let metadosis_start = outbe_emission::api::compute_metadosis_start(day)?;
    Ok((
        NewWwdSchedule {
            day,
            forming_start,
            forming_end,
            offerings_start,
            offering_end,
        },
        metadosis_start,
    ))
}

pub fn create_worldwide_day_for_date(
    ctx: &BlockRuntimeContext<'_>,
    wwd: WorldwideDay,
) -> Result<()> {
    let (schedule, _) = compute_wwd_schedule(wwd)?;
    outbe_emission::api::link_wwd(ctx.storage.clone(), schedule.day, wwd)?;
    let mut factory = TributeFactoryContract::new(ctx.storage.clone());
    create_worldwide_day(&mut factory, wwd, schedule)?;
    Ok(())
}

/// Runs the daily `WorldwideDay` lifecycle step (OIP-00343 item 25, step A6).
///
/// On UTC day `X` (evaluated against the daily cutoff `00:00:00 UTC` of `X`, or
/// `ctx.block.timestamp` when `last_wwd_advance_day == 0` at genesis):
/// 1. Seals every pipeline WWD whose `offering_end <= cutoff_ts`.
/// 2. Creates every open WWD `D` up to `X + 1` (never earlier than `first_wwd_for_genesis`)
///    whose `offering_end > cutoff_ts`.
/// 3. Stores the forming VWAP snapshot for every pipeline WWD whose `forming_end <= cutoff_ts`.
/// 4. Records `last_wwd_advance_day = X` so subsequent hourly ticks on day `X` are no-ops.
pub fn advance_worldwide_days(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    validate_wwd_timestamp(ctx.block.timestamp)?;
    let block_utc_day = timestamp_to_date_key(ctx.block.timestamp);
    let factory = TributeFactoryContract::new(ctx.storage.clone());
    let last_day = factory.last_wwd_advance_day.read()?;
    if last_day >= block_utc_day {
        return Ok(());
    }

    let cutoff_ts =
        date_key_to_utc_timestamp(block_utc_day).map_err(super::errors::date_step_error)?;

    // 1. Seal every pipeline WWD whose offering window ended at or before 00:00 UTC of day X,
    //    ensuring its forming VWAP snapshot is finalized first if the chain skipped past both boundaries.
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    for current in aggregate.active_records() {
        verify_emission_day_link(&ctx.storage, current.day, current.worldwide_day)?;
        if current.forming_end <= cutoff_ts {
            let _ = snapshot_forming_vwap(ctx, current)?;
        }
        if current.offering_end <= cutoff_ts {
            let mut factory_mut = TributeFactoryContract::new(ctx.storage.clone());
            close_worldwide_day(&mut factory_mut, current.worldwide_day)?;
        }
    }

    // 2. Create open WWDs up to X + 1 (whose forming window starts at 10:00 UTC on day X).
    create_open_worldwide_days(ctx, block_utc_day, cutoff_ts, last_day == 0)?;

    // 3. Snapshot forming VWAP for every open pipeline WWD whose forming window ended at or before 00:00 UTC of day X.
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    for current in aggregate.active_records() {
        verify_emission_day_link(&ctx.storage, current.day, current.worldwide_day)?;
        if current.forming_end <= cutoff_ts {
            let _ = snapshot_forming_vwap(ctx, current)?;
        }
    }

    // 4. Record completion for UTC day X.
    factory.last_wwd_advance_day.write(block_utc_day)?;
    Ok(())
}

fn create_open_worldwide_days(
    ctx: &BlockRuntimeContext<'_>,
    block_utc_day: u32,
    cutoff_ts: u64,
    is_initial_run: bool,
) -> Result<()> {
    let target_day = next_date_key(block_utc_day).map_err(super::errors::date_step_error)?;
    let target_ts =
        date_key_to_utc_timestamp(target_day).map_err(super::errors::date_step_error)?;
    let window_span_days = (MAX_PIPELINE_WWDS as u64).saturating_sub(1);
    let earliest_open_ts = target_ts.saturating_sub(window_span_days * SECONDS_PER_DAY);

    let calendar_floor_ts = date_key_to_utc_timestamp(GENESIS_CALENDAR_FLOOR_DATE_KEY)
        .map_err(super::errors::date_step_error)?
        + 12 * SECONDS_PER_HOUR;
    let genesis_anchor_ts = if ctx.block.genesis_timestamp >= calendar_floor_ts
        && ctx.block.genesis_timestamp <= ctx.block.timestamp
    {
        ctx.block.genesis_timestamp
    } else if is_initial_run {
        ctx.block.timestamp
    } else {
        0
    };

    let start_ts = if genesis_anchor_ts > 0 {
        let genesis_first_wwd = first_wwd_for_genesis(genesis_anchor_ts);
        let genesis_first_ts = genesis_first_wwd
            .to_timestamp_utc()
            .map_err(super::errors::date_step_error)?;
        earliest_open_ts.max(genesis_first_ts)
    } else {
        earliest_open_ts
    };

    let factory = TributeFactoryContract::new(ctx.storage.clone());
    let mut wwd_ts = start_ts;
    while wwd_ts <= target_ts {
        let wwd = WorldwideDay::new(timestamp_to_date_key(wwd_ts));
        if factory.worldwide_days.get(wwd)?.is_none() {
            let (schedule, _) = compute_wwd_schedule(wwd)?;
            if schedule.offering_end > cutoff_ts {
                create_worldwide_day_for_date(ctx, wwd)?;
            }
        }
        wwd_ts = wwd_ts.saturating_add(SECONDS_PER_DAY);
    }
    Ok(())
}

/// Command-bound entry point for `advance_worldwide_days` used by `outbe-cycle` during `ProcessEmissionDay`.
pub fn advance_worldwide_days_command(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    run_with_wwd_mutation_binding(
        ctx,
        metadosis_advance_due_binding(
            ctx.block.chain_id,
            ctx.block.block_number,
            ctx.block.timestamp,
        ),
    )
}

/// Command-bound entry point used by `handle_cycle_tick` at activation height.
pub fn bootstrap_genesis_worldwide_days_command(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    run_with_wwd_mutation_binding(
        ctx,
        metadosis_init_genesis_binding(
            ctx.block.chain_id,
            ctx.block.block_number,
            ctx.block.timestamp,
        ),
    )
}

fn run_with_wwd_mutation_binding(
    ctx: &BlockRuntimeContext<'_>,
    binding: alloy_primitives::B256,
) -> Result<()> {
    let command_storage = ctx.storage.clone();
    ctx.storage
        .clone()
        .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
            ValidatedWwdAggregate::load_and_validate(command_storage.clone())?;
            advance_worldwide_days(ctx)?;
            ValidatedWwdAggregate::load_and_validate(command_storage)?;
            Ok(())
        })
}

fn verify_emission_day_link(
    storage: &StorageHandle<'_>,
    day: u32,
    expected_wwd: WorldwideDay,
) -> Result<()> {
    if let Some(emission_day) = outbe_emission::api::get_emission_day_record(storage.clone(), day)?
    {
        if emission_day.wwd != expected_wwd {
            return Err(WwdStorageCorruptionError::EmissionDayWwdLinkMismatch {
                day,
                expected_wwd,
                found_wwd: emission_day.wwd,
            }
            .into());
        }
    }
    Ok(())
}

pub fn snapshot_forming_vwap(
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
) -> Result<bool> {
    let wwd = current.worldwide_day;
    let factory = TributeFactoryContract::new(ctx.storage.clone());
    if factory.forming_snapshot_finalized.read(&wwd)? {
        return outbe_oracle::api::has_worldwide_day_vwap_snapshot(ctx.storage.clone(), wwd);
    }
    match outbe_oracle::api::store_worldwide_day_vwap_snapshot(
        ctx.storage.clone(),
        wwd,
        current.forming_start,
        current.forming_end,
    ) {
        Ok(stored) => {
            factory.forming_snapshot_finalized.write(&wwd, true)?;
            Ok(stored)
        }
        Err(error)
            if error.to_string().contains("VWAP must be non-zero") || is_vwap_overflow(&error) =>
        {
            factory.forming_snapshot_finalized.write(&wwd, true)?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

alloy_sol_types::sol! {
    error VwapOverflow(string accumulator);
}

fn is_vwap_overflow(error: &PrecompileError) -> bool {
    use alloy_sol_types::SolError;
    if matches!(
        error,
        PrecompileError::RevertBytes(bytes)
            if bytes.len() >= 4 && bytes[..4] == VwapOverflow::SELECTOR
    ) {
        return true;
    }
    let text = error.to_string();
    text.contains("overflow in WWD")
        || text.contains("division by zero in WWD")
        || text.contains("overflow in window")
}
