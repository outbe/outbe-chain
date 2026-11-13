//! EmissionDay and WorldwideDay lifecycle orchestration.

use std::collections::BTreeSet;

use alloy_primitives::U256;
use outbe_emission::schema::metadosis_state;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    time::{
        date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key, WorldwideDay,
        SECONDS_PER_DAY,
    },
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;
use outbe_tributefactory::TributeFactoryContract;

use crate::{
    errors::{MetadosisCallerError, MetadosisStorageCorruptionError},
    precompile::IMetadosis,
    schema::{EmissionDay, MetadosisContract},
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
    let Some(mut existing) = emission_days.get(day)? else {
        return Err(outbe_emission::errors::EmissionError::EmissionDayNotFound.into());
    };
    existing.emission_limit = existing.emission_limit.max(limit);
    existing.metadosis_limit = limit;
    if existing.state == outbe_emission::schema::emission_day_state::PENDING {
        existing.state = outbe_emission::schema::emission_day_state::PROCESSING;
    }
    emission_days.update(&existing)
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
            && record.metadosis_state == metadosis_state::PENDING
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

pub(crate) fn complete_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    if !outbe_agentreward::distribution::has_pending_accruals_for_day(
        &metadosis.storage,
        emission_day.day,
    )? {
        outbe_emission::api::complete_emission_day(metadosis.storage.clone(), emission_day.day)?;
    }
    Ok(())
}

pub(crate) fn cancel_emission_day(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
) -> Result<()> {
    if metadosis.emission_days().get(day)?.is_some() {
        let record =
            outbe_emission::api::record_metadosis_canceled(metadosis.storage.clone(), day)?;
        if !outbe_agentreward::distribution::has_pending_accruals_for_day(
            &metadosis.storage,
            record.day,
        )? {
            outbe_emission::api::complete_emission_day(metadosis.storage.clone(), record.day)?;
        }
    }
    metadosis.emit(IMetadosis::MetadosisCanceled {
        emissionDay: day,
        reason,
    })
}

pub(crate) fn fail_metadosis(
    metadosis: &mut MetadosisContract<'_>,
    day: u32,
    reason: u8,
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
    metadosis.emit(IMetadosis::MetadosisFailed {
        emissionDay: day,
        reason,
    })
}
