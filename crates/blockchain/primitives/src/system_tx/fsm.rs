use super::{
    end_block_nonce_ordinal, SysTxSelector, SystemTx, SystemTxError, SystemTxStepId,
    BEGIN_BLOCK_STAGE_SELECTOR, CONSENSUS_STAGE_SELECTOR, END_BLOCK_STAGE_SELECTOR,
};
use crate::block::{BlockHeightRegime, BlockOrder, BlockSystemFlags, ProtocolStep};

/// 4-Stage Block Execution FSM stage (`Consensus -> BeginBlock -> MidBlock -> EndBlock -> Completed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlockExecutionStage {
    Consensus,
    BeginBlock,
    MidBlock,
    EndBlock,
    Completed,
}

/// Concrete step descriptor in a per-block `BlockSystemPlan`.
/// Retains the static `SystemTxStepId` (`stage` + `step_index`) in `BlockOrder` across active-step filtering.
#[derive(Debug, Clone, Copy)]
pub struct PlannedSystemStep {
    pub id: SystemTxStepId,
    pub step: ProtocolStep,
}

impl PartialEq for PlannedSystemStep {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.step.name == other.step.name
            && self.step.revert_fails_block == other.step.revert_fails_block
            && self.step.include_validator_snapshot == other.step.include_validator_snapshot
    }
}
impl Eq for PlannedSystemStep {}

impl PlannedSystemStep {
    pub const fn from_protocol_step(
        stage: BlockExecutionStage,
        step_index: u8,
        step: &ProtocolStep,
    ) -> Self {
        Self {
            id: SystemTxStepId { stage, step_index },
            step: *step,
        }
    }

    pub const fn selector(&self) -> SysTxSelector {
        match self.id.stage {
            BlockExecutionStage::Consensus => CONSENSUS_STAGE_SELECTOR,
            BlockExecutionStage::BeginBlock => BEGIN_BLOCK_STAGE_SELECTOR,
            BlockExecutionStage::EndBlock => END_BLOCK_STAGE_SELECTOR,
            BlockExecutionStage::MidBlock | BlockExecutionStage::Completed => [0u8; 4],
        }
    }

    pub const fn matches_tx(&self, tx: &SystemTx) -> bool {
        match (self.id.stage, tx) {
            (BlockExecutionStage::Consensus, SystemTx::Consensus { step_index, .. })
            | (BlockExecutionStage::BeginBlock, SystemTx::BeginBlock { step_index })
            | (BlockExecutionStage::EndBlock, SystemTx::EndBlock { step_index }) => {
                self.id.step_index == *step_index
            }
            _ => false,
        }
    }

    pub const fn end_nonce_ordinal(&self) -> Option<u8> {
        if matches!(self.id.stage, BlockExecutionStage::EndBlock) {
            end_block_nonce_ordinal(self.id.step_index)
        } else {
            None
        }
    }
}

/// Deterministic per-block schedule of `Consensus`, `BeginBlock`, `MidBlock`, and `EndBlock` work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSystemPlan {
    pub block_number: u64,
    pub consensus_steps: Vec<PlannedSystemStep>,
    pub begin_block_steps: Vec<PlannedSystemStep>,
    pub end_block_steps: Vec<PlannedSystemStep>,
    pub allows_user_txs: bool,
}

impl BlockSystemPlan {
    pub fn for_block(block_number: u64, flags: BlockSystemFlags, order: &BlockOrder) -> Self {
        let regime = BlockHeightRegime::for_block(block_number);
        if !regime.has_system_tx_prefix() {
            return Self {
                block_number,
                consensus_steps: Vec::new(),
                begin_block_steps: Vec::new(),
                end_block_steps: Vec::new(),
                allows_user_txs: false,
            };
        }

        let collect_active_steps = |stage: BlockExecutionStage,
                                    steps: &'static [ProtocolStep]|
         -> Vec<PlannedSystemStep> {
            debug_assert!(u8::try_from(steps.len()).is_ok());
            steps
                .iter()
                .enumerate()
                .filter(|(_, step)| (step.is_active)(regime, flags))
                .filter_map(|(idx, step)| {
                    let step_index = u8::try_from(idx).ok()?;
                    Some(PlannedSystemStep::from_protocol_step(
                        stage, step_index, step,
                    ))
                })
                .collect()
        };

        let consensus_steps = collect_active_steps(BlockExecutionStage::Consensus, order.consensus);
        let begin_block_steps =
            collect_active_steps(BlockExecutionStage::BeginBlock, order.begin_block);
        let end_block_steps = if regime.has_end_block_system_txs() {
            collect_active_steps(BlockExecutionStage::EndBlock, order.end_block)
        } else {
            Vec::new()
        };

        Self {
            block_number,
            consensus_steps,
            begin_block_steps,
            end_block_steps,
            allows_user_txs: regime.allows_user_transactions(),
        }
    }

    pub fn expected_prefix_steps(&self) -> Vec<SystemTxStepId> {
        let mut out = Vec::with_capacity(self.consensus_steps.len() + self.begin_block_steps.len());
        out.extend(self.consensus_steps.iter().map(|s| s.id));
        out.extend(self.begin_block_steps.iter().map(|s| s.id));
        out
    }

    pub fn expected_end_block_steps(&self) -> Vec<SystemTxStepId> {
        self.end_block_steps.iter().map(|s| s.id).collect()
    }

    pub const fn initial_stage(&self) -> BlockExecutionStage {
        if !self.consensus_steps.is_empty() {
            BlockExecutionStage::Consensus
        } else {
            self.stage_after_consensus()
        }
    }

    pub const fn stage_after_consensus(&self) -> BlockExecutionStage {
        if !self.begin_block_steps.is_empty() {
            BlockExecutionStage::BeginBlock
        } else {
            self.stage_after_begin()
        }
    }

    pub const fn stage_after_begin(&self) -> BlockExecutionStage {
        if self.allows_user_txs {
            BlockExecutionStage::MidBlock
        } else if !self.end_block_steps.is_empty() {
            BlockExecutionStage::EndBlock
        } else {
            BlockExecutionStage::Completed
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockExecutionFsm {
    plan: BlockSystemPlan,
    stage: BlockExecutionStage,
    consensus_cursor: usize,
    begin_cursor: usize,
    end_cursor: usize,
    user_txs_executed: usize,
}

impl BlockExecutionFsm {
    pub fn new(plan: BlockSystemPlan) -> Self {
        let stage = plan.initial_stage();
        Self {
            plan,
            stage,
            consensus_cursor: 0,
            begin_cursor: 0,
            end_cursor: 0,
            user_txs_executed: 0,
        }
    }

    pub fn for_block(block_number: u64, flags: BlockSystemFlags, order: &BlockOrder) -> Self {
        Self::new(BlockSystemPlan::for_block(block_number, flags, order))
    }

    /// Standalone execution starts in MidBlock without running protocol pre-execution.
    pub fn for_mid_block(order: &BlockOrder) -> Self {
        let end_block_steps = order
            .end_block
            .iter()
            .enumerate()
            .filter_map(|(idx, step)| {
                let step_index = u8::try_from(idx).ok()?;
                Some(PlannedSystemStep::from_protocol_step(
                    BlockExecutionStage::EndBlock,
                    step_index,
                    step,
                ))
            })
            .collect();
        Self {
            plan: BlockSystemPlan {
                block_number: 0,
                consensus_steps: Vec::new(),
                begin_block_steps: Vec::new(),
                end_block_steps,
                allows_user_txs: true,
            },
            stage: BlockExecutionStage::MidBlock,
            consensus_cursor: 0,
            begin_cursor: 0,
            end_cursor: 0,
            user_txs_executed: 0,
        }
    }

    pub const fn stage(&self) -> BlockExecutionStage {
        self.stage
    }

    pub const fn plan(&self) -> &BlockSystemPlan {
        &self.plan
    }

    pub const fn end_cursor(&self) -> usize {
        self.end_cursor
    }

    pub const fn user_txs_executed(&self) -> usize {
        self.user_txs_executed
    }

    /// Returns the 0-based index into the active system-tx prefix (`Consensus` + `BeginBlock`)
    /// transaction list when the FSM is in `Consensus` or `BeginBlock`.
    pub const fn expected_prefix_body_index(&self) -> Option<usize> {
        match self.stage {
            BlockExecutionStage::Consensus => Some(self.consensus_cursor),
            BlockExecutionStage::BeginBlock => {
                Some(self.plan.consensus_steps.len() + self.begin_cursor)
            }
            BlockExecutionStage::MidBlock
            | BlockExecutionStage::EndBlock
            | BlockExecutionStage::Completed => None,
        }
    }

    pub fn expected_step(&self) -> Option<PlannedSystemStep> {
        match self.stage {
            BlockExecutionStage::Consensus => self
                .plan
                .consensus_steps
                .get(self.consensus_cursor)
                .copied(),
            BlockExecutionStage::BeginBlock => {
                self.plan.begin_block_steps.get(self.begin_cursor).copied()
            }
            BlockExecutionStage::MidBlock => self.plan.end_block_steps.first().copied(),
            BlockExecutionStage::EndBlock => {
                self.plan.end_block_steps.get(self.end_cursor).copied()
            }
            BlockExecutionStage::Completed => None,
        }
    }

    pub fn expected_step_id(&self) -> Option<SystemTxStepId> {
        self.expected_step().map(|step| step.id)
    }

    pub fn expected_ordinal(&self) -> Option<u8> {
        match self.stage {
            BlockExecutionStage::Consensus => u8::try_from(self.consensus_cursor).ok(),
            BlockExecutionStage::BeginBlock => {
                u8::try_from(self.plan.consensus_steps.len() + self.begin_cursor).ok()
            }
            BlockExecutionStage::MidBlock | BlockExecutionStage::EndBlock => self
                .expected_step()
                .and_then(|step| step.end_nonce_ordinal()),
            BlockExecutionStage::Completed => None,
        }
    }

    /// Transitions `MidBlock -> EndBlock` (or `MidBlock -> Completed` when `block_number`
    /// has no `end_block` system transactions). Idempotent if already in `EndBlock` or `Completed`.
    /// Replaces `OutbeBlockExecutor::enter_end_block_phase_if_needed`.
    pub fn enter_end_block(
        &mut self,
        block_number: u64,
    ) -> Result<BlockExecutionStage, SystemTxError> {
        match self.stage {
            BlockExecutionStage::EndBlock | BlockExecutionStage::Completed => Ok(self.stage),
            BlockExecutionStage::MidBlock => {
                if !BlockHeightRegime::for_block(block_number).has_end_block_system_txs()
                    || self.plan.end_block_steps.is_empty()
                {
                    self.plan.end_block_steps.clear();
                    self.stage = BlockExecutionStage::Completed;
                } else {
                    self.stage = BlockExecutionStage::EndBlock;
                    self.end_cursor = 0;
                }
                Ok(self.stage)
            }
            BlockExecutionStage::Consensus | BlockExecutionStage::BeginBlock => {
                Err(SystemTxError::UnexpectedSystemTx {
                    stage: self.stage,
                    expected: self.expected_step().map(|s| s.step.name),
                    actual: "EndBlock",
                })
            }
        }
    }

    pub fn on_system_tx(&mut self, tx: &SystemTx) -> Result<BlockExecutionStage, SystemTxError> {
        let Some(expected) = self.expected_step() else {
            return Err(SystemTxError::UnexpectedSystemTx {
                stage: self.stage,
                expected: None,
                actual: tx.stage_name(),
            });
        };
        if !expected.matches_tx(tx) {
            return Err(SystemTxError::UnexpectedSystemTx {
                stage: self.stage,
                expected: Some(expected.step.name),
                actual: tx.stage_name(),
            });
        }
        self.advance_after_expected_system_tx();
        Ok(self.stage)
    }

    fn advance_after_expected_system_tx(&mut self) {
        match self.stage {
            BlockExecutionStage::Consensus => {
                self.consensus_cursor += 1;
                if self.consensus_cursor == self.plan.consensus_steps.len() {
                    self.stage = self.plan.stage_after_consensus();
                }
            }
            BlockExecutionStage::BeginBlock => {
                self.begin_cursor += 1;
                if self.begin_cursor == self.plan.begin_block_steps.len() {
                    self.stage = self.plan.stage_after_begin();
                }
            }
            BlockExecutionStage::MidBlock => {
                self.end_cursor = 1;
                self.stage = if self.end_cursor == self.plan.end_block_steps.len() {
                    BlockExecutionStage::Completed
                } else {
                    BlockExecutionStage::EndBlock
                };
            }
            BlockExecutionStage::EndBlock => {
                self.end_cursor += 1;
                if self.end_cursor == self.plan.end_block_steps.len() {
                    self.stage = BlockExecutionStage::Completed;
                }
            }
            BlockExecutionStage::Completed => {}
        }
    }

    pub fn on_user_tx(&mut self) -> Result<BlockExecutionStage, SystemTxError> {
        if self.stage != BlockExecutionStage::MidBlock || !self.plan.allows_user_txs {
            return Err(SystemTxError::UnexpectedUserTx { stage: self.stage });
        }
        self.user_txs_executed += 1;
        Ok(self.stage)
    }

    pub fn finish(&self) -> Result<(), SystemTxError> {
        if self.stage == BlockExecutionStage::Completed {
            Ok(())
        } else {
            Err(SystemTxError::IncompleteBlockExecution {
                stage: self.stage,
                expected_next: self.expected_step().map(|step| step.step.name),
            })
        }
    }
}
