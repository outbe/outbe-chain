//! Consensus-safety checks for the unified FSM: deterministic traces,
//! monotonic body positions, atomic rejection, and terminal completion.

use alloy_primitives::Bytes;
use outbe_primitives::{system_tx::*, test_utils::TEST_BLOCK_ORDER};
use proptest::prelude::*;

fn input_for_step(id: SystemTxStepId) -> SystemTx {
    match id.stage {
        BlockExecutionStage::Consensus => SystemTx::Consensus {
            step_index: id.step_index,
            payload: Bytes::new(),
        },
        BlockExecutionStage::BeginBlock => SystemTx::BeginBlock {
            step_index: id.step_index,
        },
        BlockExecutionStage::EndBlock => SystemTx::EndBlock {
            step_index: id.step_index,
        },
        _ => panic!("not a system step"),
    }
}

#[test]
fn execution_stage_set_includes_completed() {
    let stages = [
        BlockExecutionStage::Consensus,
        BlockExecutionStage::BeginBlock,
        BlockExecutionStage::MidBlock,
        BlockExecutionStage::EndBlock,
        BlockExecutionStage::Completed,
    ];
    for stage in stages {
        match stage {
            BlockExecutionStage::Consensus
            | BlockExecutionStage::BeginBlock
            | BlockExecutionStage::MidBlock
            | BlockExecutionStage::EndBlock
            | BlockExecutionStage::Completed => {}
        }
    }
    assert_eq!(stages.len(), 5);
}

#[test]
fn completed_stage_has_no_envelope_and_cannot_advance_to_users() {
    for has_boundary_outcome in [false, true] {
        for has_publication in [false, true] {
            let flags = BlockSystemFlags {
                has_boundary_outcome,
                has_publication,
            };
            let mut fsm = BlockExecutionFsm::for_block(1, flags, &TEST_BLOCK_ORDER);
            for id in fsm.plan().expected_prefix_steps() {
                fsm.on_system_tx(&input_for_step(id)).unwrap();
            }
            assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
            assert_eq!(fsm.expected_step_id(), None);
            assert_eq!(fsm.expected_prefix_body_index(), None);
            assert_eq!(fsm.expected_ordinal(), None);
            let completed = fsm.clone();
            assert!(fsm.on_user_tx().is_err());
            assert!(fsm
                .on_system_tx(&SystemTx::EndBlock { step_index: 0 })
                .is_err());
            assert_eq!(
                fsm.enter_end_block(1).unwrap(),
                BlockExecutionStage::Completed
            );
            assert_eq!(fsm, completed);
            fsm.finish().unwrap();
        }
    }
}

#[test]
fn genesis_block_starts_completed_without_phase1() {
    let fsm = BlockExecutionFsm::for_block(0, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    assert!(fsm.plan().expected_prefix_steps().is_empty());
    assert!(fsm.plan().expected_end_block_steps().is_empty());
    fsm.finish().unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn proptest_system_tx_transitions_are_deterministic(
        block_number in 0u64..1000,
        has_boundary_outcome in any::<bool>(),
        has_publication in any::<bool>(),
        inputs in proptest::collection::vec((0u8..3, 0u8..10), 0..32),
    ) {
        let flags = BlockSystemFlags { has_boundary_outcome, has_publication };
        let walk = || {
            let mut fsm = BlockExecutionFsm::for_block(block_number, flags, &TEST_BLOCK_ORDER);
            let mut trace = Vec::new();
            for &(stage, step_index) in &inputs {
                let tx = match stage {
                    0 => SystemTx::Consensus { step_index, payload: Bytes::new() },
                    1 => SystemTx::BeginBlock { step_index },
                    _ => SystemTx::EndBlock { step_index },
                };
                let before = fsm.clone();
                let result = fsm.on_system_tx(&tx);
                if result.is_err() { assert_eq!(fsm, before); }
                trace.push((result, fsm.clone()));
            }
            trace
        };
        prop_assert_eq!(walk(), walk());
        prop_assert_eq!(walk(), walk());
    }

    #[test]
    fn proptest_for_block_is_deterministic(
        block_number in 0u64..1_000_000,
        has_boundary_outcome in any::<bool>(),
        has_publication in any::<bool>(),
    ) {
        let flags = BlockSystemFlags { has_boundary_outcome, has_publication };
        let a = BlockExecutionFsm::for_block(block_number, flags, &TEST_BLOCK_ORDER);
        let b = BlockExecutionFsm::for_block(block_number, flags, &TEST_BLOCK_ORDER);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn proptest_expected_step_is_stable_across_the_full_walk(
        block_number in 0u64..1000,
        has_boundary_outcome in any::<bool>(),
        has_publication in any::<bool>(),
    ) {
        let flags = BlockSystemFlags { has_boundary_outcome, has_publication };
        let mut fsm = BlockExecutionFsm::for_block(block_number, flags, &TEST_BLOCK_ORDER);
        let plan = fsm.plan().clone();
        for id in plan.expected_prefix_steps().into_iter().chain(plan.expected_end_block_steps()) {
            prop_assert_eq!(fsm.expected_step_id(), Some(id));
            let expected = fsm.expected_step().unwrap();
            prop_assert_eq!(Some(expected), fsm.expected_step());
            let input = input_for_step(id);
            prop_assert!(expected.matches_tx(&input));
            prop_assert_eq!(input.selector(), expected.selector());
            fsm.on_system_tx(&input).unwrap();
        }
        prop_assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
        prop_assert_eq!(fsm.expected_step_id(), None);
        prop_assert_eq!(fsm.expected_ordinal(), None);
        prop_assert!(fsm.finish().is_ok());
    }
}

#[test]
fn body_index_monotonic_across_prefix_consumption() {
    for block in [1u64, 2, 7, 42, 1000] {
        let flags = BlockSystemFlags {
            has_boundary_outcome: true,
            has_publication: true,
        };
        let mut fsm = BlockExecutionFsm::for_block(block, flags, &TEST_BLOCK_ORDER);
        let steps = fsm.plan().expected_prefix_steps();
        for (index, id) in steps.iter().copied().enumerate() {
            assert_eq!(fsm.expected_prefix_body_index(), Some(index));
            fsm.on_system_tx(&input_for_step(id)).unwrap();
            let next = (index + 1 < steps.len()).then_some(index + 1);
            assert_eq!(fsm.expected_prefix_body_index(), next);
        }
        assert_eq!(fsm.expected_prefix_body_index(), None);
    }
}

#[test]
fn end_block_chain_follows_user_transactions_at_reserved_ordinals() {
    let mut fsm = BlockExecutionFsm::for_mid_block(&TEST_BLOCK_ORDER);
    fsm.on_user_tx().unwrap();
    assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
    assert_eq!(
        fsm.enter_end_block(2).unwrap(),
        BlockExecutionStage::EndBlock
    );
    for step_index in 0..5 {
        assert_eq!(
            fsm.expected_step_id(),
            Some(SystemTxStepId::end_block(step_index))
        );
        assert_eq!(fsm.expected_ordinal(), end_block_nonce_ordinal(step_index));
        assert_eq!(fsm.expected_ordinal(), Some(24 + step_index));
        assert_eq!(fsm.expected_prefix_body_index(), None);
        fsm.on_system_tx(&SystemTx::EndBlock { step_index })
            .unwrap();
    }
    assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
    fsm.finish().unwrap();
}

#[test]
fn block_execution_fsm_finish_requires_the_scheduled_stages() {
    let mut fsm = BlockExecutionFsm::for_block(2, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    assert_eq!(fsm.stage(), BlockExecutionStage::Consensus);
    assert!(matches!(
        fsm.finish(),
        Err(SystemTxError::IncompleteBlockExecution {
            stage: BlockExecutionStage::Consensus,
            ..
        })
    ));
    assert!(matches!(
        fsm.on_user_tx(),
        Err(SystemTxError::UnexpectedUserTx {
            stage: BlockExecutionStage::Consensus
        })
    ));
    for id in fsm.plan().expected_prefix_steps() {
        fsm.on_system_tx(&input_for_step(id)).unwrap();
    }
    assert!(fsm.finish().is_err());
    fsm.on_user_tx().unwrap();
    assert!(fsm.finish().is_err());
    fsm.on_system_tx(&SystemTx::EndBlock { step_index: 0 })
        .unwrap();
    assert!(fsm.finish().is_err());
    for step_index in 1..5 {
        fsm.on_system_tx(&SystemTx::EndBlock { step_index })
            .unwrap();
    }
    fsm.finish().unwrap();
}
