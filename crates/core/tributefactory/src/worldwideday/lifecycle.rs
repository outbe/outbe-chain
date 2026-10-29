//! Authoritative `WorldwideDay` lifecycle orchestration (`OIP-00343`).

use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::{metadosis_advance_due_binding, MetadosisCycleLifecycle, StorageHandle},
    time::{
        date_key_to_utc_timestamp, day_number_between, timestamp_to_date_key, WorldwideDay,
        SECONDS_PER_DAY, UTC_PLUS_14_OFFSET,
    },
};

use crate::schema::TributeFactoryContract;

use super::{
    aggregate::{
        activate_worldwide_day, close_worldwide_day, create_worldwide_day, NewWwdSchedule,
        ValidatedWwdAggregate, WwdProjection,
    },
    constants::SECONDS_PER_HOUR,
    errors::{WwdCallerError, WwdStorageCorruptionError},
};

const GENESIS_CALENDAR_FLOOR_DATE_KEY: u32 = 20200101;
const MAX_WWD_CATCHUP_DAYS: u32 = 64;

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
    let forming_start = wwd.start_timestamp();
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

pub fn init_genesis_day(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    create_lookahead_days(ctx, ctx.block.timestamp)
}

pub fn create_lookahead_days(ctx: &BlockRuntimeContext<'_>, timestamp: u64) -> Result<()> {
    validate_wwd_timestamp(timestamp)?;
    let base_wwd = first_wwd_for_genesis(timestamp);
    let base_ts = date_key_to_utc_timestamp(base_wwd.value());
    let lookahead = outbe_chain_constants::get_metadosis_lookahead_days();
    create_catchup_days(ctx, timestamp, base_wwd, base_ts, lookahead)?;

    for offset in 0..lookahead {
        let wwd_ts = base_ts.saturating_add(u64::from(offset) * SECONDS_PER_DAY);
        let wwd = WorldwideDay::new(timestamp_to_date_key(wwd_ts));
        create_worldwide_day_for_date(ctx, wwd)?;
    }
    Ok(())
}

fn create_catchup_days(
    ctx: &BlockRuntimeContext<'_>,
    timestamp: u64,
    base_wwd: WorldwideDay,
    base_ts: u64,
    lookahead: u32,
) -> Result<()> {
    let calendar_floor_ts =
        date_key_to_utc_timestamp(GENESIS_CALENDAR_FLOOR_DATE_KEY) + 12 * SECONDS_PER_HOUR;
    if ctx.block.genesis_timestamp < calendar_floor_ts || ctx.block.genesis_timestamp > timestamp {
        return Ok(());
    }
    let genesis_first_wwd = first_wwd_for_genesis(ctx.block.genesis_timestamp);
    if genesis_first_wwd >= base_wwd {
        return Ok(());
    }
    let Ok(gap_days) = day_number_between(genesis_first_wwd.value(), base_wwd.value()) else {
        return Ok(());
    };
    let catchup_days = gap_days.min(MAX_WWD_CATCHUP_DAYS);
    let start_ts = base_ts.saturating_sub(u64::from(catchup_days) * SECONDS_PER_DAY);
    let factory = TributeFactoryContract::new(ctx.storage.clone());
    let max_catchup_pipeline =
        super::constants::MAX_PIPELINE_WWDS.saturating_sub(lookahead as usize);
    for offset in 0..catchup_days {
        let wwd_ts = start_ts.saturating_add(u64::from(offset) * SECONDS_PER_DAY);
        let wwd = WorldwideDay::new(timestamp_to_date_key(wwd_ts));
        if factory.worldwide_days.get(wwd)?.is_none() {
            let (schedule, _) = compute_wwd_schedule(wwd)?;
            if schedule.offering_end > timestamp
                && factory.pipeline_wwd.read_all()?.len() >= max_catchup_pipeline
            {
                continue;
            }
            create_worldwide_day_for_date(ctx, wwd)?;
        }
    }
    Ok(())
}

pub fn advance_active_worldwide_days(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    let now = ctx.block.timestamp;

    for current in aggregate.active_records() {
        advance_single_worldwide_day(ctx, current, now)?;
    }
    Ok(())
}

/// Runs the complete `WorldwideDay` lifecycle step (closing expired days, creating
/// catch-up and lookahead `WorldwideDay`s, and advancing forming/offering transitions).
pub fn advance_worldwide_days(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    validate_wwd_timestamp(ctx.block.timestamp)?;
    advance_active_worldwide_days(ctx)?;
    create_lookahead_days(ctx, ctx.block.timestamp)?;
    advance_active_worldwide_days(ctx)
}

/// Command-bound entry point for `advance_worldwide_days` used by `outbe-cycle`.
pub fn advance_worldwide_days_command(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let binding = metadosis_advance_due_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
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

fn advance_single_worldwide_day(
    ctx: &BlockRuntimeContext<'_>,
    current: &WwdProjection,
    now: u64,
) -> Result<()> {
    let wwd = current.worldwide_day;
    verify_emission_day_link(&ctx.storage, current.day, wwd)?;

    let has_forming_vwap = if now >= current.forming_end {
        snapshot_forming_vwap(ctx, current)?
    } else {
        false
    };

    let mut factory = TributeFactoryContract::new(ctx.storage.clone());
    if now >= current.offering_end {
        close_worldwide_day(&mut factory, wwd)?;
    } else if now >= current.offerings_start && has_forming_vwap {
        activate_worldwide_day(&mut factory, wwd)?;
    }
    Ok(())
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

fn snapshot_forming_vwap(ctx: &BlockRuntimeContext<'_>, current: &WwdProjection) -> Result<bool> {
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
