use alloy_primitives::B256;

pub use super::fsm::{BlockExecutionFsm, BlockExecutionStage, BlockSystemFlags, BlockSystemPlan};
use super::{
    BlockHeightRegime, SysTxSelector, SystemTx, BOUNDARY_OUTCOME_SELECTOR,
    CERTIFIED_PARENT_ACCOUNTING_SELECTOR, CREDIS_FACTORY_ORDINAL, CREDIS_FACTORY_SELECTOR,
    CYCLE_TICK_SELECTOR, ENCLAVE_UPGRADE_DEADLINE_SELECTOR, GEM_CLEANUP_ORDINAL,
    GEM_CLEANUP_SELECTOR, GEM_FACTORY_ORDINAL, GEM_FACTORY_SELECTOR, HYPERLANE_SLASH_SELECTOR,
    INTEX_FACTORY_ORDINAL, INTEX_FACTORY_SELECTOR, LATE_FINALIZE_CREDITS_SELECTOR,
    NOD_FACTORY_ORDINAL, NOD_FACTORY_SELECTOR, ORACLE_SELECTOR, ORACLE_SLASH_SELECTOR,
    RADICLE_PUBLICATION_SELECTOR, REWARDS_SELECTOR, STAKING_SELECTOR, TEE_BOOTSTRAP_SELECTOR,
    UPDATE_SELECTOR, VALIDATOR_LIVENESS_SELECTOR, VOTE_SELECTOR,
};

/// Executor cursor that names the next system-tx phase that the block executor
/// expects to consume across `ConsensusStage -> BeginBlock -> UserTxs -> EndBlock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemTxPhase {
    // Stage 1: CONSENSUS_ORDER
    Phase1Preexecuted {
        body_index: u8,
        tx_hash: B256,
        receipt_index: u8,
    },
    LateFinalizeCredits {
        body_index: u8,
    },
    BoundaryOutcomeOptional {
        body_index: u8,
    },
    TeeBootstrapOptional {
        body_index: u8,
    },
    ValidatorLiveness {
        body_index: u8,
    },
    RadiclePublication {
        body_index: u8,
    },
    // Stage 2: BEGIN_BLOCK_ORDER
    Vote {
        body_index: u8,
    },
    Update {
        body_index: u8,
    },
    EnclaveUpgradeDeadline {
        body_index: u8,
    },
    Rewards {
        body_index: u8,
    },
    Staking {
        body_index: u8,
    },
    Oracle {
        body_index: u8,
    },
    OracleSlash {
        body_index: u8,
    },
    HyperlaneSlash {
        body_index: u8,
    },
    CycleTick {
        body_index: u8,
    },
    // Stage 3: MidBlock
    UserTxs,
    // Stage 4: END_BLOCK_ORDER
    EndBlockNodFactory,
    EndBlockGemFactory,
    EndBlockIntexFactory,
    EndBlockCredisFactory,
    EndBlockGemCleanup,
    // Terminal state
    EndBlock,
}

impl SystemTxPhase {
    pub const fn initial_for_regime(regime: BlockHeightRegime) -> Self {
        match regime {
            BlockHeightRegime::Genesis => Self::UserTxs,
            BlockHeightRegime::GenesisBootstrap => Self::BoundaryOutcomeOptional { body_index: 0 },
            BlockHeightRegime::SteadyState => Self::Phase1Preexecuted {
                body_index: 0,
                tx_hash: B256::ZERO,
                receipt_index: 0,
            },
        }
    }

    pub const fn initial_for_block(block_number: u64, genesis_bootstrap_block_number: u64) -> Self {
        if block_number == 0 || block_number > genesis_bootstrap_block_number {
            Self::initial_for_regime(BlockHeightRegime::for_block(block_number))
        } else {
            Self::BoundaryOutcomeOptional { body_index: 0 }
        }
    }

    pub const fn expected_selector(&self) -> Option<SysTxSelector> {
        match self {
            Self::Phase1Preexecuted { .. } => Some(CERTIFIED_PARENT_ACCOUNTING_SELECTOR),
            Self::LateFinalizeCredits { .. } => Some(LATE_FINALIZE_CREDITS_SELECTOR),
            Self::BoundaryOutcomeOptional { .. } => Some(BOUNDARY_OUTCOME_SELECTOR),
            Self::TeeBootstrapOptional { .. } => Some(TEE_BOOTSTRAP_SELECTOR),
            Self::ValidatorLiveness { .. } => Some(VALIDATOR_LIVENESS_SELECTOR),
            Self::RadiclePublication { .. } => Some(RADICLE_PUBLICATION_SELECTOR),
            Self::Vote { .. } => Some(VOTE_SELECTOR),
            Self::Update { .. } => Some(UPDATE_SELECTOR),
            Self::EnclaveUpgradeDeadline { .. } => Some(ENCLAVE_UPGRADE_DEADLINE_SELECTOR),
            Self::Rewards { .. } => Some(REWARDS_SELECTOR),
            Self::Staking { .. } => Some(STAKING_SELECTOR),
            Self::Oracle { .. } => Some(ORACLE_SELECTOR),
            Self::OracleSlash { .. } => Some(ORACLE_SLASH_SELECTOR),
            Self::HyperlaneSlash { .. } => Some(HYPERLANE_SLASH_SELECTOR),
            Self::CycleTick { .. } => Some(CYCLE_TICK_SELECTOR),
            Self::EndBlockNodFactory => Some(NOD_FACTORY_SELECTOR),
            Self::EndBlockGemFactory => Some(GEM_FACTORY_SELECTOR),
            Self::EndBlockIntexFactory => Some(INTEX_FACTORY_SELECTOR),
            Self::EndBlockCredisFactory => Some(CREDIS_FACTORY_SELECTOR),
            Self::EndBlockGemCleanup => Some(GEM_CLEANUP_SELECTOR),
            Self::UserTxs | Self::EndBlock => None,
        }
    }

    pub fn accepts(&self, input: &SystemTx) -> bool {
        self.expected_selector() == Some(input.selector())
    }

    pub const fn body_index(&self) -> Option<u8> {
        match self {
            Self::Phase1Preexecuted { body_index, .. }
            | Self::LateFinalizeCredits { body_index }
            | Self::BoundaryOutcomeOptional { body_index }
            | Self::TeeBootstrapOptional { body_index }
            | Self::ValidatorLiveness { body_index }
            | Self::RadiclePublication { body_index }
            | Self::Vote { body_index }
            | Self::Update { body_index }
            | Self::EnclaveUpgradeDeadline { body_index }
            | Self::Rewards { body_index }
            | Self::Staking { body_index }
            | Self::Oracle { body_index }
            | Self::OracleSlash { body_index }
            | Self::HyperlaneSlash { body_index }
            | Self::CycleTick { body_index } => Some(*body_index),
            Self::EndBlockNodFactory => Some(NOD_FACTORY_ORDINAL),
            Self::EndBlockGemFactory => Some(GEM_FACTORY_ORDINAL),
            Self::EndBlockIntexFactory => Some(INTEX_FACTORY_ORDINAL),
            Self::EndBlockCredisFactory => Some(CREDIS_FACTORY_ORDINAL),
            Self::EndBlockGemCleanup => Some(GEM_CLEANUP_ORDINAL),
            Self::UserTxs | Self::EndBlock => None,
        }
    }

    pub const fn advance_after_commit(
        self,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
    ) -> Self {
        self.advance_after_commit_with_publication(has_boundary_outcome, has_tee_bootstrap, false)
    }

    pub const fn advance_after_commit_with_publication(
        self,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
        has_publication: bool,
    ) -> Self {
        match self {
            Self::Phase1Preexecuted { body_index, .. } => Self::LateFinalizeCredits {
                body_index: body_index + 1,
            },
            Self::LateFinalizeCredits { body_index } => Self::next_after_late_credits(
                body_index + 1,
                has_boundary_outcome,
                has_tee_bootstrap,
            ),
            Self::BoundaryOutcomeOptional { body_index } => {
                Self::next_after_boundary_outcome(body_index + 1, has_tee_bootstrap)
            }
            Self::TeeBootstrapOptional { body_index } => Self::ValidatorLiveness {
                body_index: body_index + 1,
            },
            Self::ValidatorLiveness { body_index } => {
                Self::next_after_validator_liveness(body_index + 1, has_publication)
            }
            Self::RadiclePublication { body_index } => Self::Vote {
                body_index: body_index + 1,
            },
            other => other.advance_fixed_phase(),
        }
    }

    const fn next_after_late_credits(
        next_index: u8,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
    ) -> Self {
        if has_boundary_outcome {
            Self::BoundaryOutcomeOptional {
                body_index: next_index,
            }
        } else {
            Self::next_after_boundary_outcome(next_index, has_tee_bootstrap)
        }
    }

    const fn next_after_boundary_outcome(next_index: u8, has_tee_bootstrap: bool) -> Self {
        if has_tee_bootstrap {
            Self::TeeBootstrapOptional {
                body_index: next_index,
            }
        } else {
            Self::ValidatorLiveness {
                body_index: next_index,
            }
        }
    }

    const fn next_after_validator_liveness(next_index: u8, has_publication: bool) -> Self {
        if has_publication {
            Self::RadiclePublication {
                body_index: next_index,
            }
        } else {
            Self::Vote {
                body_index: next_index,
            }
        }
    }

    const fn advance_fixed_phase(self) -> Self {
        match self {
            Self::Vote { body_index } => Self::Update {
                body_index: body_index + 1,
            },
            Self::Update { body_index } => Self::EnclaveUpgradeDeadline {
                body_index: body_index + 1,
            },
            Self::EnclaveUpgradeDeadline { body_index } => Self::Rewards {
                body_index: body_index + 1,
            },
            Self::Rewards { body_index } => Self::Staking {
                body_index: body_index + 1,
            },
            Self::Staking { body_index } => Self::Oracle {
                body_index: body_index + 1,
            },
            Self::Oracle { body_index } => Self::OracleSlash {
                body_index: body_index + 1,
            },
            Self::OracleSlash { body_index } => Self::HyperlaneSlash {
                body_index: body_index + 1,
            },
            Self::HyperlaneSlash { body_index } => Self::CycleTick {
                body_index: body_index + 1,
            },
            Self::CycleTick { .. } | Self::UserTxs => Self::UserTxs,
            Self::EndBlockNodFactory => Self::EndBlockGemFactory,
            Self::EndBlockGemFactory => Self::EndBlockIntexFactory,
            Self::EndBlockIntexFactory => Self::EndBlockCredisFactory,
            Self::EndBlockCredisFactory => Self::EndBlockGemCleanup,
            Self::EndBlockGemCleanup
            | Self::EndBlock
            | Self::Phase1Preexecuted { .. }
            | Self::LateFinalizeCredits { .. }
            | Self::BoundaryOutcomeOptional { .. }
            | Self::TeeBootstrapOptional { .. }
            | Self::ValidatorLiveness { .. }
            | Self::RadiclePublication { .. } => Self::EndBlock,
        }
    }
}
