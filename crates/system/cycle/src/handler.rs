//! ProcessEmissionDay calendar orchestration of the emission day.
//!
//! The Cycle only calls the steps of the day when their time comes, with the day: the WWD step and
//! the records of the days, `start_day_emission_limit` at 00:00 after the day, and the WAA and SRA
//! rewards with Metadosis when the offering of the day ends. Each module reads its limit from the
//! record of the day and writes its result into it.

use outbe_agentreward::api::RewardPool;
use outbe_emission::schema::{emission_day_state, metadosis_state};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_system_common::time::{
    date_key_to_utc_timestamp, next_date_key, previous_date_key, timestamp_to_date_key, TimeError,
    WorldwideDay,
};

use crate::{errors::CycleError, tasks::Readiness};

/// Maximum number of completed UTC days that `derive_active_utc_day` scans backward
/// and catches up (one oldest unallocated day per hourly run, OIP-00343).
pub const MAX_REWARD_CATCHUP_DAYS: u32 = 64;

fn latest_allocated_day_through(
    storage: StorageHandle<'_>,
    from_day: u32,
    start_date: u32,
) -> Result<Option<u32>> {
    let mut d = from_day;
    loop {
        if let Some(record) = outbe_emission::api::get_emission_day(storage.clone(), d)? {
            if record.state == emission_day_state::PROCESSING
                || record.state == emission_day_state::COMPLETED
            {
                return Ok(Some(d));
            }
        }
        if d <= start_date {
            return Ok(None);
        }
        d = previous_date_key(d).map_err(calendar_step_error)?;
    }
}

/// Derives the active UTC calendar day without a stored cursor (OIP-00343).
///
/// Scans backward from `yesterday = previous_date_key(block_utc_day)` down to
/// `max(start_date, yesterday - 63)` (`MAX_REWARD_CATCHUP_DAYS = 64`), stopping at the first
/// `EmissionDay` whose `state` is `PROCESSING` or `COMPLETED` (pre-linked future
/// shells have `state == PENDING` and do not stop the scan).
pub fn derive_active_utc_day(
    storage: StorageHandle<'_>,
    genesis_timestamp: u64,
    block_timestamp: u64,
) -> Result<u32> {
    if genesis_timestamp == 0 {
        return Ok(0);
    }
    let start_date = timestamp_to_date_key(genesis_timestamp);
    if !is_valid_utc_day(start_date) {
        return Ok(start_date);
    }
    let block_utc_day = timestamp_to_date_key(block_timestamp);
    if !is_valid_utc_day(block_utc_day) || block_utc_day <= start_date {
        return Ok(start_date);
    }
    let yesterday = previous_date_key(block_utc_day).map_err(calendar_step_error)?;
    let yesterday_ts = date_key_to_utc_timestamp(yesterday).map_err(calendar_step_error)?;
    let catchup_span_secs = u64::from(MAX_REWARD_CATCHUP_DAYS.saturating_sub(1))
        * outbe_system_common::time::SECONDS_PER_DAY;
    let catchup_floor = timestamp_to_date_key(yesterday_ts.saturating_sub(catchup_span_secs));
    let floor_day = start_date.max(catchup_floor);
    let d_allocated = latest_allocated_day_through(storage, yesterday, floor_day)?;
    match d_allocated {
        Some(allocated) => next_date_key(allocated).map_err(calendar_step_error),
        None => Ok(floor_day),
    }
}

/// Returns the day that an hourly ProcessEmissionDay execution allocates, if any.
///
/// If `active_utc_day < block_utc_day`, it is the single oldest unallocated UTC day
/// `active_utc_day` (`E_next`), catching up at most one day per hourly run within `MAX_REWARD_CATCHUP_DAYS`.
/// The executor shares this decision, so it derives command authority from the same calendar.
pub fn day_to_allocate(active_utc_day: u32, block_utc_day: u32) -> Result<Option<u32>> {
    if !is_valid_utc_day(active_utc_day) {
        return Err(CycleError::InvalidActiveUtcDay {
            active: active_utc_day,
            block: block_utc_day,
        }
        .into());
    }
    if !is_valid_utc_day(block_utc_day) {
        return Err(CycleError::InvalidBlockUtcDay {
            active: active_utc_day,
            block: block_utc_day,
        }
        .into());
    }
    if active_utc_day > block_utc_day {
        return Err(CycleError::ActiveUtcDayAfterBlock {
            active: active_utc_day,
            block: block_utc_day,
        }
        .into());
    }

    Ok((active_utc_day < block_utc_day).then_some(active_utc_day))
}

/// Maps a date step to the cycle error class.
///
/// `TimeError` is `non_exhaustive`. The trailing arm keeps `PreGenesis` and any
/// later variant on the fatal path. It does not invent a `DateStep` for them.
fn calendar_step_error(err: TimeError) -> PrecompileError {
    match err {
        TimeError::DateBeforeMinimum { date_key } => {
            CycleError::DateBeforeMinimum { date_key }.into()
        }
        TimeError::DateOverflow { date_key } => CycleError::DateOverflow { date_key }.into(),
        TimeError::TimestampStepOverflow { date_key } => {
            CycleError::TimestampStepOverflow { date_key }.into()
        }
        err => PrecompileError::Fatal(err.to_string()),
    }
}

/// Returns `true` when `date_key` is a `YYYYMMDD` UTC day from 1970 or later
/// that round-trips through its UTC midnight timestamp.
fn is_valid_utc_day(date_key: u32) -> bool {
    let year = date_key / 10_000;
    let month = (date_key / 100) % 100;
    let day = date_key % 100;
    year >= 1970
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && date_key_to_utc_timestamp(date_key).is_ok_and(|ts| timestamp_to_date_key(ts) == date_key)
}

/// Makes the record of emission day `D + 1` for each WWD `D` that the step opened.
fn create_linked_records(ctx: &BlockRuntimeContext, opened: &[WorldwideDay]) -> Result<()> {
    for wwd in opened {
        let (schedule, _) = outbe_worldwideday::compute_wwd_schedule(*wwd)?;
        match outbe_emission::api::get_emission_day(ctx.storage.clone(), schedule.emission_day)? {
            None => {
                outbe_emission::api::create_emission_day(ctx, schedule.emission_day, *wwd)?;
            }
            Some(record) if record.wwd == *wwd => {}
            Some(_) => {
                return Err(CycleError::EmissionDayWwdConflict {
                    day: schedule.emission_day,
                    wwd: wwd.value(),
                }
                .into())
            }
        }
    }
    Ok(())
}

/// The first block makes the first WWDs and the records of the genesis day and of the next day.
/// A day of the two that has no WWD gets a record with no WWD.
pub(crate) fn bootstrap_genesis_days(ctx: &BlockRuntimeContext) -> Result<()> {
    let opened = outbe_worldwideday::bootstrap_genesis_worldwide_days_command(ctx)?;
    create_linked_records(ctx, &opened)?;
    let genesis_day = timestamp_to_date_key(ctx.block.genesis_timestamp);
    for day in [
        genesis_day,
        next_date_key(genesis_day).map_err(calendar_step_error)?,
    ] {
        if outbe_emission::api::get_emission_day(ctx.storage.clone(), day)?.is_none() {
            outbe_emission::api::create_emission_day(ctx, day, WorldwideDay::new(0))?;
        }
    }
    Ok(())
}

/// Seals the WWDs whose offering ended and, once a UTC day, opens the WWDs of the day with the
/// records of their emission days, in one checkpoint. A revert is logged and tried again later.
pub(crate) fn run_advance_worldwide_days_step(
    ctx: &BlockRuntimeContext,
    block_utc_day: u32,
) -> Result<()> {
    match ctx.storage.with_checkpoint(|| {
        let opened = outbe_worldwideday::advance_worldwide_days_command(ctx)?;
        create_linked_records(ctx, &opened)
    }) {
        Ok(()) => Ok(()),
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                block_utc_day,
                error = ?err,
                "advance_worldwide_days step reverted; inner checkpoint rolled back"
            );
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// The step at 00:00 after emission day `day`: seals its votes and fees, sets the rewards of the
/// validators and of the CCA, settles the burns and starts the record of the day.
pub fn start_day_emission_limit(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    outbe_rewards::api::seal_votes_and_fees(ctx, day)?;
    outbe_agentreward::api::set_agent_reward_rate(ctx, day, RewardPool::Validator)?;
    outbe_agentreward::api::set_agent_reward_rate(ctx, day, RewardPool::Cca)?;
    outbe_promislimit::PromisLimitContract::new(ctx.storage.clone()).settle_day_burns()?;
    outbe_emission::api::record_day_started(ctx.storage.clone(), day).map(|_| ())
}

/// Whether the record of `day` exists, or was made now with no WWD because the WWD of its spends
/// can no longer open. A day whose WWD can still open waits for the WWD step.
fn ensure_day_record(ctx: &BlockRuntimeContext, day: u32) -> Result<bool> {
    if outbe_emission::api::get_emission_day(ctx.storage.clone(), day)?.is_some() {
        return Ok(true);
    }
    let wwd = WorldwideDay::new(previous_date_key(day).map_err(calendar_step_error)?);
    let (schedule, _) = outbe_worldwideday::compute_wwd_schedule(wwd)?;
    if ctx.block.timestamp < schedule.offering_end {
        return Ok(false);
    }
    outbe_emission::api::create_emission_day(ctx, day, WorldwideDay::new(0))?;
    Ok(true)
}

/// Starts `day` and returns it when this run started it. A revert changes nothing, and a later
/// run tries the day again.
fn run_start_day_step(ctx: &BlockRuntimeContext, day: u32) -> Result<Option<u32>> {
    if is_day_started(ctx, day)? {
        return Ok(None);
    }
    match ctx.storage.with_checkpoint(|| {
        if !ensure_day_record(ctx, day)? {
            return Ok(false);
        }
        start_day_emission_limit(ctx, day).map(|()| true)
    }) {
        Ok(true) => Ok(Some(day)),
        Ok(false) => Ok(None),
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                day,
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                error = ?err,
                "start_day_emission_limit step reverted; the day starts in a later run"
            );
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Returns whether `day` is started (`state == PROCESSING || state == COMPLETED`).
fn is_day_started(ctx: &BlockRuntimeContext, day: u32) -> Result<bool> {
    Ok(
        outbe_emission::api::get_emission_day(ctx.storage.clone(), day)?.is_some_and(|record| {
            record.state == emission_day_state::PROCESSING
                || record.state == emission_day_state::COMPLETED
        }),
    )
}

/// The step at 12:00 after emission day `day`, when its offering ended: the rewards of the WAA
/// and of the SRA, then Metadosis. Each reward rate has its own fallback, so both pools have a
/// result before Metadosis ends. A revert that a fallback does not absorb changes nothing, and a
/// later run tries the step again.
fn run_offering_end_step(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    match ctx.storage.with_checkpoint(|| {
        outbe_agentreward::api::set_agent_reward_rate(ctx, day, RewardPool::Waa)?;
        outbe_agentreward::api::set_agent_reward_rate(ctx, day, RewardPool::Sra)?;
        run_metadosis_step(ctx, day)
    }) {
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                error = ?err,
                day,
                "offering end step reverted; the step runs in a later run"
            );
            Ok(())
        }
        result => result,
    }
}

fn is_recoverable_step_error(err: &PrecompileError) -> bool {
    err.record_pass_failure() == outbe_primitives::error::PassFailure::Skip
}

fn run_metadosis_step(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    match ctx
        .storage
        .with_checkpoint(|| outbe_metadosis::commands::start_metadosis(ctx, day))
    {
        Ok(()) => Ok(()),
        Err(err) if is_recoverable_step_error(&err) => {
            tracing::warn!(
                target: "outbe::cycle",
                block_number = ctx.block.block_number,
                block_timestamp = ctx.block.timestamp,
                error = ?err,
                day,
                "start_metadosis step reverted; routing metadosis limit to PromisLimit"
            );
            record_failed_metadosis_fallback(ctx, day)
        }
        Err(err) => Err(err),
    }
}

fn record_failed_metadosis_fallback(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    ctx.storage
        .with_checkpoint(|| outbe_metadosis::commands::fail_metadosis(ctx, day))
}

/// Runs the hourly emission day entry point. Until the votes of the day to start are sealed, it
/// starts nothing, and with no Metadosis due it changes nothing and answers not ready.
/// 1. The WWD step: seals each WWD whose offering ended, and once a UTC day opens the WWDs of the
///    day with the records of their emission days, in its own checkpoint.
/// 2. `start_day_emission_limit` for the oldest emission day not started, in its own checkpoint.
/// 3. The step at the offering end, from 12:00 UTC after the start: the WAA and SRA rewards and
///    Metadosis, for the latest day started before this run, else for the day step 2 started.
pub fn process_emission_day(ctx: &BlockRuntimeContext) -> Result<Readiness> {
    process_emission_day_steps(ctx).map_err(fail_block)
}

/// A revert that reaches the run comes from a step fallback or from code outside the step
/// checkpoints. The runner would consume the slot and try the same day again, so it fails the block.
fn fail_block(err: PrecompileError) -> PrecompileError {
    match err {
        err @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_)) => {
            PrecompileError::FatalWithCause(Box::new(err))
        }
        err => err,
    }
}

fn process_emission_day_steps(ctx: &BlockRuntimeContext) -> Result<Readiness> {
    let block_utc_day = timestamp_to_date_key(ctx.block.timestamp);
    let active_utc_day = derive_active_utc_day(
        ctx.storage.clone(),
        ctx.block.genesis_timestamp,
        ctx.block.timestamp,
    )?;
    let day = day_to_allocate(active_utc_day, block_utc_day)?;
    let due_metadosis = match previous_date_key(active_utc_day) {
        Ok(latest_started) => metadosis_due(ctx, latest_started)?,
        Err(_) => None,
    };
    let start = match day {
        Some(day) if outbe_rewards::api::is_voting_complete(ctx, day)? => Some(day),
        Some(_) if due_metadosis.is_none() => return Ok(Readiness::NotReady),
        _ => None,
    };

    run_advance_worldwide_days_step(ctx, block_utc_day)?;

    let started = match start {
        Some(day) => run_start_day_step(ctx, day)?,
        None => None,
    };
    let metadosis_day = match (due_metadosis, started) {
        (Some(day), _) => Some(day),
        (None, Some(day)) => metadosis_due(ctx, day)?,
        (None, None) => None,
    };
    if let Some(day) = metadosis_day {
        run_offering_end_step(ctx, day)?;
    }

    tracing::info!(
        target: "outbe::cycle",
        block_number = ctx.block.block_number,
        block_timestamp = ctx.block.timestamp,
        active_utc_day,
        block_utc_day,
        day_to_start = ?day,
        metadosis_day = ?metadosis_day,
        "process_emission_day completed"
    );
    Ok(Readiness::Done)
}

/// Returns `day` when it started, its Metadosis is pending and its start time has come.
fn metadosis_due(ctx: &BlockRuntimeContext, day: u32) -> Result<Option<u32>> {
    let due =
        outbe_emission::api::get_emission_day(ctx.storage.clone(), day)?.is_some_and(|record| {
            record.state == emission_day_state::PROCESSING
                && record.metadosis_state == metadosis_state::PENDING
                && ctx.block.timestamp >= record.metadosis_start
        });
    Ok(due.then_some(day))
}
