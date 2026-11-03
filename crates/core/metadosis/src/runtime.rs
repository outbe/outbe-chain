use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::MetadosisContract;
use crate::{lifecycle, settlement::process_ready_candidate};

pub fn start_metadosis(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;
    lifecycle::validate_metadosis_timestamp(timestamp)?;

    if let Some(due_day) = lifecycle::find_due_emission_day(&metadosis, ctx)? {
        process_ready_candidate(&mut metadosis, ctx, &due_day)?;
    }

    Ok(())
}
