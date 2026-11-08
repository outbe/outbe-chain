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
    let mut inputs = vec![boundary(), bootstrap(), SystemTx::ValidatorLiveness];
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
    begin_inputs.push(SystemTx::ValidatorLiveness);
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
fn split_accepts_empty_and_user_only_layouts() {
    let empty = split_system_layout(&[]).expect("empty splits");
    assert!(empty.is_empty());

    let txs = vec![user_tx(), user_tx()];
    let layout = split_system_layout(&txs).expect("user-only splits");
    assert_eq!(layout.begin.len(), 0);
    assert_eq!(layout.user.len(), 2);
    assert_eq!(layout.end.len(), 0);
}

#[test]
fn split_accepts_block1_cycle_tick_prefix() {
    let txs = vec![system_tx(SystemTx::CycleTick, 0, 1), user_tx()];
    let layout = split_system_layout(&txs).expect("layout splits");
    assert_eq!(
        layout.begin_block_selectors().expect("selectors"),
        vec![CYCLE_TICK_SELECTOR]
    );
    assert_eq!(layout.user.len(), 1);
    assert!(layout.end.is_empty());
}

#[test]
fn split_accepts_block_with_optional_boundary_prefix() {
    let txs = vec![
        system_tx(accounting(), 0, 42),
        system_tx(boundary(), 1, 42),
        system_tx(SystemTx::ValidatorLiveness, 2, 42),
        system_tx(SystemTx::CycleTick, 3, 42),
        user_tx(),
    ];
    let layout = split_system_layout(&txs).expect("layout splits");
    assert_eq!(
        layout.begin_block_selectors().expect("selectors"),
        vec![
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            BOUNDARY_OUTCOME_SELECTOR,
            VALIDATOR_LIVENESS_SELECTOR,
            CYCLE_TICK_SELECTOR,
        ]
    );
    assert_eq!(layout.consensus_txs().unwrap().len(), 3);
    assert_eq!(layout.begin_stage_txs().unwrap().len(), 1);
    assert_eq!(layout.user.len(), 1);
}

#[test]
fn split_rejects_out_of_order_prefix() {
    let txs = vec![
        system_tx(SystemTx::CycleTick, 0, 42),
        system_tx(accounting(), 1, 42),
    ];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::OutOfOrder { .. })
    ));
}

#[test]
fn split_rejects_reserved_tx_in_wrong_suffix_zone() {
    let txs = vec![user_tx(), system_tx(SystemTx::CycleTick, 0, 42)];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::SystemTxInWrongZone {
            actual: BodyZone::EndBlock,
            ..
        })
    ));
}

#[test]
fn split_rejects_reserved_tx_in_middle_zone() {
    let txs = vec![
        system_tx(boundary(), 0, 1),
        user_tx(),
        system_tx(SystemTx::CycleTick, 1, 1),
        user_tx(),
    ];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::MidBlockSystemTx { index: 2 })
    ));
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
        system_tx(SystemTx::ValidatorLiveness, 0, 2),
        system_tx(SystemTx::CycleTick, 1, 2),
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
        system_tx(SystemTx::ValidatorLiveness, 1, 1),
        system_tx(SystemTx::CycleTick, 2, 1),
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

    let block1_no_boundary_txs = vec![system_tx(SystemTx::CycleTick, 0, 1)];
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
fn revert_fails_block_classifies_all_twenty_kinds_per_decision_4a() {
    let hard_fail_kinds = [
        SystemTxKind::CertifiedParentAccounting,
        SystemTxKind::LateFinalizeCredits,
        SystemTxKind::BoundaryOutcome,
        SystemTxKind::TeeBootstrap,
        SystemTxKind::ValidatorLiveness,
        SystemTxKind::RadiclePublication,
        SystemTxKind::Vote,
        SystemTxKind::Update,
        SystemTxKind::EnclaveUpgradeDeadline,
        SystemTxKind::Rewards,
        SystemTxKind::Staking,
        SystemTxKind::Oracle,
        SystemTxKind::CycleTick,
    ];
    let soft_fail_kinds = [
        SystemTxKind::OracleSlash,
        SystemTxKind::HyperlaneSlash,
        SystemTxKind::NodFactory,
        SystemTxKind::GemFactory,
        SystemTxKind::IntexFactory,
        SystemTxKind::CredisFactory,
        SystemTxKind::GemCleanup,
    ];
    assert_eq!(hard_fail_kinds.len() + soft_fail_kinds.len(), 20);
    for kind in hard_fail_kinds {
        assert!(
            kind.revert_fails_block(),
            "{kind:?} must fail the block on revert"
        );
    }
    for kind in soft_fail_kinds {
        assert!(
            !kind.revert_fails_block(),
            "{kind:?} must keep the soft-receipt skip"
        );
    }
}

#[test]
fn validate_active_system_tx_set_rejects_phase3b_outside_block1() {
    let mut txs = vec![
        system_tx(accounting(), 0, 2),
        system_tx(late_credits(), 1, 2),
        system_tx(boundary(), 2, 2),
        system_tx(bootstrap(), 3, 2),
        system_tx(SystemTx::ValidatorLiveness, 4, 2),
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
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));
}

#[test]
fn reserved_address_does_not_collide_with_system_precompiles() {
    let addr_bytes = OUTBE_SYSTEM_TX_ADDRESS.0;
    assert_eq!(addr_bytes[0], 0xff);
    assert_ne!(addr_bytes[19], 0x00);
}

#[test]
fn selectors_require_complete_decode_even_after_a_matching_transaction() {
    let valid = system_tx(SystemTx::CycleTick, 0, 2);
    let mut malformed =
        build_unsigned_system_tx(1, 2, CHAIN_ID, SystemTx::Vote.encode().unwrap()).unwrap();
    malformed.input = Bytes::from_static(b"OSVT\x02\x00");
    let malformed: TransactionSigned = malformed.into_signed(Signature::test_signature()).into();
    let layout = SystemTxLayout {
        begin: vec![&valid, &malformed],
        user: vec![],
        end: vec![],
    };
    assert!(!layout.has_begin_selector(CYCLE_TICK_SELECTOR));
    assert_eq!(
        layout.begin_block_selectors(),
        Err(SystemTxError::UnexpectedBody {
            kind: "Vote",
            len: 1
        })
    );
    let end_layout = SystemTxLayout {
        begin: vec![],
        user: vec![],
        end: vec![&malformed],
    };
    assert_eq!(
        end_layout.end_block_selectors(),
        Err(SystemTxError::UnexpectedBody {
            kind: "Vote",
            len: 1
        })
    );
}

#[test]
fn differing_payloads_have_the_same_structure_and_are_still_duplicates() {
    let left = accounting();
    let mut metadata = sample_metadata();
    metadata.finalized_block_hash = B256::repeat_byte(0x99);
    let right = SystemTx::CertifiedParentAccounting { metadata };
    assert_ne!(left, right);
    assert_eq!(left.selector(), right.selector());
    let left = system_tx(left, 0, 2);
    let right = system_tx(right, 1, 2);
    let layout = SystemTxLayout {
        begin: vec![&left, &right],
        user: vec![],
        end: vec![],
    };
    assert_eq!(
        layout.begin_block_selectors().unwrap(),
        vec![CERTIFIED_PARENT_ACCOUNTING_SELECTOR; 2]
    );
    assert_eq!(
        split_system_layout(&[left, right]).unwrap_err(),
        SystemTxError::OutOfOrder {
            zone: BodyZone::BeginBlock,
            previous: "CertifiedParentAccounting",
            current: "CertifiedParentAccounting",
        }
    );
}
