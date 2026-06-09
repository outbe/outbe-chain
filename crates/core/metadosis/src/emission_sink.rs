use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::precompile::IMetadosis;
use crate::runtime::timestamp_to_date_key;
use crate::schema::MetadosisContract;

/// Writes the terminal emission allocation into Metadosis-owned state.
pub fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<U256> {
    let mut metadosis = ctx.contract::<MetadosisContract>();
    metadosis.record_day_limit_at(ctx.block.timestamp, amount)?;

    let date = timestamp_to_date_key(ctx.block.timestamp);
    let total = metadosis.get_day_limit(date.into())?;
    metadosis.emit(IMetadosis::MetadosisAccumulation {
        date,
        dayMetadosisLimitAmount: amount,
        totalAccumulated: total,
        blockNumber: ctx.block.block_number,
    })?;

    Ok(U256::ZERO)
}
