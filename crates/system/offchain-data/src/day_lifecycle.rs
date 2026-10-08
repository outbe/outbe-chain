//! Tribute retirement removes the day's database. Nod for that day stays.

use outbe_offchain_storage::AtomicWriteBatch;
use outbe_tribute::{list_tribute_day_marks, tribute_day_mark_operation, TributeDayMark};

use super::{DayDatabaseRoute, DayRetirement, ProjectionError};

pub(super) fn sweep(route: &DayDatabaseRoute) -> Result<(), ProjectionError> {
    for (day, mark) in list_tribute_day_marks(route.durable_reader.as_ref())? {
        match mark {
            TributeDayMark::DropPending | TributeDayMark::Retired => {
                route.databases.forget_tribute_day(day)?;
                route.databases.directory().drop_tribute_day(day)?;
            }
            TributeDayMark::Retained(_) => {}
        }
    }
    Ok(())
}

pub(super) fn finish_retirements(
    route: &DayDatabaseRoute,
    retirements: &[DayRetirement],
    mut shared: AtomicWriteBatch,
    state: AtomicWriteBatch,
) -> Result<AtomicWriteBatch, ProjectionError> {
    for retirement in retirements {
        if let DayRetirement::Drop(day) = retirement {
            commit_drop(route, *day)?;
        }
    }
    for retirement in retirements {
        let mark = match retirement {
            DayRetirement::Drop(day) => (*day, TributeDayMark::Retired),
            DayRetirement::Retain { day, lease } => (*day, TributeDayMark::Retained(*lease)),
        };
        shared.push(tribute_day_mark_operation(mark.0, mark.1)?);
    }
    shared.extend(state.operations().iter().cloned());
    shared.validate()?;
    Ok(shared)
}

fn commit_drop(route: &DayDatabaseRoute, day: u32) -> Result<(), ProjectionError> {
    let current = outbe_tribute::read_tribute_day_mark(route.durable_reader.as_ref(), day)?;
    if !matches!(
        current,
        Some(TributeDayMark::DropPending) | Some(TributeDayMark::Retired)
    ) {
        let batch = AtomicWriteBatch::from_operations(vec![tribute_day_mark_operation(
            day,
            TributeDayMark::DropPending,
        )?]);
        batch.validate()?;
        route.durable_writer.as_ref().apply_atomic(&batch)?;
    }
    route.databases.forget_tribute_day(day)?;
    route.databases.directory().drop_tribute_day(day)?;
    Ok(())
}

pub(super) fn finish_shared_retirements(
    enabled: bool,
    retirements: &[DayRetirement],
    mut batch: AtomicWriteBatch,
    state: AtomicWriteBatch,
) -> Result<AtomicWriteBatch, ProjectionError> {
    if enabled {
        for retirement in retirements {
            let (day, mark) = match retirement {
                DayRetirement::Drop(day) => (*day, TributeDayMark::Retired),
                DayRetirement::Retain { day, lease } => (*day, TributeDayMark::Retained(*lease)),
            };
            batch.push(tribute_day_mark_operation(day, mark)?);
            batch.retire_scope(outbe_tribute::partitioning::day_scope(day)?);
        }
    }
    batch.extend(state.operations().iter().cloned());
    batch.validate()?;
    Ok(batch)
}
