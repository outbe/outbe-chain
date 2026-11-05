//! Daily allocation and bounded settlement. Amounts are protocol units, not native COEN.
pub use crate::distribution::{
    abandon_day_agent_rewards, allocate_day_agent_rewards, process_agent_rewards,
    DayAgentAllocation, DayAgentPools,
};

/// Delete at most 256 WAA/SRA entries and inspect at most four headers per block.
/// Cleanup has its own checkpoint and height guard, independent of monetary accrual.
pub fn cleanup(
    ctx: &outbe_primitives::block::BlockRuntimeContext<'_>,
) -> outbe_primitives::error::Result<()> {
    ctx.with_checkpoint(|| {
        let contract = ctx.contract::<crate::schema::AgentRewardContract>();
        if contract.cleanup_initialized.read()?
            && contract.cleanup_height.read()? == ctx.block.block_number
        {
            return Ok(());
        }
        crate::state::cleanup(&contract)?;
        contract.cleanup_height.write(ctx.block.block_number)?;
        contract.cleanup_initialized.write(true)
    })
}
