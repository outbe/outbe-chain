use crate::runtime::create_worldwide_day_for_date;
use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};
use outbe_promislimit::PromisLimitContract;

use crate::precompile::IMetadosis;
use crate::schema::{MetadosisContract, OcompDayLimitFormationStateEntryExt, WorldwideDayEntryExt};

/// Writes the terminal emission allocation into Metadosis-owned state.
pub fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<U256> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    if metadosis
        .read_ocomp_request_profile(&crate::ocomp::schema::poc_schema_limits())?
        .is_some()
    {
        return apply_ocomp_day_limit(ctx, amount);
    }

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

fn apply_ocomp_day_limit(ctx: &BlockRuntimeContext, base_limit: U256) -> Result<U256> {
    ctx.storage.with_checkpoint(|| {
        let wwd = WorldwideDay::from_timestamp(ctx.block.timestamp);
        let mut metadosis = MetadosisContract::new(ctx.storage.clone());
        create_worldwide_day_for_date(&mut metadosis, ctx, wwd)?;

        if let Some(formed) = metadosis.ocomp_day_limit_formation(wwd)? {
            if formed.base_limit != base_limit {
                return Err(PrecompileError::Revert(
                    "formed OCOMP day limit cannot be replaced".into(),
                ));
            }
            return Ok(U256::ZERO);
        }

        let carry_over = PromisLimitContract::new(ctx.storage.clone()).checked_take_carry_over()?;
        let day_limit = base_limit
            .checked_add(carry_over.taken)
            .ok_or_else(|| PrecompileError::Revert("OCOMP day limit carry-over overflow".into()))?;
        let day = metadosis.worldwide_days.entry(wwd);
        day.metadosis_limit_amount().write(day_limit)?;
        let formation = metadosis.ocomp_day_limit_formations.entry(wwd);
        formation.carry_over_taken().write(carry_over.taken)?;

        metadosis.emit(IMetadosis::MetadosisAccumulation {
            date: wwd.value(),
            dayMetadosisLimitAmount: day_limit,
            totalAccumulated: day_limit,
            blockNumber: ctx.block.block_number,
        })?;
        metadosis.emit(IMetadosis::OcompDayLimitFormed {
            worldwideDay: wwd.value(),
            baseLimit: base_limit,
            carryOverBefore: carry_over.before,
            carryOverTaken: carry_over.taken,
            carryOverAfter: carry_over.after,
            formedDayLimit: day_limit,
            blockNumber: ctx.block.block_number,
        })?;

        // Selector-last: a caught storage/event failure cannot make a partial
        // formation look committed, and exact replay cannot consume again.
        formation.formed().write(true)?;
        Ok(U256::ZERO)
    })
}
