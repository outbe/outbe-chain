use super::*;

#[test]
fn build_unsigned_system_tx_sets_deterministic_fields() {
    let input = SystemTx::CycleTick.encode().expect("input encodes");
    let tx = build_unsigned_system_tx(0, 1, CHAIN_ID, input.clone()).expect("tx builds");
    assert_eq!(tx.chain_id, Some(CHAIN_ID));
    assert_eq!(tx.nonce, u64::from(MAX_SYSTEM_TXS_PER_BLOCK));
    assert_eq!(tx.gas_price, 0);
    assert_eq!(
        tx.gas_limit,
        system_tx_visible_gas_limit(input.as_ref()).expect("visible gas computes")
    );
    assert!(tx.gas_limit >= SYSTEM_TX_VISIBLE_GAS_FLOOR);
    assert!(tx.gas_limit < SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    assert_eq!(tx.to, TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS));
    assert_eq!(tx.value, U256::ZERO);
    assert_eq!(tx.input, input);
}

#[test]
fn signature_hash_is_deterministic_for_identical_inputs() {
    let input = SystemTx::CycleTick.encode().expect("input encodes");
    let a = build_unsigned_system_tx(0, 42, CHAIN_ID, input.clone()).expect("tx builds");
    let b = build_unsigned_system_tx(0, 42, CHAIN_ID, input).expect("tx builds");
    assert_eq!(a.signature_hash(), b.signature_hash());
    // Pre-refactor EIP-155 signing RLP: nonce=672, gas=21080, chain_id=2026,
    // reserved recipient and exact OSC2 version-2 calldata.
    assert_eq!(
        a.signature_hash(),
        alloy_primitives::b256!("42df2903653441a11bd3e216d487882d0f2fdb1485d5313d7a52e00703033176")
    );

    let different_block = build_unsigned_system_tx(
        0,
        43,
        CHAIN_ID,
        SystemTx::CycleTick.encode().expect("input encodes"),
    )
    .expect("tx builds");
    assert_ne!(a.signature_hash(), different_block.signature_hash());
}

#[test]
fn envelope_boundaries_fully_decode_calldata_and_check_intrinsic_gas() {
    let malformed = Bytes::from_static(b"OSC2\x02\x00");
    assert_eq!(
        build_unsigned_system_tx(0, 1, CHAIN_ID, malformed.clone()),
        Err(SystemTxError::UnexpectedBody {
            kind: "CycleTick",
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
        Err(SystemTxError::UnexpectedBody {
            kind: "CycleTick",
            len: 1
        })
    );
    let calldata = SystemTx::CycleTick.encode().unwrap();
    let intrinsic_gas = system_tx_intrinsic_gas(&calldata).unwrap();
    assert_eq!(
        build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
            ordinal: 0,
            block_number: 1,
            chain_id: CHAIN_ID,
            calldata,
            gas_limit: intrinsic_gas - 1,
        }),
        Err(SystemTxError::GasLimitBelowIntrinsic {
            gas_limit: intrinsic_gas - 1,
            intrinsic_gas
        })
    );
}

#[test]
fn nonce_is_block_number_times_max_plus_ordinal() {
    assert_eq!(system_tx_nonce(1, 0).expect("nonce"), 16);
    assert_eq!(system_tx_nonce(200_600, 2).expect("nonce"), 3_209_602);
    assert!(matches!(
        system_tx_nonce(u64::MAX, 15),
        Err(SystemTxError::NonceOverflow { .. })
    ));
}
