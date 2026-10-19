use crate::runtime::create_worldwide_day_for_date;
use crate::state::{DayLimitFormation, DayLimitFormationReceipt};
use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::{
        metadosis_cycle_allocation_binding, metadosis_late_settlement_binding,
        MetadosisCertifiedFinality, MetadosisCycleLifecycle,
    },
};
use outbe_promislimit::PromisLimitContract;

use crate::schema::MetadosisContract;

/// Writes the terminal emission allocation into Metadosis-owned state.
pub(crate) fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<DayLimitFormationReceipt> {
    let binding = metadosis_cycle_allocation_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    crate::commit::commit_transition::<MetadosisCycleLifecycle, _>(
        ctx.storage.clone(),
        binding,
        |_| apply_inner(ctx, amount),
    )
}

fn apply_inner(ctx: &BlockRuntimeContext, amount: U256) -> Result<DayLimitFormationReceipt> {
    apply_day_limit(ctx, amount)
}

/// Recycles late-settlement headroom without racing daily limit formation.
/// Daily Cycle allocations form base limits. Late residues become carry-over
/// credit, which native Metadosis draws when the next day limit is formed.
pub(crate) fn apply_late_settlement_headroom(
    ctx: &BlockRuntimeContext,
    amount: U256,
) -> Result<U256> {
    let binding = metadosis_late_settlement_binding(
        ctx.block.chain_id,
        ctx.block.block_number,
        ctx.block.timestamp,
    );
    crate::commit::commit_transition::<MetadosisCertifiedFinality, _>(
        ctx.storage.clone(),
        binding,
        |_| apply_late_settlement_headroom_inner(ctx, amount),
    )
}

fn apply_late_settlement_headroom_inner(ctx: &BlockRuntimeContext, amount: U256) -> Result<U256> {
    PromisLimitContract::new(ctx.storage.clone()).checked_add_carry_over(amount)?;
    Ok(U256::ZERO)
}

fn apply_day_limit(
    ctx: &BlockRuntimeContext,
    base_limit: U256,
) -> Result<DayLimitFormationReceipt> {
    (|| {
        let wwd = WorldwideDay::from_timestamp(ctx.block.timestamp);
        let mut metadosis = MetadosisContract::new(ctx.storage.clone());
        create_worldwide_day_for_date(&mut metadosis, ctx, wwd)?;

        if let Some(formed) = metadosis.day_limit_formation(wwd)? {
            if formed.base_limit != base_limit {
                return Err(
                    crate::errors::MetadosisCallerError::FormedDayLimitCannotBeReplaced.into(),
                );
            }
            return Ok(DayLimitFormationReceipt::Formed(formed));
        }

        // A day is formed against its own emission. The accumulator is drawn from at the request,
        // where the day's demand is known, so forming one day never strands it for the next.
        let carry_over = PromisLimitContract::new(ctx.storage.clone()).get_total_unallocated()?;
        crate::commit::commit_day_limit_formation(
            &mut metadosis,
            DayLimitFormation {
                worldwide_day: wwd,
                base_limit,
                carry_over_before: carry_over,
                carry_over_taken: U256::ZERO,
                carry_over_after: carry_over,
                day_limit: base_limit,
                block_number: ctx.block.block_number,
            },
        )
    })()
}
