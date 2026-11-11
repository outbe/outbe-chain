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
    let txs = vec![
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 0, 1),
        user_tx(),
    ];
    let layout = split_system_layout(&txs).expect("layout splits");
    assert_eq!(
        layout.prefix_steps().expect("selectors"),
        vec![SystemTxStepId::begin_block(8)]
    );
    assert_eq!(layout.user.len(), 1);
    assert!(layout.end.is_empty());
}

#[test]
fn split_accepts_block_with_optional_boundary_prefix() {
    let txs = vec![
        system_tx(accounting(), 0, 42),
        system_tx(boundary(), 1, 42),
        system_tx(
            SystemTx::Consensus {
                step_index: 4,
                payload: Bytes::new(),
            },
            2,
            42,
        ),
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 3, 42),
        user_tx(),
    ];
    let layout = split_system_layout(&txs).expect("layout splits");
    assert_eq!(
        layout.prefix_steps().expect("selectors"),
        vec![
            SystemTxStepId::consensus(0),
            SystemTxStepId::consensus(2),
            SystemTxStepId::consensus(4),
            SystemTxStepId::begin_block(8),
        ]
    );
    assert_eq!(layout.consensus_txs().unwrap().len(), 3);
    assert_eq!(layout.begin_stage_txs().unwrap().len(), 1);
    assert_eq!(layout.user.len(), 1);
}

#[test]
fn split_rejects_out_of_order_prefix() {
    let txs = vec![
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 0, 42),
        system_tx(accounting(), 1, 42),
    ];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::OutOfOrder { .. })
    ));
}

#[test]
fn split_rejects_reserved_tx_in_wrong_suffix_zone() {
    let txs = vec![
        user_tx(),
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 0, 42),
    ];
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
        system_tx(SystemTx::BeginBlock { step_index: 8 }, 1, 1),
        user_tx(),
    ];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::MidBlockSystemTx { index: 2 })
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
    let valid = system_tx(SystemTx::BeginBlock { step_index: 8 }, 0, 2);
    let mut malformed = build_unsigned_system_tx(
        1,
        2,
        CHAIN_ID,
        SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
    )
    .unwrap();
    malformed.input = Bytes::from_static(b"OSBB\x03\x00\x00");
    let malformed: TransactionSigned = malformed.into_signed(Signature::test_signature()).into();
    let layout = SystemTxLayout {
        begin: vec![&valid, &malformed],
        user: vec![],
        end: vec![],
    };
    assert!(!layout.has_consensus_step(0));
    assert_eq!(
        layout.prefix_steps(),
        Err(SystemTxError::NonEmptyStagePayload {
            stage: BlockExecutionStage::BeginBlock,
            len: 1
        })
    );
    let end_layout = SystemTxLayout {
        begin: vec![],
        user: vec![],
        end: vec![&malformed],
    };
    assert_eq!(
        end_layout.end_block_steps(),
        Err(SystemTxError::NonEmptyStagePayload {
            stage: BlockExecutionStage::BeginBlock,
            len: 1
        })
    );
}

#[test]
fn differing_payloads_have_the_same_structure_and_are_still_duplicates() {
    let left = accounting();
    let mut metadata = sample_metadata();
    metadata.finalized_block_hash = B256::repeat_byte(0x99);
    let right = SystemTx::Consensus {
        step_index: 0,
        payload: (metadata).encode().unwrap(),
    };
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
        layout.prefix_steps().unwrap(),
        vec![SystemTxStepId::consensus(0); 2]
    );
    assert_eq!(
        split_system_layout(&[left, right]).unwrap_err(),
        SystemTxError::OutOfOrder {
            zone: BodyZone::BeginBlock,
            previous: SystemTxStepId::consensus(0),
            current: SystemTxStepId::consensus(0),
        }
    );
}
