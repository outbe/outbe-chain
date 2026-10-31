use alloy_primitives::B256;
use outbe_intex::precompile::IIntex;
use outbe_intex::{IntexContract, SeriesRecord};

use crate::errors::date_step_error;
use outbe_oracle::call_window::CallWindow;
use outbe_primitives::{
    call_breach::BreachTerms, error::Result, storage::StorageHandle, time::first_full_day,
};

use crate::schema::IntexFactoryContract;

/// What one group's call reads and writes.
pub(crate) struct GroupCall<'a, 'storage> {
    pub(crate) storage: &'a StorageHandle<'storage>,
    pub(crate) intex: &'a mut IntexContract<'storage>,
    pub(crate) factory: &'a IntexFactoryContract<'storage>,
}

/// Force-call a whole group on the terms it sealed: its series count from the same day
/// against the same call price. Returns how many series were called.
pub(crate) fn call_if_breached(
    call: GroupCall<'_, '_>,
    group: B256,
    window: &CallWindow,
    now_ts: u64,
) -> Result<u32> {
    let GroupCall {
        storage,
        intex,
        factory,
    } = call;
    let members = intex.group_members(group)?;
    let Some(&first) = members.first() else {
        return Ok(0);
    };
    let series = outbe_intex::api::get_series(storage, first)?;
    if !window.breached(&breach_terms(intex, &series, group)?) {
        return Ok(0);
    }

    let stamp = intex.group_stamp(group, now_ts)?;
    intex.record_group_called(group, now_ts, stamp.close_at)?;

    // The notices leave after the slice, once every group of the block is decided.
    for &series_id in &members {
        crate::notify::enqueue_notice(factory, crate::notify::pack_call_notice(series_id, now_ts))?;
    }

    intex.emit(IIntex::IntexGroupCalled {
        groupKey: group,
        seriesIds: members.iter().map(|id| (*id).into()).collect(),
        calledAt: now_ts,
        settlementDeadline: stamp.deadline,
    })?;
    Ok(members.len() as u32)
}

/// The terms a group is decided on: the call price and start day its key carries, and
/// the window and threshold its first series sealed.
pub(crate) fn breach_terms(
    intex: &IntexContract<'_>,
    member: &SeriesRecord,
    group: B256,
) -> Result<BreachTerms> {
    Ok(BreachTerms {
        call_price: member.call_price_minor,
        window_seconds: intex.call_group_window_seconds.read(&group)?,
        threshold_seconds: intex.call_group_threshold_seconds.read(&group)?,
        start_day: first_full_day(member.issued_at).map_err(date_step_error)?,
    })
}
