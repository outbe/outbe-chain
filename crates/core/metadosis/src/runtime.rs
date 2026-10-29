use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::MetadosisContract;
use crate::{lifecycle, settlement::process_ready_candidate};

pub fn start_metadosis(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;
    lifecycle::validate_metadosis_timestamp(timestamp)?;

    if ctx.block.block_number == 1 {
        lifecycle::init_genesis_day_inner(&mut metadosis, ctx)?;
    }

    lifecycle::advance_active_worldwide_days(ctx)?;

    lifecycle::create_lookahead_days(&mut metadosis, ctx, timestamp)?;

    lifecycle::advance_active_worldwide_days(ctx)?;

    if let Some(due_day) = lifecycle::find_due_emission_day(&metadosis, ctx)? {
        process_ready_candidate(&mut metadosis, ctx, &due_day)?;
    }

    Ok(())
}

pub fn advance_active_worldwide_days(ctx: &BlockRuntimeContext) -> Result<()> {
    lifecycle::advance_active_worldwide_days(ctx)
}

pub fn init_genesis_day(ctx: &BlockRuntimeContext) -> Result<()> {
    lifecycle::init_genesis_day(ctx)
}
