use outbe_intex::IntexState;
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{
    call_breach::BreachTerms,
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::first_full_day,
};

use crate::schema::IntexFactoryContract;
use crate::state::Group;

/// What one group's call reads and writes.
pub(crate) struct GroupCall<'a, 'storage> {
    pub(crate) storage: &'a StorageHandle<'storage>,
    pub(crate) factory: &'a mut IntexFactoryContract<'storage>,
}

/// Force-call a whole group: its series share trigger, issue time and call
/// parameters, so one read decides them all. Returns how many were called.
pub(crate) fn try_call_group(
    call: GroupCall<'_, '_>,
    group: &Group,
    window: &CallWindow,
    now_ts: u64,
) -> Result<u32> {
    let GroupCall { storage, factory } = call;
    let Some(&first) = group.members.first() else {
        return Ok(0);
    };
    let series = outbe_intex::api::read_series(storage, first)?;
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(0);
    }
    let breached = window.breached(&BreachTerms {
        call_price: series.call_price_minor,
        window_seconds: series.call_window_seconds,
        threshold_seconds: series.call_threshold_seconds,
        start_day: first_full_day(u64::from(series.issued_at)),
    });
    if !breached {
        return Ok(0);
    }

    // u32 timestamp. It is bounded until 2106 (matches issued_at).
    let called_at = u32::try_from(now_ts)
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;
    for &series_id in &group.members {
        outbe_intex::api::mark_called(storage, series_id, called_at)?;
    }
    let settlement_deadline = u64::from(called_at) + u64::from(series.call_notice_period_seconds);
    factory.remove_call_bin_group(group.iso_code, group.worldwide_day)?;
    factory.push_called_group(group.iso_code, group.worldwide_day, settlement_deadline)?;

    // The notices leave after the slice, once every group of the block is decided. Each
    // notice carries its own series: members may have expired by the time it is sent.
    for &series_id in &group.members {
        crate::notify::enqueue_notice(
            factory,
            crate::notify::pack_called_notice(series_id, called_at),
        )?;
    }

    for &series_id in &group.members {
        crate::runtime::emit_event(
            storage,
            crate::precompile::IIntexFactory::SeriesCalled {
                seriesId: series_id.into(),
                calledAt: called_at,
                settlementDeadline: settlement_deadline,
            },
        )?;
    }
    Ok(group.members.len() as u32)
}
