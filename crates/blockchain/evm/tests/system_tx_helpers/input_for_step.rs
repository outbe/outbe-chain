use alloy_primitives::Bytes;
use outbe_primitives::system_tx::{BlockExecutionStage, SystemTx, SystemTxStepId};

pub(crate) fn input_for_step(id: SystemTxStepId) -> SystemTx {
    match id.stage {
        BlockExecutionStage::Consensus => SystemTx::Consensus {
            step_index: id.step_index,
            payload: Bytes::new(),
        },
        BlockExecutionStage::BeginBlock => SystemTx::BeginBlock {
            step_index: id.step_index,
        },
        BlockExecutionStage::EndBlock => SystemTx::EndBlock {
            step_index: id.step_index,
        },
        _ => panic!("not a system step"),
    }
}
