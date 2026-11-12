//! Atomic prefix consumption and the unified block execution FSM.

#[path = "system_tx_helpers/input_for_step.rs"]
mod input_for_step;

use input_for_step::input_for_step;

use alloy_primitives::Bytes;
use outbe_evm::system_tx::{TEST_BLOCK_ORDER, *};

const BLOCK_1: u64 = 1;
const BLOCK_2: u64 = 2;

fn walk_prefix(block_number: u64, flags: BlockSystemFlags) -> BlockExecutionFsm {
    let mut fsm = BlockExecutionFsm::for_block(block_number, flags, &TEST_BLOCK_ORDER);
    for (index, id) in fsm.plan().expected_prefix_steps().into_iter().enumerate() {
        assert_eq!(fsm.expected_step_id(), Some(id));
        assert_eq!(fsm.expected_prefix_body_index(), Some(index));
        assert_eq!(fsm.expected_ordinal(), Some(u8::try_from(index).unwrap()));
        fsm.on_system_tx(&input_for_step(id)).unwrap();
    }
    assert_eq!(fsm.expected_prefix_body_index(), None);
    fsm
}

#[test]
fn block_1_body_index_map_starts_at_boundary_outcome() {
    let flags = BlockSystemFlags {
        has_boundary_outcome: true,
        has_publication: false,
    };
    let fsm = BlockExecutionFsm::for_block(BLOCK_1, flags, &TEST_BLOCK_ORDER);
    assert_eq!(fsm.stage(), BlockExecutionStage::Consensus);
    assert_eq!(fsm.expected_prefix_body_index(), Some(0));
    assert_eq!(
        fsm.expected_step_id(),
        Some(SystemTxStepId::consensus(
            CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX
        ))
    );
    assert_eq!(fsm.plan().expected_prefix_steps().len(), 12);
    assert_eq!(
        walk_prefix(BLOCK_1, flags).stage(),
        BlockExecutionStage::Completed
    );
}

#[test]
fn oracle_slash_body_index_follows_optional_boundary_outcome() {
    let slash = SystemTxStepId::begin_block(6);
    let slash_index = |has_boundary_outcome| {
        let flags = BlockSystemFlags {
            has_boundary_outcome,
            has_publication: false,
        };
        let mut fsm = BlockExecutionFsm::for_block(BLOCK_2, flags, &TEST_BLOCK_ORDER);
        for id in fsm.plan().expected_prefix_steps() {
            if id == slash {
                assert_eq!(fsm.expected_step_id(), Some(slash));
                return fsm.expected_prefix_body_index().unwrap();
            }
            fsm.on_system_tx(&input_for_step(id)).unwrap();
        }
        panic!("OracleSlash must be scheduled")
    };
    assert_eq!(slash_index(true), slash_index(false) + 1);
}

#[test]
fn rejected_phase1_input_does_not_advance_the_fsm() {
    let mut fsm =
        BlockExecutionFsm::for_block(BLOCK_2, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    let before = fsm.clone();
    let wrong = input_for_step(SystemTxStepId::consensus(
        CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
    ));
    assert!(matches!(
        fsm.on_system_tx(&wrong),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::Consensus,
            ..
        })
    ));
    assert_eq!(fsm, before);
}

/// The witness consumes the parent-accounting step once; replay cannot advance it again.
#[test]
fn preexecuted_phase1_body_tx_is_consumed_once() {
    let mut fsm =
        BlockExecutionFsm::for_block(BLOCK_2, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    let input = SystemTx::Consensus {
        step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
        payload: Bytes::new(),
    };
    assert_eq!(fsm.expected_step_id(), Some(input.step_id()));
    fsm.on_system_tx(&input).unwrap();
    assert_eq!(
        fsm.expected_step_id(),
        Some(SystemTxStepId::consensus(
            CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX
        ))
    );
    assert_eq!(fsm.expected_prefix_body_index(), Some(1));
    let after = fsm.clone();
    assert!(fsm.on_system_tx(&input).is_err());
    assert_eq!(fsm, after);
}

fn drive_fsm(block_number: u64, user_txs: usize) -> BlockExecutionFsm {
    let flags = BlockSystemFlags {
        has_boundary_outcome: true,
        has_publication: false,
    };
    let mut fsm = walk_prefix(block_number, flags);
    if user_txs == 0 {
        assert_ne!(fsm.stage(), BlockExecutionStage::MidBlock);
    } else {
        assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
        for _ in 0..user_txs {
            fsm.on_user_tx().unwrap();
        }
        assert_eq!(fsm.user_txs_executed(), user_txs);
    }
    for id in fsm.plan().expected_end_block_steps() {
        let input = input_for_step(id);
        assert_eq!(fsm.expected_step_id(), Some(id));
        assert_eq!(fsm.expected_ordinal(), input.nonce_ordinal());
        fsm.on_system_tx(&input).unwrap();
    }
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    fsm.finish().unwrap();
    fsm
}

#[test]
fn block_execution_fsm_completes_block1_without_user_or_end_zone() {
    let mut fsm = drive_fsm(BLOCK_1, 0);
    assert!(matches!(
        fsm.on_user_tx(),
        Err(SystemTxError::UnexpectedUserTx {
            stage: BlockExecutionStage::Completed
        })
    ));
    assert!(matches!(
        fsm.on_system_tx(&SystemTx::EndBlock { step_index: 0 }),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::Completed,
            ..
        })
    ));
}

#[test]
fn block_execution_fsm_completes_steady_block_through_end_zone() {
    let fsm = drive_fsm(BLOCK_2, 1);
    assert_eq!(fsm.user_txs_executed(), 1);
    assert_eq!(fsm.plan().end_block_steps.len(), 5);
}
