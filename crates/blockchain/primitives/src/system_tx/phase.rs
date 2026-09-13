use alloy_primitives::B256;

use super::{SystemTxKind, GENESIS_BOOTSTRAP_BLOCK_NUMBER};

/// Executor cursor that names the next system-tx phase the block executor
/// expects to consume. introduces this enum so phase routing no
/// longer derives from `self.inner.receipts.len()` once Phase 1 is committed
/// in `apply_pre_execution_changes` (pre-execution) rather than the main tx
/// loop.
///
/// Invariants:
/// - On block `1` (genesis bootstrap), cursor starts at `CycleTick { body_index: 0 }`.
/// - On block `n >= GENESIS_BOOTSTRAP_BLOCK_NUMBER + 1`, cursor starts at
///   `Phase1Preexecuted { body_index: 0, tx_hash, receipt_index: 0 }` after
///   the executor has pre-built and committed the Phase 1 system tx.
/// - The cursor advances exactly once per consumed begin-zone system tx; on
///   reaching the first non-system tx (or block end) it is `UserTxs`.
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
    LateFinalizeCredits { body_index: u8 },
    /// OCOMP expiry/reset phase, present only once the PoC lifecycle fork is
    /// active and ordered before `CycleTick`.
    OcompLifecycleBegin { body_index: u8 },
    /// Next expected begin-zone tx is Phase 2 (`CycleTick`).
    CycleTick { body_index: u8 },
    /// Mandatory Rewards-owned delivery phase immediately after `CycleTick`.
    RewardsGemDelivery { body_index: u8 },
    /// Next expected begin-zone tx is the optional Phase 3
    /// (`BoundaryOutcome`); only emitted when the header carries a boundary
    /// outcome artifact.
    BoundaryOutcomeOptional { body_index: u8 },
    /// Next expected begin-zone tx is the optional Phase 3b
    /// (`TeeBootstrap`); present only in the one-time bootstrap block.
    TeeBootstrapOptional { body_index: u8 },
    /// Next expected begin-zone tx is Phase 4 (`OracleSlashWindow`).
    OracleSlashWindow { body_index: u8 },
    /// Next expected begin-zone tx is the mandatory `HookEvents` receipt carrier.
    HookEvents { body_index: u8 },
    /// All begin-zone system txs consumed. User transactions may execute until
    /// the optional end-zone transaction is consumed.
    UserTxs,
}

impl SystemTxPhase {
    /// Initial cursor for `block_number` given the configured genesis
    /// bootstrap threshold. Block `1` has no Phase 1 (genesis bootstrap),
    /// so its cursor starts at `CycleTick { body_index: 0 }`. Block `n` with
    /// `n >= genesis_bootstrap_block_number + 1` starts at
    /// `Phase1Preexecuted { body_index: 0, .. }` with a zero placeholder
    /// `tx_hash`; the executor overwrites the placeholder after the Phase 1
    /// preflight commits.
    pub const fn initial_for_block(block_number: u64, genesis_bootstrap_block_number: u64) -> Self {
        Self::initial_for_block_with_ocomp(block_number, genesis_bootstrap_block_number, false)
    }

    pub const fn initial_for_block_with_ocomp(
        block_number: u64,
        genesis_bootstrap_block_number: u64,
        ocomp_lifecycle_active: bool,
    ) -> Self {
        if block_number > genesis_bootstrap_block_number
            && block_number > GENESIS_BOOTSTRAP_BLOCK_NUMBER
        {
            Self::Phase1Preexecuted {
                body_index: 0,
                tx_hash: B256::ZERO,
                receipt_index: 0,
            }
        } else if block_number > 0 && ocomp_lifecycle_active {
            Self::OcompLifecycleBegin { body_index: 0 }
        } else {
            Self::CycleTick { body_index: 0 }
        }
    }

    /// The begin-zone system-tx kind the cursor expects to consume next, or
    /// `None` if the cursor is `UserTxs`.
    pub const fn expected_kind(&self) -> Option<SystemTxKind> {
        match self {
            Self::Phase1Preexecuted { .. } => Some(SystemTxKind::CertifiedParentAccounting),
            Self::LateFinalizeCredits { .. } => Some(SystemTxKind::LateFinalizeCredits),
            Self::OcompLifecycleBegin { .. } => Some(SystemTxKind::OcompLifecycleBegin),
            Self::CycleTick { .. } => Some(SystemTxKind::CycleTick),
            Self::RewardsGemDelivery { .. } => Some(SystemTxKind::RewardsGemDelivery),
            Self::BoundaryOutcomeOptional { .. } => Some(SystemTxKind::BoundaryOutcome),
            Self::TeeBootstrapOptional { .. } => Some(SystemTxKind::TeeBootstrap),
            Self::OracleSlashWindow { .. } => Some(SystemTxKind::OracleSlashWindow),
            Self::HookEvents { .. } => Some(SystemTxKind::HookEvents),
            Self::UserTxs => None,
        }
    }

    /// Body index of the next expected begin-zone system tx, or `None` if
    /// the cursor is `UserTxs`.
    pub const fn body_index(&self) -> Option<u8> {
        match self {
            Self::Phase1Preexecuted { body_index, .. }
            | Self::LateFinalizeCredits { body_index }
            | Self::OcompLifecycleBegin { body_index }
            | Self::CycleTick { body_index }
            | Self::RewardsGemDelivery { body_index }
            | Self::BoundaryOutcomeOptional { body_index }
            | Self::TeeBootstrapOptional { body_index }
            | Self::OracleSlashWindow { body_index }
            | Self::HookEvents { body_index } => Some(*body_index),
            Self::UserTxs => None,
        }
    }

    /// Advance the cursor after a successful begin-zone system-tx commit.
    /// Given the cursor's current variant and whether the current block
    /// carries a boundary-outcome artifact, returns the next cursor
    /// position. Once HookEvents is consumed, the cursor transitions to
    /// `UserTxs`.
    ///
    /// `has_boundary_outcome` controls whether Phase 3
    /// (`BoundaryOutcomeOptional`) is interleaved between
    /// `RewardsGemDelivery` and `OracleSlashWindow`. The flag mirrors the
    /// block-1 invariant:
    /// at block 1, V2 always carries a boundary outcome (genesis bootstrap),
    /// so `has_boundary_outcome = true` is the canonical path there.
    ///
    /// `has_tee_bootstrap` interleaves the optional Phase 3b
    /// (`TeeBootstrapOptional`) after `BoundaryOutcome` (or after
    /// `RewardsGemDelivery` if no boundary outcome) and before
    /// `OracleSlashWindow`. It is true only in the one-time bootstrap block.
    pub const fn advance_after_commit(
        self,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
    ) -> Self {
        self.advance_after_commit_with_ocomp(has_boundary_outcome, has_tee_bootstrap, false)
    }

    pub const fn advance_after_commit_with_ocomp(
        self,
        has_boundary_outcome: bool,
        has_tee_bootstrap: bool,
        ocomp_lifecycle_active: bool,
    ) -> Self {
        match self {
            Self::Phase1Preexecuted { body_index, .. } => Self::LateFinalizeCredits {
                body_index: body_index + 1,
            },
            Self::LateFinalizeCredits { body_index } => {
                if ocomp_lifecycle_active {
                    Self::OcompLifecycleBegin {
                        body_index: body_index + 1,
                    }
                } else {
                    Self::CycleTick {
                        body_index: body_index + 1,
                    }
                }
            }
            Self::OcompLifecycleBegin { body_index } => Self::CycleTick {
                body_index: body_index + 1,
            },
            Self::CycleTick { body_index } => Self::RewardsGemDelivery {
                body_index: body_index + 1,
            },
            Self::RewardsGemDelivery { body_index } => {
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
            Self::HookEvents { .. } | Self::UserTxs => Self::UserTxs,
        }
    }
}
