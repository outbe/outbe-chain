use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::constants::METADOSIS_CANCELED_NO_LIMIT;
use crate::lifecycle::{self, cancel_metadosis};
use crate::schema::{EmissionDay, MetadosisContract};

pub fn process_emission_day(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    lifecycle::validate_metadosis_timestamp(ctx.block.timestamp)?;
    if let Some(record) = lifecycle::ready_emission_day(&metadosis, ctx, day)? {
        run_ready_emission_day(&mut metadosis, ctx, &record)?;
    }
    Ok(())
}

pub(crate) fn run_ready_emission_day(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    let limit_amount = emission_day.metadosis_limit_minor();
    if limit_amount.is_zero() {
        return cancel_metadosis(metadosis, emission_day.day, METADOSIS_CANCELED_NO_LIMIT);
    }
    crate::loads::execute(metadosis, ctx, emission_day)
}
