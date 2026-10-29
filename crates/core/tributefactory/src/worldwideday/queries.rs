use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::precompile::ITributeFactory;
use crate::schema::TributeFactoryContract;

pub(crate) fn worldwide_day(
    factory: &TributeFactoryContract<'_>,
    c: ITributeFactory::getWorldwideDayCall,
) -> Result<<ITributeFactory::getWorldwideDayCall as SolCall>::Return> {
    let Some(day) = factory.worldwide_days.get(c.wwd.into())? else {
        return Err(outbe_primitives::storage::dsl::missing_record_err(
            "WorldwideDay",
        ));
    };
    Ok((
        day.emission_day,
        day.forming_start(),
        day.forming_end(),
        day.offering_start,
        day.offering_end,
    )
        .into())
}
