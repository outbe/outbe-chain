use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::constants::METADOSIS_CANCELED_NO_LIMIT;
use crate::lifecycle::{self, cancel_emission_day};
use crate::schema::{EmissionDay, MetadosisContract};

pub fn start_metadosis(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;
    lifecycle::validate_metadosis_timestamp(timestamp)?;

    if let Some(due_day) = lifecycle::find_due_emission_day(&metadosis, ctx)? {
        process_ready_candidate(&mut metadosis, ctx, &due_day)?;
    }

    Ok(())
}

pub(crate) fn process_ready_candidate(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    let limit_amount = emission_day.metadosis_limit_minor();
    if limit_amount.is_zero() {
        return cancel_emission_day(metadosis, emission_day.day, METADOSIS_CANCELED_NO_LIMIT);
    }
    crate::lysis::execute(metadosis, ctx, emission_day)
}
