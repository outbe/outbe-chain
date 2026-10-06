use std::collections::BTreeMap;

use alloy_primitives::U256;
use outbe_intex::IntexState;
use outbe_oracle::schema::{OracleContract, PairIndex};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::{first_full_day, previous_date_key, SECONDS_PER_DAY},
};

use crate::schema::IntexFactoryContract;
use crate::state::Group;

/// Finalized per-day VWAPs of one oracle pair, read once per scan.
pub(crate) struct DayVwaps {
    pair_index: PairIndex,
    days: BTreeMap<u32, Option<U256>>,
}

impl DayVwaps {
    pub(crate) fn new(pair_index: PairIndex) -> Self {
        Self {
            pair_index,
            days: BTreeMap::new(),
        }
    }

    fn get(&mut self, oracle: &OracleContract, day: u32) -> Result<Option<U256>> {
        if let Some(v) = self.days.get(&day) {
            return Ok(*v);
        }
        let v = oracle.get_utc_day_vwap_for_pair(day, self.pair_index)?;
        self.days.insert(day, v);
        Ok(v)
    }

    /// Breach-days (VWAP > trigger) inside the window, not before issuance.
    fn count_breaches(
        &mut self,
        oracle: &OracleContract,
        last_day: u32,
        days: u32,
        issued_day: u32,
        trigger: U256,
    ) -> Result<u32> {
        let mut breaches: u32 = 0;
        let mut day = last_day;
        for _ in 0..days {
            if day < issued_day {
                break;
            }
            if let Some(vwap) = self.get(oracle, day)? {
                if vwap > trigger {
                    breaches += 1;
                }
            }
            day = previous_date_key(day);
        }
        Ok(breaches)
    }
}

/// The finalized VWAP window one call scan decides against, and the price that
/// summarises it.
pub(crate) struct CallWindow {
    /// Most recent fully-closed UTC day. The window ends here.
    pub(crate) last_day: u32,
    /// Window length and required breach count, both in whole days.
    pub(crate) days: u32,
    pub(crate) threshold: u32,
    /// The `threshold`-th largest VWAP: `trigger < p_star` and "breached on at
    /// least `threshold` days" are one statement, so a group decides by comparison.
    pub(crate) p_star: U256,
    /// The window's first day. A group issued on or before it sees the whole window.
    pub(crate) first_day: u32,
}

/// The window's `threshold`-th largest finalized VWAP. `None` when too few days
/// carry a price for any trigger to be breached often enough.
pub(crate) fn call_window(
    oracle: &OracleContract,
    vwaps: &mut DayVwaps,
    last_day: u32,
    days: u32,
    threshold: u32,
) -> Result<Option<CallWindow>> {
    if days == 0 || threshold == 0 {
        return Ok(None);
    }
    let mut priced: Vec<U256> = Vec::with_capacity(days as usize);
    let mut day = last_day;
    for _ in 0..days {
        if let Some(vwap) = vwaps.get(oracle, day)? {
            priced.push(vwap);
        }
        day = previous_date_key(day);
    }
    if (priced.len() as u32) < threshold {
        return Ok(None);
    }
    priced.sort_unstable_by(|a, b| b.cmp(a));
    let mut first_day = last_day;
    for _ in 1..days {
        first_day = previous_date_key(first_day);
    }
    Ok(Some(CallWindow {
        last_day,
        days,
        threshold,
        p_star: priced[threshold as usize - 1],
        first_day,
    }))
}

/// What one group's call reads and writes.
pub(crate) struct GroupCall<'a, 'storage> {
    pub(crate) storage: &'a StorageHandle<'storage>,
    pub(crate) factory: &'a mut IntexFactoryContract<'storage>,
    pub(crate) oracle: &'a OracleContract<'storage>,
    pub(crate) vwaps: &'a mut DayVwaps,
}

/// Force-call a whole group: its series share trigger, issue time and call
/// parameters, so one read decides them all. Returns how many were called.
pub(crate) fn try_call_group(
    call: GroupCall<'_, '_>,
    group: &Group,
    window: &CallWindow,
    now_ts: u64,
) -> Result<u32> {
    let GroupCall {
        storage,
        factory,
        oracle,
        vwaps,
    } = call;
    let Some(&first) = group.members.first() else {
        return Ok(0);
    };
    let series = outbe_intex::api::read_series(storage, first)?;
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(0);
    }
    let trigger = series.call_price_minor;
    // The scan walks finalized daily VWAPs, so both bounds floor to whole days.
    let secs_per_day = SECONDS_PER_DAY as u32;
    let group_days = series.call_window_seconds / secs_per_day;
    let group_threshold = series.call_threshold_seconds / secs_per_day;
    if group_days == 0 || group_threshold == 0 {
        return Ok(0);
    }

    let issued_day = first_full_day(u64::from(series.issued_at));
    let breached = if issued_day <= window.first_day
        && group_days == window.days
        && group_threshold == window.threshold
    {
        trigger < window.p_star
    } else {
        // The group has a shorter window than the scan's: it was issued inside it, or it
        // has different stored parameters. So the scan counts its own days. Wider stored
        // parameters are only reached under `p_star`. Only a profile change on a live
        // chain parts them.
        vwaps.count_breaches(oracle, window.last_day, group_days, issued_day, trigger)?
            >= group_threshold
    };
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
    // Park it with its members: the expiry sweep has no other way back to them.
    factory.remove_call_bin_group(group.iso_code, group.worldwide_day)?;
    factory.push_called_group(
        group.iso_code,
        group.worldwide_day,
        settlement_deadline,
        &group.members,
    )?;

    // A slice of this sweep runs in a block hook, which cannot call contracts. So the notices
    // leave from the `intex_drain_notices` trigger. Each notice carries its own series: the group
    // has left the index by then.
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
