//! Daily accumulation — the Metadosis side of the daily emission-limit handoff.
//!
//! `outbe_emissionlimit` (`crates/system/emissionlimit/src/block.rs`) computes the
//! terminal daily metadosis-limit allocation and hands the amount here via
//! [`apply`]; this module ensures the day exists and records the amount into
//! Metadosis-owned state. It does not compute the allocation — only persists it
//! (via `set_metadosis_limit`) and emits `MetadosisAccumulation`.

use crate::runtime::create_worldwide_day_for_date;
use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;

/// Writes the terminal emission allocation into Metadosis-owned state.
pub fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<U256> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());

    let wwd = WorldwideDay::from_timestamp(ctx.block.timestamp);
    create_worldwide_day_for_date(&mut metadosis, ctx, wwd)?;
    metadosis.set_metadosis_limit(wwd, amount)?;

    metadosis.emit(IMetadosis::MetadosisAccumulation {
        date: wwd.value(),
        dayMetadosisLimitAmount: amount,
        totalAccumulated: amount,
        blockNumber: ctx.block.block_number,
    })?;

    Ok(U256::ZERO)
}
