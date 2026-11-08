//! Phase 1 atomicity, cursor semantics, and the 4-stage block FSM.
//!
//! The `SystemTxPhase` cursor walks the begin zone and stops at `UserTxs`.
//! `BlockExecutionFsm` is the machine that also consumes the end zone.

use outbe_primitives::system_tx::{
    BlockExecutionFsm, BlockExecutionStage, SystemTxError, SystemTxKind, SystemTxPhase,
    CERTIFIED_PARENT_ACCOUNTING_SELECTOR, GENESIS_BOOTSTRAP_BLOCK_NUMBER,
    LATE_FINALIZE_CREDITS_SELECTOR,
};

const BLOCK_1: u64 = 1;
const BLOCK_2: u64 = 2;

fn walk_begin(
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
) -> SystemTxPhase {
    let selectors = outbe_primitives::system_tx::expected_begin_block_selectors(
        block_number,
        has_boundary_outcome,
        has_tee_bootstrap,
    );
    let mut cursor = SystemTxPhase::initial_for_block(block_number, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    for (index, selector) in selectors.iter().enumerate() {
        assert_eq!(cursor.expected_selector(), Some(*selector));
        assert_eq!(cursor.body_index(), Some(u8::try_from(index).unwrap()));
        cursor = cursor.advance_after_commit(has_boundary_outcome, has_tee_bootstrap);
    }
    assert_eq!(cursor, SystemTxPhase::UserTxs);
    cursor
}

/// Block 1 starts at BoundaryOutcome and walks the 12-tx bootstrap schedule.
#[test]
fn block_1_body_index_map_starts_at_boundary_outcome() {
    let cursor = SystemTxPhase::initial_for_block(BLOCK_1, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(
        cursor,
        SystemTxPhase::BoundaryOutcomeOptional { body_index: 0 }
    );
    assert_eq!(
        cursor.expected_selector(),
        Some(outbe_primitives::system_tx::BOUNDARY_OUTCOME_SELECTOR)
    );
    assert!(!matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }));
    walk_begin(BLOCK_1, true, true);
}

/// OracleSlash follows the optional boundary by one body index.
#[test]
fn oracle_slash_body_index_follows_optional_boundary_outcome() {
    let with_boundary =
        outbe_primitives::system_tx::expected_begin_block_selectors(BLOCK_2, true, false);
    let without =
        outbe_primitives::system_tx::expected_begin_block_selectors(BLOCK_2, false, false);
    let slash_with = with_boundary
        .iter()
        .position(|selector| *selector == outbe_primitives::system_tx::ORACLE_SLASH_SELECTOR)
        .unwrap();
    let slash_without = without
        .iter()
        .position(|selector| *selector == outbe_primitives::system_tx::ORACLE_SLASH_SELECTOR)
        .unwrap();
    assert_eq!(slash_with, slash_without + 1);

    let mut cursor = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    for _ in 0..slash_with {
        cursor = cursor.advance_after_commit(true, false);
    }
    assert_eq!(
        cursor,
        SystemTxPhase::OracleSlash {
            body_index: u8::try_from(slash_with).unwrap()
        }
    );

    let mut cursor = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    for _ in 0..slash_without {
        cursor = cursor.advance_after_commit(false, false);
    }
    assert_eq!(
        cursor,
        SystemTxPhase::OracleSlash {
            body_index: u8::try_from(slash_without).unwrap()
        }
    );
}

/// Block 1 never enters Phase 1. UserTxs does not advance into the end zone.
#[test]
fn debug_assert_system_tx_phase_cursor_after_preexec_for_block1_and_phase1() {
    let cursor = SystemTxPhase::initial_for_block(BLOCK_1, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert!(matches!(
        cursor,
        SystemTxPhase::BoundaryOutcomeOptional { body_index: 0 }
    ));
    assert!(!matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }));
    let cursor2 = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert!(matches!(cursor2, SystemTxPhase::Phase1Preexecuted { .. }));
    let user = SystemTxPhase::UserTxs;
    assert_eq!(
        user.advance_after_commit(true, false),
        SystemTxPhase::UserTxs
    );
    assert_eq!(
        user.advance_after_commit(false, false),
        SystemTxPhase::UserTxs
    );
    assert_eq!(user.body_index(), None);
    assert_eq!(user.expected_selector(), None);
}

/// Phase 1 names CertifiedParentAccounting, then LateFinalizeCredits.
#[test]
fn preexecuted_phase1_body_tx_is_validated_but_not_reexecuted() {
    let cursor = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(
        cursor.expected_selector(),
        Some(CERTIFIED_PARENT_ACCOUNTING_SELECTOR),
    );
    let after = cursor.advance_after_commit(true, false);
    assert_eq!(
        after.expected_selector(),
        Some(LATE_FINALIZE_CREDITS_SELECTOR)
    );
}

fn drive_fsm(
    block_number: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
    user_txs: usize,
) -> BlockExecutionFsm {
    let mut fsm =
        BlockExecutionFsm::for_block(block_number, has_boundary_outcome, has_tee_bootstrap, false);
    let begin_kinds = fsm.plan().begin_zone_kinds();
    let end_kinds = fsm.plan().end_block_kinds.clone();
    for kind in begin_kinds {
        assert_eq!(fsm.expected_system_tx_kind(), Some(kind));
        fsm.on_system_tx(kind).unwrap();
    }
    if user_txs == 0 {
        assert_ne!(fsm.stage(), BlockExecutionStage::MidBlock);
    } else {
        assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
        for _ in 0..user_txs {
            fsm.on_user_tx().unwrap();
        }
        assert_eq!(fsm.user_txs_executed(), user_txs);
    }
    for kind in end_kinds {
        assert_eq!(fsm.expected_system_tx_kind(), Some(kind));
        assert_eq!(fsm.expected_ordinal(), kind.nonce_ordinal());
        fsm.on_system_tx(kind).unwrap();
    }
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    fsm.finish().unwrap();
    fsm
}

#[test]
fn block_execution_fsm_completes_block1_without_user_or_end_zone() {
    let mut fsm = drive_fsm(BLOCK_1, true, true, 0);
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    assert!(matches!(
        fsm.on_user_tx(),
        Err(SystemTxError::UnexpectedUserTx {
            stage: BlockExecutionStage::Completed
        })
    ));
    assert!(matches!(
        fsm.on_system_tx(SystemTxKind::NodFactory),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::Completed,
            ..
        })
    ));
}

#[test]
fn block_execution_fsm_completes_steady_block_through_end_zone() {
    let fsm = drive_fsm(BLOCK_2, true, false, 1);
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    assert_eq!(fsm.user_txs_executed(), 1);
    assert_eq!(fsm.plan().end_block_kinds.len(), 5);
}
