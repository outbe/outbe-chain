//! Private outer-WWD lifecycle transitions and their ordered effects.

mod advance;
mod effects;
pub(crate) use advance::advance_active_worldwide_days;

use crate::{
    aggregate::ValidatedWwdAggregate,
    commit::{commit_new_wwd, commit_outer_transition, NewWwdSchedule},
    ocomp::schema::require_active_ocomp_profile,
    reducer::{reduce_outer_wwd, OuterWwdEvent},
    schema::{MetadosisContract, WorldwideDayEntryExt},
};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, time::WorldwideDay};

pub(crate) fn validate_metadosis_timestamp(timestamp: u64) -> Result<()> {
    timestamp
        .checked_add(outbe_primitives::time::UTC_PLUS_14_OFFSET)
        .ok_or_else(|| crate::errors::caller_rejection("Metadosis UTC+14 timestamp overflow"))?;
    Ok(())
}

pub(crate) fn init_genesis_day(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    require_active_ocomp_profile(&metadosis)?;
    init_genesis_day_inner(&mut metadosis, ctx)
}

pub(crate) fn init_genesis_day_inner(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
) -> Result<()> {
    create_initial_worldwide_day_if_needed(metadosis, ctx, ctx.block.timestamp)?;
    initialize_bootstrap_if_needed(metadosis)
}

/// The bootstrap runs until the first Worldwide Day opens its offering, so the
/// boundary is that day's own schedule rather than a duration beside it.
fn initialize_bootstrap_if_needed(metadosis: &mut MetadosisContract) -> Result<()> {
    if metadosis.get_bootstrap_end_time()? != 0 {
        return Ok(());
    }
    let active = metadosis.active_wwd.read_all()?;
    let first = *active.first().ok_or_else(|| {
        crate::errors::storage_corruption(
            "Metadosis bootstrap needs the genesis Worldwide Day".into(),
        )
    })?;
    let record = metadosis.worldwide_days.get(first)?.ok_or_else(|| {
        crate::errors::storage_corruption("genesis Worldwide Day record disappeared".into())
    })?;
    metadosis.set_bootstrap_end_time(record.lookback_end)
}

fn create_initial_worldwide_day_if_needed(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    if !metadosis.active_wwd.is_empty()? {
        return Ok(());
    }
    create_worldwide_day_if_needed(metadosis, ctx, timestamp)
}

pub(crate) fn create_worldwide_day_if_needed(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    timestamp: u64,
) -> Result<()> {
    validate_metadosis_timestamp(timestamp)?;
    let wwd = WorldwideDay::from_timestamp(timestamp);
    create_worldwide_day_for_date(metadosis, ctx, wwd)
}

pub(crate) fn create_worldwide_day_for_date(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> Result<()> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(metadosis.storage.clone())?;
    let existing_forming_start = metadosis.worldwide_days.entry(wwd).forming_start().read()?;
    if existing_forming_start != 0 {
        let current = aggregate.record(wwd).ok_or_else(|| {
            crate::errors::storage_corruption(format!(
                "Metadosis WWD {wwd} record exists outside validated membership"
            ))
        })?;
        let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::CreateDay)?;
        return commit_outer_transition(metadosis, wwd, &transition, ctx.block.block_number);
    }

    aggregate.ensure_can_insert_active(wwd)?;
    let forming_start = wwd.start_timestamp();
    let transition = reduce_outer_wwd(None, OuterWwdEvent::CreateDay)?;
    commit_new_wwd(
        metadosis,
        wwd,
        NewWwdSchedule {
            forming_start,
            forming_period_seconds: outbe_chain_constants::get_metadosis_forming_period_seconds(),
            lookback_delay_seconds: outbe_chain_constants::get_metadosis_lookback_delay_seconds(),
            offering_period_seconds: outbe_chain_constants::get_metadosis_offering_period_seconds(),
            waiting_period_seconds: outbe_chain_constants::get_metadosis_waiting_period_seconds(),
        },
        &transition,
    )
}
