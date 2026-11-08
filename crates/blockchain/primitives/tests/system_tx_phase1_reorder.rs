//! Phase 1 pre-execution reorder invariants.
//!
//! `SystemTxPhase` and `BlockExecutionFsm` share the begin-zone selector
//! schedule. The cursor stops at `UserTxs`. The FSM continues through the
//! end zone at ordinals 24..=28.

use outbe_primitives::system_tx::*;

const BLOCK_1: u64 = 1;
const BLOCK_2: u64 = 2;
const BLOCK_42: u64 = 42;

/// Block 1 skips Phase 1 and starts at BoundaryOutcome.
#[test]
fn apply_pre_execution_changes_skips_phase1_for_block_1() {
    let cursor = SystemTxPhase::initial_for_block(BLOCK_1, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(
        cursor,
        SystemTxPhase::BoundaryOutcomeOptional { body_index: 0 }
    );
    assert_eq!(cursor.expected_selector(), Some(BOUNDARY_OUTCOME_SELECTOR));
    assert!(!matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }));
}

/// Phase 1 is receipt index 0. CycleTick is the last begin-zone phase.
#[test]
fn phase1_receipt_index_0_before_cycle_tick() {
    let phase1 = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert!(matches!(
        phase1,
        SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            receipt_index: 0,
            ..
        }
    ));
    let selectors = expected_begin_block_selectors(BLOCK_2, false, false);
    let mut cursor = phase1;
    for (index, selector) in selectors.iter().enumerate() {
        assert_eq!(cursor.body_index(), Some(u8::try_from(index).unwrap()));
        assert_eq!(cursor.expected_selector(), Some(*selector));
        if *selector == CYCLE_TICK_SELECTOR {
            assert_eq!(index, 11);
        }
        cursor = cursor.advance_after_commit(false, false);
    }
    assert_eq!(cursor, SystemTxPhase::UserTxs);
}

/// Steady-state selector order is stable with and without BoundaryOutcome.
/// Body index increases by 1. The FSM uses the same begin selectors, then
/// the end-zone ordinals 24..=28.
#[test]
fn phase1_reordering_preserves_body_receipt_order() {
    assert_eq!(
        expected_begin_block_selectors(BLOCK_42, false, false),
        vec![
            *b"OSA3", *b"OSL2", *b"OSVL", *b"OSVT", *b"OSUP", *b"OSEU", *b"OSRW", *b"OSST",
            *b"OSOR", *b"OSOS", *b"OSHL", *b"OSC2",
        ]
    );
    let selectors = expected_begin_block_selectors(BLOCK_42, true, false);
    assert_eq!(
        selectors,
        vec![
            *b"OSA3", *b"OSL2", *b"OSB2", *b"OSVL", *b"OSVT", *b"OSUP", *b"OSEU", *b"OSRW",
            *b"OSST", *b"OSOR", *b"OSOS", *b"OSHL", *b"OSC2",
        ]
    );

    let mut fsm = BlockExecutionFsm::for_block(BLOCK_42, true, false, false);
    let mut cursor = SystemTxPhase::initial_for_block(BLOCK_42, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    for (index, selector) in selectors.iter().enumerate() {
        let index = u8::try_from(index).unwrap();
        assert_eq!(cursor.expected_selector(), Some(*selector));
        assert_eq!(cursor.body_index(), Some(index));
        assert_eq!(fsm.expected_selector(), Some(*selector));
        assert_eq!(fsm.expected_ordinal(), Some(index));
        let kind = fsm.expected_system_tx_kind().unwrap();
        fsm.on_system_tx(kind).unwrap();
        cursor = cursor.advance_after_commit(true, false);
    }
    assert_eq!(cursor, SystemTxPhase::UserTxs);
    assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
    fsm.on_user_tx().unwrap();
    assert_eq!(fsm.user_txs_executed(), 1);

    let end_selectors = expected_end_block_selectors(BLOCK_42);
    assert_eq!(
        end_selectors,
        vec![*b"OSND", *b"OSGM", *b"OSIX", *b"OSCR", *b"OSGC"]
    );
    for (offset, selector) in end_selectors.iter().enumerate() {
        let ordinal = 24 + u8::try_from(offset).unwrap();
        assert_eq!(fsm.expected_selector(), Some(*selector));
        assert_eq!(fsm.expected_ordinal(), Some(ordinal));
        assert!((NOD_FACTORY_ORDINAL..=GEM_CLEANUP_ORDINAL).contains(&ordinal));
        let kind = fsm.expected_system_tx_kind().unwrap();
        fsm.on_system_tx(kind).unwrap();
    }
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    fsm.finish().unwrap();
}

/// A begin-zone kind submitted during the consensus stage is rejected.
#[test]
fn block_execution_fsm_rejects_reordered_system_tx() {
    let mut fsm = BlockExecutionFsm::for_block(BLOCK_42, false, false, false);
    let err = fsm.on_system_tx(SystemTxKind::Vote).unwrap_err();
    assert!(matches!(
        err,
        SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::ConsensusStage,
            ..
        }
    ));
    assert_eq!(
        fsm.expected_system_tx_kind(),
        Some(SystemTxKind::CertifiedParentAccounting)
    );
    assert_eq!(fsm.expected_ordinal(), Some(0));
}

/// Phase 1 is consumed once. The long begin walk reaches UserTxs.
#[test]
fn phase1_executes_exactly_once() {
    let mut cursor = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    let mut saw_phase1 = 0;
    if matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }) {
        saw_phase1 += 1;
    }
    for _ in 0..16 {
        cursor = cursor.advance_after_commit(true, false);
        if matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }) {
            saw_phase1 += 1;
        }
    }
    assert_eq!(saw_phase1, 1, "Phase 1 must be encountered exactly once");
    assert_eq!(cursor, SystemTxPhase::UserTxs);
}
