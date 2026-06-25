use crate::runtime::create_worldwide_day_for_date;
use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::precompile::IMetadosis;
use crate::schema::{MetadosisContract, WorldwideDayEntryExt};

/// Writes the terminal emission allocation into Metadosis-owned state.
pub fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<U256> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());

    let wwd = WorldwideDay::from_timestamp(ctx.block.timestamp);
    create_worldwide_day_for_date(&mut metadosis, ctx, wwd)?;
    metadosis
        .worldwide_days
        .entry(wwd)
        .metadosis_limit_amount()
        .write(amount)?;

    metadosis.emit(IMetadosis::MetadosisAccumulation {
        date: wwd.value(),
        dayMetadosisLimitAmount: amount,
        totalAccumulated: amount,
        blockNumber: ctx.block.block_number,
    })?;

    Ok(U256::ZERO)
}
