use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::MetadosisContract;
use crate::{lifecycle, process::process_ready_candidate};

pub fn process_emission_day(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    lifecycle::validate_metadosis_timestamp(ctx.block.timestamp)?;
    if let Some(record) = lifecycle::ready_emission_day(&metadosis, ctx, day)? {
        process_ready_candidate(&mut metadosis, ctx, &record)?;
    }
    Ok(())
}
