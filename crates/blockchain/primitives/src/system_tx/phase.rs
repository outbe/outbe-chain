use alloy_primitives::B256;

use super::{
    BlockHeightRegime, SysTxSelector, SystemTx, BOUNDARY_OUTCOME_SELECTOR,
    CALL_EVENT_PROCESSING_ORDINAL, CALL_EVENT_PROCESSING_SELECTOR,
    CERTIFIED_PARENT_ACCOUNTING_SELECTOR, CLEANUP_PROCESSING_ORDINAL, CLEANUP_PROCESSING_SELECTOR,
    CYCLE_TICK_SELECTOR, HOOK_EVENTS_SELECTOR, LATE_FINALIZE_CREDITS_SELECTOR,
    ORACLE_SLASH_WINDOW_SELECTOR, RADICLE_PUBLICATION_SELECTOR, TEE_BOOTSTRAP_SELECTOR,
};

/// Executor cursor that names the next system-tx phase that the block executor
/// expects to consume. With this enum, phase routing does not derive from
/// `self.inner.receipts.len()` when the executor commits Phase 1 in
/// `apply_pre_execution_changes` (pre-execution) and not in the main tx loop.
///
/// Invariants:
/// - Genesis has no begin-zone witnesses and starts at `UserTxs`.
/// - On block `1` (genesis bootstrap), cursor starts at
///   `CycleTick { body_index: 0 }`.
/// - On block `n >= GENESIS_BOOTSTRAP_BLOCK_NUMBER + 1`, cursor starts at
///   `Phase1Preexecuted { body_index: 0, tx_hash, receipt_index: 0 }` after
///   the executor has pre-built and committed the Phase 1 system tx.
/// - The cursor advances exactly once per consumed begin-zone system tx. When
///   it reaches the first non-system tx (or block end), it is `UserTxs`.
/// - Encoded purely in-memory: never serialised, hashed, or part of any
///   wire format or `header.extra_data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemTxPhase {
    /// Phase 1 (`CertifiedParentAccounting`) has been built, verified, and
    /// committed in pre-execution. The proposer-supplied body[`body_index`]
    /// must match `tx_hash` byte-for-byte and is validated without
    /// re-execution.
    Phase1Preexecuted {
        body_index: u8,
        tx_hash: B256,
        receipt_index: u8,
    },
    /// Next expected begin-zone tx is the mandatory (blocks `>= 2`)
    /// `LateFinalizeCredits` phase, ordered immediately after Phase 1.
    LateFinalizeCredits {
        body_index: u8,
    },
    /// Next expected begin-zone tx is Phase 2 (`CycleTick`).
    CycleTick {
        body_index: u8,
    },
    /// Next expected begin-zone tx is the optional Phase 3
    /// (`BoundaryOutcome`). It is emitted only when the header carries a
    /// boundary outcome artifact.
    BoundaryOutcomeOptional {
        body_index: u8,
    },
    /// Next expected begin-zone tx is the optional Phase 3b
    /// (`TeeBootstrap`). It is present only in the one-time bootstrap block.
    TeeBootstrapOptional {
        body_index: u8,
    },
    /// Next expected begin-zone tx is Phase 4 (`OracleSlashWindow`).
    OracleSlashWindow {
        body_index: u8,
    },
    /// Next expected begin-zone tx is the mandatory `HookEvents` receipt carrier.
    HookEvents {
        body_index: u8,
    },
    RadiclePublication {
        body_index: u8,
    },
    /// All begin-zone system txs are consumed. User transactions may execute.
    UserTxs,
    /// Next expected end-zone tx is `CallEventProcessing` (ordinal 14).
    EndBlockCallEvents {
        gas_capacity: u64,
    },
    /// Next expected end-zone tx is `CleanupProcessing` (ordinal 15).
    EndBlockCleanup {
        gas_capacity: u64,
        call_gas_used: u64,
    },
    /// All end-zone system txs are consumed (or block regime has no end-zone txs).
    EndBlock {
        gas_capacity: u64,
    },
}

impl SystemTxPhase {
    pub const fn initial_for_regime(regime: BlockHeightRegime) -> Self {
        match regime {
            BlockHeightRegime::Genesis => Self::UserTxs,
            BlockHeightRegime::GenesisBootstrap => Self::CycleTick { body_index: 0 },
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
            Self::CycleTick { body_index: 0 }
        }
    }

    /// The system-tx selector the cursor expects to consume next, or
    /// `None` if the cursor is `UserTxs` or completed `EndBlock`.
    pub const fn expected_selector(&self) -> Option<SysTxSelector> {
        match self {
            Self::Phase1Preexecuted { .. } => Some(CERTIFIED_PARENT_ACCOUNTING_SELECTOR),
            Self::LateFinalizeCredits { .. } => Some(LATE_FINALIZE_CREDITS_SELECTOR),
            Self::CycleTick { .. } => Some(CYCLE_TICK_SELECTOR),
            Self::BoundaryOutcomeOptional { .. } => Some(BOUNDARY_OUTCOME_SELECTOR),
            Self::TeeBootstrapOptional { .. } => Some(TEE_BOOTSTRAP_SELECTOR),
            Self::OracleSlashWindow { .. } => Some(ORACLE_SLASH_WINDOW_SELECTOR),
            Self::HookEvents { .. } => Some(HOOK_EVENTS_SELECTOR),
            Self::RadiclePublication { .. } => Some(RADICLE_PUBLICATION_SELECTOR),
            Self::EndBlockCallEvents { .. } => Some(CALL_EVENT_PROCESSING_SELECTOR),
            Self::EndBlockCleanup { .. } => Some(CLEANUP_PROCESSING_SELECTOR),
            Self::UserTxs | Self::EndBlock { .. } => None,
        }
    }

    pub fn accepts(&self, input: &SystemTx) -> bool {
        self.expected_selector() == Some(input.selector())
    }

    /// Body index / nonce ordinal of the next expected system tx, or `None` if
    /// the cursor is `UserTxs` or completed `EndBlock`.
    pub const fn body_index(&self) -> Option<u8> {
        match self {
            Self::Phase1Preexecuted { body_index, .. }
            | Self::LateFinalizeCredits { body_index }
            | Self::CycleTick { body_index }
            | Self::BoundaryOutcomeOptional { body_index }
            | Self::TeeBootstrapOptional { body_index }
            | Self::OracleSlashWindow { body_index }
            | Self::HookEvents { body_index }
            | Self::RadiclePublication { body_index } => Some(*body_index),
            Self::EndBlockCallEvents { .. } => Some(CALL_EVENT_PROCESSING_ORDINAL),
            Self::EndBlockCleanup { .. } => Some(CLEANUP_PROCESSING_ORDINAL),
            Self::UserTxs | Self::EndBlock { .. } => None,
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
            Self::LateFinalizeCredits { body_index } => Self::CycleTick {
                body_index: body_index + 1,
            },
            Self::CycleTick { body_index } => {
                if has_boundary_outcome {
                    Self::BoundaryOutcomeOptional {
                        body_index: body_index + 1,
                    }
                } else if has_tee_bootstrap {
                    Self::TeeBootstrapOptional {
                        body_index: body_index + 1,
                    }
                } else {
                    Self::OracleSlashWindow {
                        body_index: body_index + 1,
                    }
                }
            }
            Self::BoundaryOutcomeOptional { body_index } => {
                if has_tee_bootstrap {
                    Self::TeeBootstrapOptional {
                        body_index: body_index + 1,
                    }
                } else {
                    Self::OracleSlashWindow {
                        body_index: body_index + 1,
                    }
                }
            }
            Self::TeeBootstrapOptional { body_index } => Self::OracleSlashWindow {
                body_index: body_index + 1,
            },
            Self::OracleSlashWindow { body_index } => Self::HookEvents {
                body_index: body_index + 1,
            },
            Self::HookEvents { body_index } if has_publication => Self::RadiclePublication {
                body_index: body_index + 1,
            },
            Self::HookEvents { .. } | Self::RadiclePublication { .. } | Self::UserTxs => {
                Self::UserTxs
            }
            Self::EndBlockCallEvents { .. }
            | Self::EndBlockCleanup { .. }
            | Self::EndBlock { .. } => self,
        }
    }
}
