use alloy_primitives::B256;
use outbe_intex::precompile::IIntex;
use outbe_intex::IntexContract;

use crate::errors::IntexFactoryError;
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

/// Force-call a whole group: its series count from the same day against the same call
/// price, so its first member decides them all. Returns how many series were called.
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
    let series = outbe_intex::api::read_series(storage, first)?;
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
    let called_at =
        u32::try_from(now_ts).map_err(|_| IntexFactoryError::BlockTimestampExceedsU32)?;
    let settlement_deadline = series.call_stamp(called_at).deadline;
    intex.record_group_called(group, called_at, settlement_deadline)?;

    // The notices leave after the slice, once every group of the block is decided.
    for &series_id in &members {
        crate::notify::enqueue_notice(
            factory,
            crate::notify::pack_called_notice(series_id, called_at),
        )?;
    }

    intex.emit(IIntex::IntexGroupCalled {
        groupKey: group,
        seriesIds: members.iter().map(|id| (*id).into()).collect(),
        calledAt: called_at,
        settlementDeadline: settlement_deadline,
    })?;
    Ok(members.len() as u32)
}
