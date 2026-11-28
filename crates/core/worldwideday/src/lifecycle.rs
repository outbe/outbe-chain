//! Authoritative `WorldwideDay` lifecycle orchestration (`OIP-00343`).

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{CycleLifecyclePurpose, StorageHandle},
};
use outbe_system_common::{
    metadosis_bindings::{metadosis_advance_due_binding, metadosis_init_genesis_binding},
    time::{
        date_key_to_utc_timestamp, next_date_key, timestamp_to_date_key, WorldwideDay,
        SECONDS_PER_DAY, UTC_PLUS_14_OFFSET,
    },
};

use crate::schema::WorldwideDayContract;

use super::{
    aggregate::{close_worldwide_day, open_worldwide_day, NewWwdSchedule, ValidatedWwdAggregate},
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
    let offering_start = forming_start
        .checked_add(outbe_chain_constants::OFFERING_START_AFTER_FORMING_START_SECONDS)
        .ok_or(WwdCallerError::OfferingWindowOverflow)?;
    let offering_end = offering_start
        .checked_add(outbe_chain_constants::get_metadosis_offering_period_seconds())
        .ok_or(WwdCallerError::OfferingWindowOverflow)?;
    let emission_day = next_date_key(wwd.value()).map_err(super::errors::date_step_error)?;
    let metadosis_start = outbe_emission::api::compute_metadosis_start(emission_day)?;
    Ok((
        NewWwdSchedule {
            emission_day,
            forming_start,
            forming_end,
            offering_start,
            offering_end,
        },
        metadosis_start,
    ))
}

pub fn open_worldwide_day_for_date(
    ctx: &BlockRuntimeContext<'_>,
    wwd: WorldwideDay,
) -> Result<bool> {
    let (schedule, _) = compute_wwd_schedule(wwd)?;
    let mut factory = WorldwideDayContract::new(ctx.storage.clone());
    open_worldwide_day(&mut factory, wwd, schedule)
}

/// Runs the `WorldwideDay` step of a Cycle run and returns the WWDs it opened.
///
/// 1. Seals every pipeline WWD whose offering ended at or before the block time.
/// 2. Once per UTC day `X` (or at genesis), opens every WWD `D` up to `X` (never earlier than
///    `first_wwd_for_genesis`) whose offering has not ended, and records `last_wwd_advance_day = X`.
///
/// The caller makes the record of emission day `D + 1` for each opened WWD in the same checkpoint.
pub fn advance_worldwide_days(ctx: &BlockRuntimeContext<'_>) -> Result<Vec<WorldwideDay>> {
    validate_wwd_timestamp(ctx.block.timestamp)?;
    let now = ctx.block.timestamp;
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    for current in aggregate.active_records() {
        verify_emission_day_link(&ctx.storage, current.emission_day, current.wwd)?;
        if current.offering_end <= now {
            let mut factory_mut = WorldwideDayContract::new(ctx.storage.clone());
            close_worldwide_day(&mut factory_mut, current.wwd)?;
        }
    }

    let block_utc_day = timestamp_to_date_key(now);
    let factory = WorldwideDayContract::new(ctx.storage.clone());
    let last_day = factory.last_wwd_advance_day.read()?;
    if last_day >= block_utc_day {
        return Ok(Vec::new());
    }
    let opened = open_worldwide_days(ctx, block_utc_day, last_day == 0)?;
    factory.last_wwd_advance_day.write(block_utc_day)?;
    Ok(opened)
}

fn open_worldwide_days(
    ctx: &BlockRuntimeContext<'_>,
    block_utc_day: u32,
    is_initial_run: bool,
) -> Result<Vec<WorldwideDay>> {
    let target_ts =
        date_key_to_utc_timestamp(block_utc_day).map_err(super::errors::date_step_error)?;
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

    let factory = WorldwideDayContract::new(ctx.storage.clone());
    let mut opened = Vec::new();
    let mut wwd_ts = start_ts;
    while wwd_ts <= target_ts {
        let wwd = WorldwideDay::new(timestamp_to_date_key(wwd_ts));
        if factory.worldwide_days.get(wwd)?.is_none() {
            let (schedule, _) = compute_wwd_schedule(wwd)?;
            if schedule.offering_end > ctx.block.timestamp && open_worldwide_day_for_date(ctx, wwd)?
            {
                opened.push(wwd);
            }
        }
        wwd_ts = wwd_ts.saturating_add(SECONDS_PER_DAY);
    }
    Ok(opened)
}

/// Command-bound entry point for `advance_worldwide_days` used by `outbe-cycle` during `ProcessEmissionDay`.
pub fn advance_worldwide_days_command(ctx: &BlockRuntimeContext<'_>) -> Result<Vec<WorldwideDay>> {
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
pub fn bootstrap_genesis_worldwide_days_command(
    ctx: &BlockRuntimeContext<'_>,
) -> Result<Vec<WorldwideDay>> {
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
) -> Result<Vec<WorldwideDay>> {
    let command_storage = ctx.storage.clone();
    ctx.storage
        .clone()
        .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
            ValidatedWwdAggregate::load_and_validate(command_storage.clone())?;
            let opened = advance_worldwide_days(ctx)?;
            ValidatedWwdAggregate::load_and_validate(command_storage)?;
            Ok(opened)
        })
}

fn verify_emission_day_link(
    storage: &StorageHandle<'_>,
    day: u32,
    expected_wwd: WorldwideDay,
) -> Result<()> {
    if let Some(emission_day) = outbe_emission::api::get_emission_day(storage.clone(), day)? {
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
