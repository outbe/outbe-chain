use super::{
    BlockHeightRegime, SysTxSelector, SystemTxError, SystemTxKind, BEGIN_BLOCK_ORDER,
    END_BLOCK_ORDER,
};

/// 4-Stage Block Execution FSM stage (`ConsensusStage -> BeginBlock -> MidBlock -> EndBlock -> Completed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockExecutionStage {
    ConsensusStage,
    BeginBlock,
    MidBlock,
    EndBlock,
    Completed,
}

/// Optional consensus-stage flags derived from block height and header artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockSystemFlags {
    pub has_boundary_outcome: bool,
    pub has_tee_bootstrap: bool,
    pub has_publication: bool,
}

/// Deterministic per-block schedule of `ConsensusStage`, `BeginBlock`, `MidBlock`, and `EndBlock` work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSystemPlan {
    pub block_number: u64,
    pub consensus_kinds: Vec<SystemTxKind>,
    pub begin_block_kinds: Vec<SystemTxKind>,
    pub end_block_kinds: Vec<SystemTxKind>,
    pub allows_user_txs: bool,
}

impl BlockSystemPlan {
    pub fn for_block(
        block_number: u64,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
        has_publication: bool,
    ) -> Self {
        Self::for_block_flags(
            block_number,
            BlockSystemFlags {
                has_boundary_outcome,
                has_tee_bootstrap,
                has_publication,
            },
        )
    }

    pub fn for_block_flags(block_number: u64, flags: BlockSystemFlags) -> Self {
        let regime = BlockHeightRegime::for_block(block_number);
        if !regime.has_begin_zone_system_txs() {
            return Self {
                block_number,
                consensus_kinds: Vec::new(),
                begin_block_kinds: Vec::new(),
                end_block_kinds: Vec::new(),
                allows_user_txs: false,
            };
        }

        let consensus_kinds = Self::build_consensus_kinds(regime, flags);
        let begin_block_kinds = BEGIN_BLOCK_ORDER.to_vec();
        let end_block_kinds = if regime.has_end_zone_system_txs() {
            END_BLOCK_ORDER.to_vec()
        } else {
            Vec::new()
        };

        Self {
            block_number,
            consensus_kinds,
            begin_block_kinds,
            end_block_kinds,
            allows_user_txs: regime.allows_user_transactions(),
        }
    }

    fn build_consensus_kinds(
        regime: BlockHeightRegime,
        flags: BlockSystemFlags,
    ) -> Vec<SystemTxKind> {
        let mut consensus_kinds = Vec::with_capacity(6);
        if regime.has_parent_finalization_accounting() {
            consensus_kinds.push(SystemTxKind::CertifiedParentAccounting);
            consensus_kinds.push(SystemTxKind::LateFinalizeCredits);
        }
        if flags.has_boundary_outcome {
            consensus_kinds.push(SystemTxKind::BoundaryOutcome);
        }
        if flags.has_tee_bootstrap {
            consensus_kinds.push(SystemTxKind::TeeBootstrap);
        }
        consensus_kinds.push(SystemTxKind::ValidatorLiveness);
        if flags.has_publication {
            consensus_kinds.push(SystemTxKind::RadiclePublication);
        }
        consensus_kinds
    }

    pub fn begin_zone_kinds(&self) -> Vec<SystemTxKind> {
        let mut out = Vec::with_capacity(self.consensus_kinds.len() + self.begin_block_kinds.len());
        out.extend_from_slice(&self.consensus_kinds);
        out.extend_from_slice(&self.begin_block_kinds);
        out
    }

    pub fn begin_zone_selectors(&self) -> Vec<SysTxSelector> {
        self.begin_zone_kinds()
            .into_iter()
            .map(SystemTxKind::selector)
            .collect()
    }

    pub fn end_zone_selectors(&self) -> Vec<SysTxSelector> {
        self.end_block_kinds
            .iter()
            .copied()
            .map(SystemTxKind::selector)
            .collect()
    }

    pub const fn initial_stage(&self) -> BlockExecutionStage {
        if !self.consensus_kinds.is_empty() {
            BlockExecutionStage::ConsensusStage
        } else {
            self.stage_after_consensus()
        }
    }

    pub const fn stage_after_consensus(&self) -> BlockExecutionStage {
        if !self.begin_block_kinds.is_empty() {
            BlockExecutionStage::BeginBlock
        } else {
            self.stage_after_begin()
        }
    }

    pub const fn stage_after_begin(&self) -> BlockExecutionStage {
        if self.allows_user_txs {
            BlockExecutionStage::MidBlock
        } else if !self.end_block_kinds.is_empty() {
            BlockExecutionStage::EndBlock
        } else {
            BlockExecutionStage::Completed
        }
    }
}

/// Deterministic 4-Stage Block Execution Finite State Machine
/// (`ConsensusStage -> BeginBlock -> MidBlock -> EndBlock -> Completed`).
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

    pub fn for_block(
        block_number: u64,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
        has_publication: bool,
    ) -> Self {
        Self::new(BlockSystemPlan::for_block(
            block_number,
            has_boundary_outcome,
            has_tee_bootstrap,
            has_publication,
        ))
    }

    pub const fn stage(&self) -> BlockExecutionStage {
        self.stage
    }

    pub const fn plan(&self) -> &BlockSystemPlan {
        &self.plan
    }

    pub const fn user_txs_executed(&self) -> usize {
        self.user_txs_executed
    }

    pub fn expected_system_tx_kind(&self) -> Option<SystemTxKind> {
        match self.stage {
            BlockExecutionStage::ConsensusStage => self
                .plan
                .consensus_kinds
                .get(self.consensus_cursor)
                .copied(),
            BlockExecutionStage::BeginBlock => {
                self.plan.begin_block_kinds.get(self.begin_cursor).copied()
            }
            BlockExecutionStage::MidBlock => self.plan.end_block_kinds.first().copied(),
            BlockExecutionStage::EndBlock => {
                self.plan.end_block_kinds.get(self.end_cursor).copied()
            }
            BlockExecutionStage::Completed => None,
        }
    }

    pub fn expected_selector(&self) -> Option<SysTxSelector> {
        self.expected_system_tx_kind().map(SystemTxKind::selector)
    }

    pub fn expected_ordinal(&self) -> Option<u8> {
        match self.stage {
            BlockExecutionStage::ConsensusStage => u8::try_from(self.consensus_cursor).ok(),
            BlockExecutionStage::BeginBlock => {
                u8::try_from(self.plan.consensus_kinds.len() + self.begin_cursor).ok()
            }
            BlockExecutionStage::MidBlock | BlockExecutionStage::EndBlock => self
                .expected_system_tx_kind()
                .and_then(|kind| kind.nonce_ordinal()),
            BlockExecutionStage::Completed => None,
        }
    }

    pub fn on_system_tx(
        &mut self,
        kind: SystemTxKind,
    ) -> Result<BlockExecutionStage, SystemTxError> {
        let Some(expected) = self.expected_system_tx_kind() else {
            return Err(SystemTxError::UnexpectedSystemTx {
                stage: self.stage,
                expected: None,
                actual: kind.name(),
            });
        };
        if kind != expected {
            return Err(SystemTxError::UnexpectedSystemTx {
                stage: self.stage,
                expected: Some(expected.name()),
                actual: kind.name(),
            });
        }
        self.advance_after_expected_system_tx();
        Ok(self.stage)
    }

    fn advance_after_expected_system_tx(&mut self) {
        match self.stage {
            BlockExecutionStage::ConsensusStage => {
                self.consensus_cursor += 1;
                if self.consensus_cursor == self.plan.consensus_kinds.len() {
                    self.stage = self.plan.stage_after_consensus();
                }
            }
            BlockExecutionStage::BeginBlock => {
                self.begin_cursor += 1;
                if self.begin_cursor == self.plan.begin_block_kinds.len() {
                    self.stage = self.plan.stage_after_begin();
                }
            }
            BlockExecutionStage::MidBlock => {
                self.end_cursor = 1;
                self.stage = if self.end_cursor == self.plan.end_block_kinds.len() {
                    BlockExecutionStage::Completed
                } else {
                    BlockExecutionStage::EndBlock
                };
            }
            BlockExecutionStage::EndBlock => {
                self.end_cursor += 1;
                if self.end_cursor == self.plan.end_block_kinds.len() {
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
                expected_next: self.expected_system_tx_kind().map(SystemTxKind::name),
            })
        }
    }
}
