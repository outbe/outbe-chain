//! EmissionDay and WorldwideDay lifecycle orchestration.

use std::collections::BTreeSet;

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    time::{
        date_key_to_utc_timestamp, next_date_key, previous_date_key, timestamp_to_date_key,
        WorldwideDay, SECONDS_PER_DAY,
    },
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;
use outbe_tributefactory::{
    worldwideday::{
        create_worldwide_day, remove_active_worldwide_day, NewWwdSchedule, ValidatedWwdAggregate,
    },
    TributeFactoryContract,
};

use crate::{
    errors::{MetadosisCallerError, MetadosisStorageCorruptionError},
    precompile::IMetadosis,
    schema::{emission_day_state, EmissionDay, MetadosisContract},
};

const MAX_DUE_SCAN_DAYS: u32 = 64;

fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => MetadosisCallerError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

pub(crate) fn validate_metadosis_timestamp(timestamp: u64) -> Result<()> {
    outbe_tributefactory::worldwideday::validate_wwd_timestamp(timestamp)
}

pub(crate) fn init_genesis_day(ctx: &BlockRuntimeContext) -> Result<()> {
    outbe_tributefactory::worldwideday::init_genesis_day(ctx)
}

pub(crate) fn init_genesis_day_inner(
    _metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
) -> Result<()> {
    outbe_tributefactory::worldwideday::init_genesis_day(ctx)
}

pub(crate) fn create_lookahead_days(
    _metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    outbe_tributefactory::worldwideday::create_lookahead_days(ctx, timestamp)
}

pub(crate) fn compute_wwd_schedule(wwd: WorldwideDay) -> Result<(NewWwdSchedule, u64)> {
    outbe_tributefactory::worldwideday::compute_wwd_schedule(wwd)
}

pub(crate) fn ensure_emission_day_with_limit(
    metadosis: &mut MetadosisContract,
    _ctx: &BlockRuntimeContext,
    day: u32,
    emission_limit_minor: U256,
) -> Result<()> {
    let limit = u128::try_from(emission_limit_minor)
        .map_err(|_| outbe_emission::errors::EmissionError::AmountOverflow)?;
    let emission_days = metadosis.emission_days();
    if let Some(mut existing) = emission_days.get(day)? {
        existing.emission_limit = limit;
        existing.metadosis_limit = limit;
        emission_days.update(&existing)?;
        return Ok(());
    }
    let wwd = WorldwideDay::new(day);
    let (schedule, _) = compute_wwd_schedule(wwd)?;
    let next_day = next_date_key(day).map_err(date_step_error)?;
    let metadosis_start = date_key_to_utc_timestamp(next_day)
        .checked_add(1)
        .ok_or(MetadosisCallerError::WaitingWindowOverflow)?;
    emission_days.create(&EmissionDay::new_with_limit(
        day,
        wwd,
        metadosis_start,
        limit,
        emission_day_state::NONE,
    ))?;
    let mut factory = TributeFactoryContract::new(metadosis.storage.clone());
    if factory.worldwide_days.get(wwd)?.is_none() {
        create_worldwide_day(&mut factory, wwd, NewWwdSchedule { day, ..schedule })?;
    }
    Ok(())
}

pub(crate) fn advance_active_worldwide_days(ctx: &BlockRuntimeContext) -> Result<()> {
    outbe_tributefactory::worldwideday::advance_active_worldwide_days(ctx)
}

pub(crate) fn find_due_emission_day(
    metadosis: &MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
) -> Result<Option<EmissionDay>> {
    let now = ctx.block.timestamp;
    let candidate_days = collect_due_candidate_days(metadosis, ctx, now)?;
    let emission_days = metadosis.emission_days();
    let factory = TributeFactoryContract::new(ctx.storage.clone());
    for day in candidate_days {
        let Some(record) = emission_days.get(day)? else {
            continue;
        };
        verify_record_wwd_backlink(&factory, &record)?;
        if record.metadosis_state == emission_day_state::NONE && now >= record.metadosis_start {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn collect_due_candidate_days(
    metadosis: &MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    now: u64,
) -> Result<BTreeSet<u32>> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    let mut candidate_days = BTreeSet::new();
    for wwd_record in aggregate.active_records() {
        verify_active_wwd_link(metadosis, wwd_record.day, wwd_record.worldwide_day)?;
        candidate_days.insert(wwd_record.day);
    }

    let today = timestamp_to_date_key(now);
    let prev_utc_day = previous_date_key(today).map_err(date_step_error)?;
    candidate_days.insert(prev_utc_day);
    let prev_ts = date_key_to_utc_timestamp(prev_utc_day);
    for offset in 1..=MAX_DUE_SCAN_DAYS {
        let scan_ts = prev_ts.saturating_sub(u64::from(offset) * SECONDS_PER_DAY);
        candidate_days.insert(timestamp_to_date_key(scan_ts));
    }
    Ok(candidate_days)
}

fn verify_active_wwd_link(
    metadosis: &MetadosisContract<'_>,
    day: u32,
    expected_wwd: WorldwideDay,
) -> Result<()> {
    let Some(record) = metadosis.emission_days().get(day)? else {
        return Ok(());
    };
    if record.wwd != expected_wwd {
        return Err(
            MetadosisStorageCorruptionError::EmissionDayWwdLinkMismatch {
                day,
                expected_wwd,
                found_wwd: record.wwd,
            }
            .into(),
        );
    }
    Ok(())
}

fn verify_record_wwd_backlink(
    factory: &TributeFactoryContract<'_>,
    record: &EmissionDay,
) -> Result<()> {
    if record.wwd.value() == 0 {
        return Ok(());
    }
    let Some(wwd_record) = factory.worldwide_days.get(record.wwd)? else {
        return Ok(());
    };
    if wwd_record.emission_day != record.day {
        return Err(
            MetadosisStorageCorruptionError::EmissionDayWwdLinkMismatch {
                day: record.day,
                expected_wwd: record.wwd,
                found_wwd: WorldwideDay::new(wwd_record.emission_day),
            }
            .into(),
        );
    }
    Ok(())
}

pub(crate) fn complete_emission_day(
    metadosis: &mut MetadosisContract<'_>,
    emission_day: &EmissionDay,
    state: u8,
) -> Result<()> {
    let mut updated = *emission_day;
    updated.metadosis_state = state;
    if state == emission_day_state::SUCCEEDED {
        updated.state = outbe_emission::schema::emission_day_state::COMPLETED;
    }
    metadosis.emission_days().update(&updated)?;
    let mut factory = TributeFactoryContract::new(metadosis.storage.clone());
    remove_active_worldwide_day(&mut factory, emission_day.wwd)?;
    Ok(())
}

pub(crate) fn fail_emission_day(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    day: u32,
    wwd: WorldwideDay,
    reason: &str,
) -> Result<()> {
    let mut tribute = TributeContract::new(metadosis.storage.clone());
    if !tribute.get_day_totals(wwd)?.is_sealed {
        tribute.seal_day(wwd)?;
    }
    let emission_days = metadosis.emission_days();
    if let Some(mut record) = emission_days.get(day)? {
        let limit_minor = record.emission_limit_minor();
        if !limit_minor.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone()).checked_credit(limit_minor)?;
            record.promis_limit = record
                .promis_limit
                .checked_add(record.metadosis_limit)
                .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
        }
        record.metadosis_state = emission_day_state::FAILED;
        emission_days.update(&record)?;
    }
    let mut factory = TributeFactoryContract::new(metadosis.storage.clone());
    remove_active_worldwide_day(&mut factory, wwd)?;
    metadosis.emit(IMetadosis::MetadosisSkipped {
        worldwideDay: wwd.into(),
        reason: reason.into(),
        status: "FAILED".into(),
        blockNumber: ctx.block.block_number,
    })
}
