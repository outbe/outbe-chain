use super::*;

fn system_tx(input: SystemTx, ordinal: u8, block_number: u64) -> TransactionSigned {
    let input = input.encode().expect("system input encodes");
    build_unsigned_system_tx(ordinal, block_number, CHAIN_ID, input)
        .expect("system tx builds")
        .into_signed(Signature::test_signature())
        .into()
}

fn user_tx() -> TransactionSigned {
    TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce: 0,
        gas_price: 0,
        gas_limit: 21_000,
        to: TxKind::Call(address!("0x4444444444444444444444444444444444444444")),
        value: U256::ZERO,
        input: Bytes::new(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

fn block1_canonical_txs() -> Vec<TransactionSigned> {
    let mut inputs = vec![
        boundary(),
        bootstrap(),
        SystemTx::Consensus {
            step_index: 4,
            payload: Bytes::new(),
        },
    ];
    inputs.extend(begin_stage_inputs());
    inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| system_tx(input, ordinal as u8, 1))
        .collect()
}

fn block2_canonical_txs(with_boundary: bool) -> Vec<TransactionSigned> {
    let mut begin_inputs = vec![accounting(), late_credits()];
    if with_boundary {
        begin_inputs.push(boundary());
    }
    begin_inputs.push(SystemTx::Consensus {
        step_index: 4,
        payload: Bytes::new(),
    });
    begin_inputs.extend(begin_stage_inputs());

    let mut txs: Vec<TransactionSigned> = begin_inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| system_tx(input, ordinal as u8, 2))
        .collect();
    for input in end_stage_inputs() {
        let ordinal = input.nonce_ordinal().expect("end ordinal");
        txs.push(system_tx(input, ordinal, 2));
    }
    txs
}

#[test]
fn validate_active_system_tx_set_accepts_expected_membership() {
    let block0 = split_system_layout(&[]).expect("layout");
    validate_active_system_tx_set(&block0, 0, false, false).expect("genesis ok");

    let block1_txs = block1_canonical_txs();
    let block1 = split_system_layout(&block1_txs).expect("layout");
    validate_active_system_tx_set(&block1, 1, true, true).expect("block 1 V2 ok");

    let block2_txs = block2_canonical_txs(false);
    let block2 = split_system_layout(&block2_txs).expect("layout");
    validate_active_system_tx_set(&block2, 2, false, false).expect("block 2 ok");

    let block2_with_boundary_txs = block2_canonical_txs(true);
    let block2_with_boundary = split_system_layout(&block2_with_boundary_txs).expect("layout");
    validate_active_system_tx_set(&block2_with_boundary, 2, true, false)
        .expect("block 2 boundary ok");
}

#[test]
fn validate_active_system_tx_set_requires_mandatory_and_conditional_kinds() {
    let missing_finalization_txs = vec![
        system_tx(
            SystemTx::Consensus {
                step_index: 4,
                payload: Bytes::new(),
            },
            0,
            2,
        ),
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 1, 2),
    ];
    let missing_finalization = split_system_layout(&missing_finalization_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&missing_finalization, 2, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let mut block1_missing_cycle_tick_txs = block1_canonical_txs();
    block1_missing_cycle_tick_txs.pop();
    let block1_missing_cycle_tick =
        split_system_layout(&block1_missing_cycle_tick_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&block1_missing_cycle_tick, 1, true, true),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let block1_missing_tee_txs = vec![
        system_tx(boundary(), 0, 1),
        system_tx(
            SystemTx::Consensus {
                step_index: 4,
                payload: Bytes::new(),
            },
            1,
            1,
        ),
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 2, 1),
    ];
    let block1_missing_tee = split_system_layout(&block1_missing_tee_txs).expect("layout");
    assert_eq!(
        validate_active_system_tx_set(&block1_missing_tee, 1, true, false),
        Err(SystemTxError::V2Block1MissingTeeBootstrap)
    );

    let mut block1_with_user_txs = block1_canonical_txs();
    block1_with_user_txs.push(user_tx());
    let block1_with_user = split_system_layout(&block1_with_user_txs).expect("layout");
    assert_eq!(
        validate_active_system_tx_set(&block1_with_user, 1, true, true),
        Err(SystemTxError::V2Block1ContainsUserTransactions { actual: 1 })
    );

    let block1_no_boundary_txs = vec![system_tx(SystemTx::BeginBlock { step_index: 8 }, 0, 1)];
    let block1_no_boundary = split_system_layout(&block1_no_boundary_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&block1_no_boundary, 1, false, false),
        Err(SystemTxError::V2Block1MissingBoundaryOutcome)
    ));

    let missing_boundary_txs = block2_canonical_txs(false);
    let missing_boundary = split_system_layout(&missing_boundary_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&missing_boundary, 2, true, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let unexpected_boundary_txs = block2_canonical_txs(true);
    let unexpected_boundary = split_system_layout(&unexpected_boundary_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&unexpected_boundary, 2, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));
}

#[test]
fn revert_policy_lives_in_injected_protocol_steps() {
    assert!(TEST_BLOCK_ORDER
        .consensus
        .iter()
        .all(|step| step.revert_fails_block));
    for (index, step) in TEST_BLOCK_ORDER.begin_block.iter().enumerate() {
        assert_eq!(step.revert_fails_block, !matches!(index, 6 | 7));
    }
    assert!(TEST_BLOCK_ORDER
        .end_block
        .iter()
        .all(|step| !step.revert_fails_block));
}

#[test]
fn validate_active_system_tx_set_rejects_phase3b_outside_block1() {
    let mut txs = vec![
        system_tx(accounting(), 0, 2),
        system_tx(late_credits(), 1, 2),
        system_tx(boundary(), 2, 2),
        system_tx(bootstrap(), 3, 2),
        system_tx(
            SystemTx::Consensus {
                step_index: 4,
                payload: Bytes::new(),
            },
            4,
            2,
        ),
    ];
    for (idx, input) in begin_stage_inputs().into_iter().enumerate() {
        txs.push(system_tx(input, (5 + idx) as u8, 2));
    }
    let layout = split_system_layout(&txs).expect("layout");
    assert_eq!(
        validate_active_system_tx_set(&layout, 2, true, true),
        Err(SystemTxError::TeeBootstrapWrongHeight { block_number: 2 })
    );
    assert!(matches!(
        validate_active_system_tx_set(&layout, 2, true, false),
        Err(SystemTxError::TeeBootstrapWrongHeight { block_number: 2 })
    ));
}

fn validate_active_system_tx_set(
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    has_boundary_outcome: bool,
    _has_tee_bootstrap: bool,
) -> Result<(), SystemTxError> {
    validate_system_tx_set_for_publication(
        &TEST_BLOCK_ORDER,
        layout,
        block_number,
        BlockSystemFlags {
            has_boundary_outcome,
            has_publication: false,
        },
    )
}

#[test]
fn empty_block_order_fails_closed_after_genesis() {
    let layout = split_system_layout(&[]).unwrap();
    for block_number in [1, 2, u64::MAX] {
        assert_eq!(
            validate_system_tx_set_for_publication(
                &BlockOrder::default(),
                &layout,
                block_number,
                BlockSystemFlags::default()
            ),
            Err(SystemTxError::EmptyBlockOrder { block_number })
        );
    }
    assert!(validate_system_tx_set_for_publication(
        &BlockOrder::default(),
        &layout,
        0,
        BlockSystemFlags::default()
    )
    .is_ok());
}

#[test]
fn block_fsm_preserves_static_indices_and_prefix_nonce_positions() {
    for (height, indices) in [(0, vec![]), (1, vec![2, 3, 4]), (2, vec![0, 1, 4])] {
        let mut fsm =
            BlockExecutionFsm::for_block(height, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
        assert_eq!(
            fsm.plan()
                .consensus_steps
                .iter()
                .map(|step| step.id.step_index)
                .collect::<Vec<_>>(),
            indices
        );
        if height == 0 {
            assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
            assert_eq!(fsm.expected_step_id(), None);
            assert_eq!(fsm.expected_ordinal(), None);
            assert_eq!(fsm.finish(), Ok(()));
            assert!(fsm.on_user_tx().is_err());
            assert!(fsm
                .on_system_tx(&SystemTx::BeginBlock { step_index: 0 })
                .is_err());
            continue;
        }
        for (position, index) in indices.iter().enumerate() {
            assert_eq!(fsm.expected_prefix_body_index(), Some(position));
            assert_eq!(fsm.expected_ordinal(), u8::try_from(position).ok());
            assert_eq!(
                fsm.expected_step_id(),
                Some(SystemTxStepId::consensus(*index))
            );
            fsm.on_system_tx(&fixtures::consensus(*index, Bytes::new()))
                .unwrap();
        }
        assert_eq!(fsm.stage(), BlockExecutionStage::BeginBlock);
        for index in 0..9 {
            assert_eq!(
                fsm.expected_prefix_body_index(),
                Some(indices.len() + usize::from(index))
            );
            assert_eq!(
                fsm.expected_ordinal(),
                Some(u8::try_from(indices.len()).unwrap() + index)
            );
            fsm.on_system_tx(&SystemTx::BeginBlock { step_index: index })
                .unwrap();
        }
        if height == 1 {
            assert_eq!(fsm.finish(), Ok(()));
            continue;
        }
        assert_eq!(fsm.stage(), BlockExecutionStage::MidBlock);
        assert_eq!(fsm.expected_prefix_body_index(), None);
        fsm.on_user_tx().unwrap();
        fsm.on_user_tx().unwrap();
        assert_eq!(fsm.user_txs_executed(), 2);
        assert!(fsm.finish().is_err());
        for index in 0..5 {
            assert_eq!(fsm.expected_ordinal(), Some(24 + index));
            fsm.on_system_tx(&SystemTx::EndBlock { step_index: index })
                .unwrap();
        }
        assert_eq!(fsm.stage(), BlockExecutionStage::Completed);
        assert_eq!(fsm.finish(), Ok(()));
    }
}

#[test]
fn rejected_fsm_input_does_not_advance_and_standalone_end_block_is_height_aware() {
    let mut fsm = BlockExecutionFsm::for_block(2, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
    let original = fsm.clone();
    assert!(fsm
        .on_system_tx(&fixtures::consensus(1, Bytes::new()))
        .is_err());
    assert!(fsm.on_user_tx().is_err());
    assert!(fsm.enter_end_block(2).is_err());
    assert!(fsm.finish().is_err());
    assert_eq!(fsm, original);
    fsm.on_system_tx(&accounting()).unwrap();
    let after_parent = fsm.clone();
    assert!(fsm.on_system_tx(&accounting()).is_err());
    assert_eq!(fsm, after_parent);
    for height in [0, 1, 2] {
        let mut mid = BlockExecutionFsm::for_mid_block(&TEST_BLOCK_ORDER);
        assert_eq!(mid.stage(), BlockExecutionStage::MidBlock);
        mid.on_user_tx().unwrap();
        let expected = if height < 2 {
            BlockExecutionStage::Completed
        } else {
            BlockExecutionStage::EndBlock
        };
        assert_eq!(mid.enter_end_block(height), Ok(expected));
        assert_eq!(mid.enter_end_block(height), Ok(expected));
        if height == 2 {
            for tx in end_stage_inputs() {
                mid.on_system_tx(&tx).unwrap();
            }
        }
        assert_eq!(mid.finish(), Ok(()));
    }
}
