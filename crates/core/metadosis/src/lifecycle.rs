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
use outbe_tributefactory::TributeFactoryContract;

use crate::{
    errors::{MetadosisCallerError, MetadosisStorageCorruptionError},
    precompile::IMetadosis,
    schema::{emission_day_state, EmissionDay, MetadosisContract},
};

const MAX_DUE_SCAN_DAYS: u32 = 64;

pub(crate) fn date_step_error(
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

pub(crate) fn ensure_emission_day_with_limit(
    metadosis: &mut MetadosisContract,
    _ctx: &BlockRuntimeContext,
    day: u32,
    emission_limit_minor: U256,
) -> Result<()> {
    let limit = outbe_emission::api::to_u128_checked(emission_limit_minor)?;
    let emission_days = metadosis.emission_days();
    if let Some(mut existing) = emission_days.get(day)? {
        existing.emission_limit = existing.emission_limit.max(limit);
        existing.metadosis_limit = limit;
        if existing.state == outbe_emission::schema::emission_day_state::PENDING
            || existing.state == outbe_emission::schema::emission_day_state::SEALED
        {
            existing.state = outbe_emission::schema::emission_day_state::PROCESSING;
        }
        emission_days.update(&existing)?;
        return Ok(());
    }
    let next_day = next_date_key(day).map_err(date_step_error)?;
    let metadosis_start = date_key_to_utc_timestamp(next_day)
        .map_err(date_step_error)?
        .checked_add(1)
        .ok_or(MetadosisCallerError::WaitingWindowOverflow)?;
    let mut record = EmissionDay::new_with_limit(
        day,
        WorldwideDay::new(0),
        metadosis_start,
        limit,
        emission_day_state::NONE,
    );
    record.state = outbe_emission::schema::emission_day_state::PROCESSING;
    emission_days.create(&record)?;
    Ok(())
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
        if record.state == outbe_emission::schema::emission_day_state::PROCESSING
            && record.metadosis_state == emission_day_state::NONE
            && now >= record.metadosis_start
        {
            if record.wwd.value() != 0
                && !TributeContract::new(ctx.storage.clone()).is_day_sealed(record.wwd)?
            {
                continue;
            }
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
    let factory = TributeFactoryContract::new(ctx.storage.clone());
    let mut candidate_days = BTreeSet::new();
    for wwd in factory.pipeline_wwd.read_all()? {
        let Some(wwd_record) = factory.worldwide_days.get(wwd)? else {
            continue;
        };
        verify_active_wwd_link(metadosis, wwd_record.emission_day, wwd_record.wwd)?;
        candidate_days.insert(wwd_record.emission_day);
    }

    let today = timestamp_to_date_key(now);
    let prev_utc_day = previous_date_key(today).map_err(date_step_error)?;
    candidate_days.insert(prev_utc_day);
    let prev_ts = date_key_to_utc_timestamp(prev_utc_day).map_err(date_step_error)?;
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
    if state == emission_day_state::SUCCEEDED {
        if !outbe_agentreward::distribution::has_pending_accruals_for_day(
            &metadosis.storage,
            emission_day.day,
        )? {
            outbe_emission::api::complete_emission_day(
                metadosis.storage.clone(),
                emission_day.day,
            )?;
        }
    } else {
        let mut updated = *emission_day;
        updated.metadosis_state = state;
        metadosis.emission_days().update(&updated)?;
    }
    Ok(())
}

pub(crate) fn fail_emission_day(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    day: u32,
    wwd: WorldwideDay,
    reason: &str,
) -> Result<()> {
    if metadosis.emission_days().get(day)?.is_some() {
        let (record, limit_minor) =
            outbe_emission::api::record_metadosis_failed(metadosis.storage.clone(), day)?;
        if !limit_minor.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone()).checked_credit(limit_minor)?;
        }
        if !outbe_agentreward::distribution::has_pending_accruals_for_day(
            &metadosis.storage,
            record.day,
        )? {
            outbe_emission::api::complete_emission_day(metadosis.storage.clone(), record.day)?;
        }
    }
    metadosis.emit(IMetadosis::MetadosisSkipped {
        worldwideDay: wwd.into(),
        reason: reason.into(),
        status: "FAILED".into(),
        blockNumber: ctx.block.block_number,
    })
}
