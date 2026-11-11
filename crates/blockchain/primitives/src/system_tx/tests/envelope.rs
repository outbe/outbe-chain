use super::*;

#[test]
fn build_unsigned_system_tx_sets_deterministic_zero_gas_fields() {
    let input = SystemTx::BeginBlock { step_index: 8 }
        .encode()
        .expect("input encodes");
    let tx = build_unsigned_system_tx(0, 1, CHAIN_ID, input.clone()).expect("tx builds");
    assert_eq!(tx.chain_id, Some(CHAIN_ID));
    assert_eq!(tx.nonce, u64::from(MAX_SYSTEM_TXS_PER_BLOCK));
    assert_eq!(tx.gas_price, 0);
    assert_eq!(tx.gas_limit, 0);
    assert_eq!(
        tx.gas_limit,
        system_tx_visible_gas_limit(input.as_ref()).expect("visible gas computes")
    );
    assert_eq!(tx.to, TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS));
    assert_eq!(tx.value, U256::ZERO);
    assert_eq!(tx.input, input);
}

#[test]
fn signature_hash_is_deterministic_for_identical_inputs() {
    let input = SystemTx::BeginBlock { step_index: 8 }
        .encode()
        .expect("input encodes");
    let a = build_unsigned_system_tx(0, 42, CHAIN_ID, input.clone()).expect("tx builds");
    let b = build_unsigned_system_tx(0, 42, CHAIN_ID, input).expect("tx builds");
    assert_eq!(a.signature_hash(), b.signature_hash());

    let different_block = build_unsigned_system_tx(
        0,
        43,
        CHAIN_ID,
        SystemTx::BeginBlock { step_index: 8 }
            .encode()
            .expect("input encodes"),
    )
    .expect("tx builds");
    assert_ne!(a.signature_hash(), different_block.signature_hash());
}

#[test]
fn envelope_boundaries_fully_decode_calldata_and_require_zero_gas_limit() {
    let malformed = Bytes::from_static(b"OSBB\x03\x08\x00");
    assert_eq!(
        build_unsigned_system_tx(0, 1, CHAIN_ID, malformed.clone()),
        Err(SystemTxError::NonEmptyStagePayload {
            stage: BlockExecutionStage::BeginBlock,
            len: 1
        })
    );
    assert_eq!(
        build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
            ordinal: 0,
            block_number: 1,
            chain_id: CHAIN_ID,
            calldata: malformed,
            gas_limit: 0,
        }),
        Err(SystemTxError::NonEmptyStagePayload {
            stage: BlockExecutionStage::BeginBlock,
            len: 1
        })
    );
    let calldata = SystemTx::BeginBlock { step_index: 8 }.encode().unwrap();
    assert_eq!(
        build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
            ordinal: 0,
            block_number: 1,
            chain_id: CHAIN_ID,
            calldata,
            gas_limit: 21_000,
        }),
        Err(SystemTxError::NonZeroGasLimit { actual: 21_000 })
    );
}

#[test]
fn nonce_is_block_number_times_max_plus_ordinal() {
    assert_eq!(MAX_SYSTEM_TXS_PER_BLOCK, 32);
    assert_eq!(system_tx_nonce(1, 0).expect("nonce"), 32);
    assert_eq!(system_tx_nonce(200_600, 2).expect("nonce"), 6_419_202);
    assert_eq!(system_tx_nonce(2, 24).expect("nonce"), 88);
    assert_eq!(system_tx_nonce(2, 28).expect("nonce"), 92);
    assert_eq!(
        system_tx_nonce(1, MAX_SYSTEM_TXS_PER_BLOCK),
        Err(SystemTxError::OrdinalTooLarge {
            ordinal: MAX_SYSTEM_TXS_PER_BLOCK,
            max: MAX_SYSTEM_TXS_PER_BLOCK,
        })
    );
    assert!(matches!(
        system_tx_nonce(u64::MAX, 31),
        Err(SystemTxError::NonceOverflow { .. })
    ));
}
