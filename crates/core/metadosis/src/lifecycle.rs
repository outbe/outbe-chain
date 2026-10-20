//! EmissionDay and WorldwideDay lifecycle orchestration.

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    emission::metadosis_day_emission_limit,
    error::Result,
    time::{
        date_key_to_utc_timestamp, day_number_between, first_full_day, next_date_key,
        previous_date_key, timestamp_to_date_key, WorldwideDay, SECONDS_PER_DAY,
    },
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::{
    worldwideday::{
        create_worldwide_day, remove_active_worldwide_day, NewWwdSchedule, ValidatedWwdAggregate,
    },
    TributeContract,
};

use crate::{
    errors::{is_vwap_overflow, MetadosisCallerError, MetadosisStorageCorruptionError},
    precompile::IMetadosis,
    schema::{emission_day_state, EmissionDay, MetadosisContract},
};

pub(crate) fn validate_metadosis_timestamp(timestamp: u64) -> Result<()> {
    timestamp
        .checked_add(outbe_primitives::time::UTC_PLUS_14_OFFSET)
        .ok_or(MetadosisCallerError::UtcPlus14TimestampOverflow)?;
    Ok(())
}

pub(crate) fn init_genesis_day(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    init_genesis_day_inner(&mut metadosis, ctx)
}

pub(crate) fn init_genesis_day_inner(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
) -> Result<()> {
    create_lookahead_days(metadosis, ctx, ctx.block.timestamp)?;
    initialize_bootstrap_if_needed(metadosis)
}

fn initialize_bootstrap_if_needed(metadosis: &mut MetadosisContract) -> Result<()> {
    if metadosis.bootstrap_end_time.read()? != 0 {
        return Ok(());
    }
    let tribute = TributeContract::new(metadosis.storage.clone());
    let active = tribute.active_wwd.read_all()?;
    let first = *active
        .first()
        .ok_or(MetadosisStorageCorruptionError::BootstrapNeedsGenesisWorldwideDay)?;
    let record = tribute
        .worldwide_days
        .get(first)?
        .ok_or(MetadosisStorageCorruptionError::GenesisWorldwideDayRecordDisappeared)?;
    metadosis.bootstrap_end_time.write(record.offerings_start)
}

pub(crate) fn create_lookahead_days(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    validate_metadosis_timestamp(timestamp)?;
    let base_wwd = WorldwideDay::from_timestamp(timestamp);
    let base_ts = base_wwd.to_timestamp_utc();
    let lookahead = outbe_chain_constants::get_metadosis_lookahead_days();
    for offset in 0..lookahead {
        let wwd_ts = base_ts.saturating_add(u64::from(offset) * SECONDS_PER_DAY);
        let wwd = WorldwideDay::new(timestamp_to_date_key(wwd_ts));
        create_worldwide_day_for_date(metadosis, ctx, wwd)?;
    }
    Ok(())
}

pub(crate) fn compute_wwd_schedule(wwd: WorldwideDay) -> Result<(NewWwdSchedule, u64)> {
    let forming_start = wwd.start_timestamp();
    let forming_end = forming_start
        .checked_add(outbe_chain_constants::get_metadosis_forming_period_seconds())
        .ok_or(MetadosisCallerError::FormingWindowOverflow)?;
    let offerings_start = forming_end
        .checked_add(outbe_chain_constants::get_metadosis_lookback_delay_seconds())
        .ok_or(MetadosisCallerError::LookbackWindowOverflow)?;
    let offering_end = offerings_start
        .checked_add(outbe_chain_constants::get_metadosis_offering_period_seconds())
        .ok_or(MetadosisCallerError::OfferingWindowOverflow)?;
    let day = first_full_day(offering_end);
    let metadosis_start = date_key_to_utc_timestamp(next_date_key(day))
        .checked_add(1)
        .ok_or(MetadosisCallerError::WaitingWindowOverflow)?;
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

fn default_emission_limit_for_day(ctx: &BlockRuntimeContext, day: u32) -> U256 {
    let anchor_utc_day = timestamp_to_date_key(ctx.block.timestamp);
    let day_number = day_number_between(anchor_utc_day, day).unwrap_or(0);
    metadosis_day_emission_limit(day_number)
}

pub(crate) fn create_worldwide_day_for_date(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> Result<()> {
    let (schedule, metadosis_start) = compute_wwd_schedule(wwd)?;
    let day = schedule.day;
    if let Some(existing_emission) = metadosis.emission_days.get(day)? {
        if existing_emission.wwd != wwd {
            return Err(
                MetadosisStorageCorruptionError::EmissionDayWwdLinkMismatch {
                    day,
                    expected_wwd: wwd,
                    found_wwd: existing_emission.wwd,
                }
                .into(),
            );
        }
    } else {
        let emission_limit_minor = default_emission_limit_for_day(ctx, day);
        metadosis.emission_days.create(&EmissionDay {
            day,
            emission_limit_minor,
            wwd,
            metadosis_start,
            state: emission_day_state::NONE,
        })?;
    }

    let mut tribute = TributeContract::new(metadosis.storage.clone());
    create_worldwide_day(&mut tribute, wwd, schedule)?;
    Ok(())
}

pub(crate) fn ensure_emission_day_with_limit(
    metadosis: &mut MetadosisContract,
    _ctx: &BlockRuntimeContext,
    day: u32,
    emission_limit_minor: U256,
) -> Result<()> {
    if let Some(mut existing) = metadosis.emission_days.get(day)? {
        existing.emission_limit_minor = emission_limit_minor;
        metadosis.emission_days.update(&existing)?;
        return Ok(());
    }
    let wwd = WorldwideDay::new(day);
    let (schedule, _) = compute_wwd_schedule(wwd)?;
    let metadosis_start = date_key_to_utc_timestamp(next_date_key(day))
        .checked_add(1)
        .ok_or(MetadosisCallerError::WaitingWindowOverflow)?;
    metadosis.emission_days.create(&EmissionDay {
        day,
        emission_limit_minor,
        wwd,
        metadosis_start,
        state: emission_day_state::NONE,
    })?;
    let mut tribute = TributeContract::new(metadosis.storage.clone());
    if tribute.worldwide_days.get(wwd)?.is_none() {
        create_worldwide_day(&mut tribute, wwd, NewWwdSchedule { day, ..schedule })?;
    }
    Ok(())
}

pub(crate) fn advance_active_worldwide_days(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    let now = ctx.block.timestamp;

    for current in aggregate.active_records() {
        advance_single_worldwide_day(&mut metadosis, ctx, current, now)?;
    }
    Ok(())
}

fn advance_single_worldwide_day(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &outbe_tribute::worldwideday::WwdProjection,
    now: u64,
) -> Result<()> {
    let wwd = current.worldwide_day;
    verify_emission_day_link(metadosis, current.day, wwd)?;

    if now >= current.forming_end && !snapshot_forming_vwap(metadosis, ctx, current)? {
        return Ok(());
    }

    sync_offering_window_seal(ctx, current, now)
}

fn verify_emission_day_link(
    metadosis: &MetadosisContract<'_>,
    day: u32,
    expected_wwd: WorldwideDay,
) -> Result<()> {
    if let Some(emission_day) = metadosis.emission_days.get(day)? {
        if emission_day.wwd != expected_wwd {
            return Err(
                MetadosisStorageCorruptionError::EmissionDayWwdLinkMismatch {
                    day,
                    expected_wwd,
                    found_wwd: emission_day.wwd,
                }
                .into(),
            );
        }
    }
    Ok(())
}

fn snapshot_forming_vwap(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    current: &outbe_tribute::worldwideday::WwdProjection,
) -> Result<bool> {
    let wwd = current.worldwide_day;
    match outbe_oracle::api::store_worldwide_day_vwap_snapshot(
        ctx.storage.clone(),
        wwd,
        current.forming_start,
        current.forming_end,
    ) {
        Ok(_) => Ok(true),
        Err(error)
            if error.to_string().contains("VWAP must be non-zero") || is_vwap_overflow(&error) =>
        {
            let reason = if is_vwap_overflow(&error) {
                "VWAP calculation overflowed"
            } else {
                "VWAP is zero"
            };
            fail_emission_day(metadosis, ctx, current.day, wwd, reason)?;
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn sync_offering_window_seal(
    ctx: &BlockRuntimeContext<'_>,
    current: &outbe_tribute::worldwideday::WwdProjection,
    now: u64,
) -> Result<()> {
    let wwd = current.worldwide_day;
    let mut tribute = TributeContract::new(ctx.storage.clone());
    let totals = tribute.get_day_totals(wwd)?;
    if current.is_offering_at(now) {
        if totals.is_sealed && !tribute.retired_days.read(&wwd)? {
            tribute.unseal_day(wwd)?;
        }
    } else if now >= current.offering_end && !totals.is_sealed {
        tribute.seal_day(wwd)?;
    }
    Ok(())
}

pub(crate) fn find_due_emission_day(
    metadosis: &MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
) -> Result<Option<EmissionDay>> {
    let now = ctx.block.timestamp;
    let prev_utc_day = previous_date_key(timestamp_to_date_key(now));
    if let Some(record) = metadosis.emission_days.get(prev_utc_day)? {
        if record.state == emission_day_state::NONE && now >= record.metadosis_start {
            return Ok(Some(record));
        }
    }

    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    for wwd_record in aggregate.active_records() {
        if let Some(record) = metadosis.emission_days.get(wwd_record.day)? {
            if record.wwd != wwd_record.worldwide_day {
                return Err(
                    MetadosisStorageCorruptionError::EmissionDayWwdLinkMismatch {
                        day: wwd_record.day,
                        expected_wwd: wwd_record.worldwide_day,
                        found_wwd: record.wwd,
                    }
                    .into(),
                );
            }
            if record.state == emission_day_state::NONE && now >= record.metadosis_start {
                return Ok(Some(record));
            }
        }
    }
    Ok(None)
}

pub(crate) fn complete_emission_day(
    metadosis: &mut MetadosisContract<'_>,
    emission_day: &EmissionDay,
    state: u8,
) -> Result<()> {
    let updated = EmissionDay {
        day: emission_day.day,
        emission_limit_minor: emission_day.emission_limit_minor,
        wwd: emission_day.wwd,
        metadosis_start: emission_day.metadosis_start,
        state,
    };
    metadosis.emission_days.update(&updated)?;
    let mut tribute = TributeContract::new(metadosis.storage.clone());
    remove_active_worldwide_day(&mut tribute, emission_day.wwd)?;
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
    if let Some(mut record) = metadosis.emission_days.get(day)? {
        if !record.emission_limit_minor.is_zero() {
            PromisLimitContract::new(metadosis.storage.clone())
                .checked_add_carry_over(record.emission_limit_minor)?;
        }
        record.state = emission_day_state::FAILED;
        metadosis.emission_days.update(&record)?;
    }
    remove_active_worldwide_day(&mut tribute, wwd)?;
    metadosis.emit(IMetadosis::MetadosisSkipped {
        worldwideDay: wwd.into(),
        reason: reason.into(),
        status: "FAILED".into(),
        blockNumber: ctx.block.block_number,
    })
}
