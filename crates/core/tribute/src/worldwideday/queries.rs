use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::precompile::ITribute;
use crate::schema::TributeContract;

use super::{
    aggregate::{WwdDayType, WwdStatus},
    errors::WwdCallerError,
};

pub(crate) fn worldwide_day(
    tribute: &TributeContract<'_>,
    c: ITribute::getWorldwideDayCall,
) -> Result<<ITribute::getWorldwideDayCall as SolCall>::Return> {
    let Some(day) = tribute.worldwide_days.get(c.wwd.into())? else {
        return Err(outbe_primitives::storage::dsl::missing_record_err(
            "WorldwideDay",
        ));
    };
    // This function validates the persisted bytes as closed tags before it
    // returns them through the raw ABI representation.
    WwdStatus::try_from(day.status)?;
    WwdDayType::try_from(day.day_type)?;
    Ok((
        day.status,
        day.day_type,
        day.forming_start,
        day.forming_end,
        day.lookback_end,
        day.offering_end,
        day.scheduled_process_time,
        day.previous_vwap_minor,
        day.current_vwap_minor,
    )
        .into())
}

pub(crate) fn worldwide_days_by_status(
    tribute: &TributeContract<'_>,
    c: ITribute::getWorldwideDaysByStatusCall,
) -> Result<<ITribute::getWorldwideDaysByStatusCall as SolCall>::Return> {
    let wanted =
        WwdStatus::try_from(c.status).map_err(|_| WwdCallerError::UnknownWorldwideDayStatus)?;
    let wwds = tribute.get_active_wwd_by_status(wanted)?;
    Ok(wwds.into_iter().map(u32::from).collect())
}
