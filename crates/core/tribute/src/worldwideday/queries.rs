use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::precompile::ITribute;
use crate::schema::TributeContract;

pub(crate) fn worldwide_day(
    tribute: &TributeContract<'_>,
    c: ITribute::getWorldwideDayCall,
) -> Result<<ITribute::getWorldwideDayCall as SolCall>::Return> {
    let Some(day) = tribute.worldwide_days.get(c.wwd.into())? else {
        return Err(outbe_primitives::storage::dsl::missing_record_err(
            "WorldwideDay",
        ));
    };
    Ok((
        day.day,
        day.forming_start,
        day.forming_end,
        day.offerings_start,
        day.offering_end,
    )
        .into())
}
