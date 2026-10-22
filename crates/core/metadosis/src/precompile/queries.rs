use super::IMetadosis;
use crate::schema::MetadosisContract;
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

pub(super) fn emission_day(
    metadosis: &MetadosisContract<'_>,
    c: IMetadosis::getEmissionDayCall,
) -> Result<<IMetadosis::getEmissionDayCall as SolCall>::Return> {
    let Some(record) = metadosis.emission_days.get(c.day)? else {
        return Err(outbe_primitives::storage::dsl::missing_record_err(
            "EmissionDay",
        ));
    };
    Ok((
        record.day,
        record.emission_limit_minor,
        record.wwd.value(),
        record.metadosis_start,
        record.state,
    )
        .into())
}
