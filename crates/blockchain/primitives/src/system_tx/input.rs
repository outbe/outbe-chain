use alloy_primitives::Bytes;

use crate::{
    block::{BlockOrder, BlockSystemFlags},
    system_tx_context::{SystemTxBuildContext, SystemTxPayloadBuildError},
};

use super::{
    end_block_nonce_ordinal, selector_from_input, BlockExecutionStage, BlockSystemPlan, BodyZone,
    SysTxSelector, SystemTxError, BEGIN_BLOCK_STAGE_SELECTOR, CONSENSUS_STAGE_SELECTOR,
    END_BLOCK_STAGE_SELECTOR, SYSTEM_CALLDATA_FRAMING_BYTES, SYSTEM_TX_INPUT_VERSION,
};

/// Wire identity uses the static index, never the filtered position in a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemTxStepId {
    pub stage: BlockExecutionStage,
    pub step_index: u8,
}

impl SystemTxStepId {
    pub const fn consensus(step_index: u8) -> Self {
        Self {
            stage: BlockExecutionStage::Consensus,
            step_index,
        }
    }
    pub const fn begin_block(step_index: u8) -> Self {
        Self {
            stage: BlockExecutionStage::BeginBlock,
            step_index,
        }
    }
    pub const fn end_block(step_index: u8) -> Self {
        Self {
            stage: BlockExecutionStage::EndBlock,
            step_index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemTx {
    Consensus { step_index: u8, payload: Bytes },
    BeginBlock { step_index: u8 },
    EndBlock { step_index: u8 },
}

impl SystemTx {
    pub const fn step_index(&self) -> u8 {
        match self {
            Self::Consensus { step_index, .. }
            | Self::BeginBlock { step_index }
            | Self::EndBlock { step_index } => *step_index,
        }
    }

    pub const fn step_id(&self) -> SystemTxStepId {
        SystemTxStepId {
            stage: self.stage(),
            step_index: self.step_index(),
        }
    }

    pub const fn stage(&self) -> BlockExecutionStage {
        match self {
            Self::Consensus { .. } => BlockExecutionStage::Consensus,
            Self::BeginBlock { .. } => BlockExecutionStage::BeginBlock,
            Self::EndBlock { .. } => BlockExecutionStage::EndBlock,
        }
    }

    pub const fn stage_name(&self) -> &'static str {
        match self {
            Self::Consensus { .. } => "Consensus",
            Self::BeginBlock { .. } => "BeginBlock",
            Self::EndBlock { .. } => "EndBlock",
        }
    }

    pub const fn selector(&self) -> SysTxSelector {
        match self {
            Self::Consensus { .. } => CONSENSUS_STAGE_SELECTOR,
            Self::BeginBlock { .. } => BEGIN_BLOCK_STAGE_SELECTOR,
            Self::EndBlock { .. } => END_BLOCK_STAGE_SELECTOR,
        }
    }

    pub fn payload(&self) -> &[u8] {
        match self {
            Self::Consensus { payload, .. } => payload.as_ref(),
            Self::BeginBlock { .. } | Self::EndBlock { .. } => &[],
        }
    }

    pub const fn body_zone(&self) -> BodyZone {
        match self {
            Self::Consensus { .. } | Self::BeginBlock { .. } => BodyZone::BeginBlock,
            Self::EndBlock { .. } => BodyZone::EndBlock,
        }
    }

    /// Prefix ordinals are body positions; only suffix ordinals derive from the static index.
    pub const fn nonce_ordinal(&self) -> Option<u8> {
        match self {
            Self::EndBlock { step_index } => end_block_nonce_ordinal(*step_index),
            _ => None,
        }
    }

    pub fn encode(&self) -> Result<Bytes, SystemTxError> {
        let mut out = Vec::with_capacity(SYSTEM_CALLDATA_FRAMING_BYTES + self.payload().len());
        out.extend_from_slice(&self.selector());
        out.push(SYSTEM_TX_INPUT_VERSION);
        out.push(self.step_index());
        out.extend_from_slice(self.payload());
        Ok(Bytes::from(out))
    }

    pub fn decode(data: &[u8]) -> Result<Self, SystemTxError> {
        if data.len() < SYSTEM_CALLDATA_FRAMING_BYTES {
            return Err(SystemTxError::InputTooShort { len: data.len() });
        }
        let selector = selector_from_input(data)?;
        if !matches!(
            selector,
            CONSENSUS_STAGE_SELECTOR | BEGIN_BLOCK_STAGE_SELECTOR | END_BLOCK_STAGE_SELECTOR
        ) {
            return Err(SystemTxError::UnknownSelector(selector));
        }
        if data[4] != SYSTEM_TX_INPUT_VERSION {
            return Err(SystemTxError::UnsupportedVersion(data[4]));
        }
        let step_index = data[5];
        let payload = &data[SYSTEM_CALLDATA_FRAMING_BYTES..];
        let tx = match selector {
            CONSENSUS_STAGE_SELECTOR => Self::Consensus {
                step_index,
                payload: Bytes::copy_from_slice(payload),
            },
            BEGIN_BLOCK_STAGE_SELECTOR => Self::BeginBlock { step_index },
            _ => Self::EndBlock { step_index },
        };
        if !matches!(tx, Self::Consensus { .. }) && !payload.is_empty() {
            return Err(SystemTxError::NonEmptyStagePayload {
                stage: tx.stage(),
                len: payload.len(),
            });
        }
        Ok(tx)
    }
}

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
        let payload = (planned.step.build_payload)(ctx)?;
        debug_assert!(payload.is_empty());
        out.push(SystemTx::BeginBlock {
            step_index: planned.id.step_index,
        });
    }
    Ok(out)
}
