use super::{BlockOrder, BlockSystemPlan, SystemTx};
use outbe_primitives::{
    block::BlockSystemFlags,
    system_tx_context::{SystemTxBuildContext, SystemTxPayloadBuildError},
};

/// Build only active prefix steps, preserving each step's static wire index.
pub fn assemble_prefix_system_txs(
    order: &BlockOrder,
    ctx: &SystemTxBuildContext<'_>,
) -> Result<Vec<SystemTx>, SystemTxPayloadBuildError> {
    if !ctx.regime.has_system_tx_prefix() {
        return Ok(Vec::new());
    }
    let plan = BlockSystemPlan::for_block(
        ctx.block_number,
        BlockSystemFlags::from_artifacts(ctx.artifacts),
        order,
    );
    let mut out = Vec::with_capacity(plan.consensus_steps.len() + plan.begin_block_steps.len());
    for planned in &plan.consensus_steps {
        let payload = (planned.step.build_payload)(ctx)?;
        out.push(SystemTx::Consensus {
            step_index: planned.id.step_index,
            payload,
        });
    }
    for planned in &plan.begin_block_steps {
        let _ = (planned.step.build_payload)(ctx)?;
        out.push(SystemTx::BeginBlock {
            step_index: planned.id.step_index,
        });
    }
    Ok(out)
}
