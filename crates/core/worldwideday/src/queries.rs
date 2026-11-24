use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::precompile::IWorldwideDay;
use crate::schema::WorldwideDayContract;

pub(crate) fn worldwide_day(
    contract: &WorldwideDayContract<'_>,
    c: IWorldwideDay::getWorldwideDayCall,
) -> Result<<IWorldwideDay::getWorldwideDayCall as SolCall>::Return> {
    let Some(day) = contract.worldwide_days.get(c.wwd.into())? else {
        return Err(outbe_primitives::storage::dsl::missing_record_err(
            "WorldwideDay",
        ));
    };
    Ok((
        day.emission_day,
        day.forming_start()
            .map_err(crate::errors::date_step_error)?,
        day.forming_end().map_err(crate::errors::date_step_error)?,
        day.offering_start,
        day.offering_end,
    )
        .into())
}
