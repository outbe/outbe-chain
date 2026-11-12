//! Phase 1 ordering uses static step identities and contiguous body ordinals.

use alloy_primitives::Bytes;
use outbe_evm::system_tx::{TEST_BLOCK_ORDER, *};

fn prefix_input(id: SystemTxStepId) -> SystemTx {
    match id.stage {
        BlockExecutionStage::Consensus => SystemTx::Consensus {
            step_index: id.step_index,
            payload: Bytes::new(),
        },
        BlockExecutionStage::BeginBlock => SystemTx::BeginBlock {
            step_index: id.step_index,
        },
        _ => panic!("not a prefix step"),
    }
}

#[test]
fn apply_pre_execution_changes_skips_phase1_for_block_1() {
    let fsm = BlockExecutionFsm::for_block(
        1,
        BlockSystemFlags {
            has_boundary_outcome: true,
            has_publication: false,
        },
        &TEST_BLOCK_ORDER,
    );
    assert_eq!(fsm.expected_prefix_body_index(), Some(0));
    assert_eq!(
        fsm.expected_step_id(),
        Some(SystemTxStepId::consensus(
            CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX
        ))
    );
    assert!(!fsm
        .plan()
        .expected_prefix_steps()
        .contains(&SystemTxStepId::consensus(
            CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX
        )));
}

#[test]
fn phase1_body_index_0_before_cycle_tick() {
    let mut fsm = BlockExecutionFsm::for_block(2, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    assert_eq!(fsm.expected_prefix_body_index(), Some(0));
    assert_eq!(
        fsm.expected_step_id(),
        Some(SystemTxStepId::consensus(
            CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX
        ))
    );
    for (index, id) in fsm.plan().expected_prefix_steps().into_iter().enumerate() {
        assert_eq!(fsm.expected_prefix_body_index(), Some(index));
        assert_eq!(fsm.expected_step_id(), Some(id));
        if id == SystemTxStepId::begin_block(8) {
            assert_eq!(index, 11);
        }
        fsm.on_system_tx(&prefix_input(id)).unwrap();
    }
    assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
}

#[test]
fn phase1_reordering_preserves_body_order_and_suffix_ordinals() {
    for has_boundary_outcome in [false, true] {
        let flags = BlockSystemFlags {
            has_boundary_outcome,
            has_publication: false,
        };
        let mut expected = vec![
            SystemTxStepId::consensus(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX),
            SystemTxStepId::consensus(CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX),
        ];
        if has_boundary_outcome {
            expected.push(SystemTxStepId::consensus(
                CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
            ));
        }
        expected.push(SystemTxStepId::consensus(
            CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
        ));
        expected.extend((0..9).map(SystemTxStepId::begin_block));
        let mut fsm = BlockExecutionFsm::for_block(42, flags, &TEST_BLOCK_ORDER);
        assert_eq!(fsm.plan().expected_prefix_steps(), expected);
        for (index, id) in expected.into_iter().enumerate() {
            assert_eq!(fsm.expected_step_id(), Some(id));
            assert_eq!(fsm.expected_prefix_body_index(), Some(index));
            assert_eq!(fsm.expected_ordinal(), Some(u8::try_from(index).unwrap()));
            fsm.on_system_tx(&prefix_input(id)).unwrap();
        }
        assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
        fsm.on_user_tx().unwrap();
        assert_eq!(fsm.user_txs_executed(), 1);
        assert_eq!(
            fsm.plan().expected_end_block_steps(),
            (0..5).map(SystemTxStepId::end_block).collect::<Vec<_>>()
        );
        for step_index in 0..5 {
            assert_eq!(
                fsm.expected_step_id(),
                Some(SystemTxStepId::end_block(step_index))
            );
            assert_eq!(fsm.expected_ordinal(), end_block_nonce_ordinal(step_index));
            assert_eq!(fsm.expected_ordinal(), Some(24 + step_index));
            fsm.on_system_tx(&SystemTx::EndBlock { step_index })
                .unwrap();
        }
        assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
        fsm.finish().unwrap();
    }
}

#[test]
fn block_execution_fsm_rejects_reordered_system_tx() {
    let mut fsm = BlockExecutionFsm::for_block(42, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    let before = fsm.clone();
    assert!(matches!(
        fsm.on_system_tx(&SystemTx::BeginBlock { step_index: 0 }),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::Consensus,
            ..
        })
    ));
    assert_eq!(fsm, before);
    assert_eq!(
        fsm.expected_step_id(),
        Some(SystemTxStepId::consensus(
            CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX
        ))
    );
    assert_eq!(fsm.expected_ordinal(), Some(0));
}

#[test]
fn phase1_executes_exactly_once() {
    let mut fsm = BlockExecutionFsm::for_block(
        2,
        BlockSystemFlags {
            has_boundary_outcome: true,
            has_publication: false,
        },
        &TEST_BLOCK_ORDER,
    );
    let mut saw_phase1 = 0;
    for id in fsm.plan().expected_prefix_steps() {
        if id == SystemTxStepId::consensus(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX) {
            saw_phase1 += 1;
        }
        fsm.on_system_tx(&prefix_input(id)).unwrap();
    }
    assert_eq!(saw_phase1, 1);
    assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
}
