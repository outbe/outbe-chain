use super::*;

fn test_signer(seed: u8) -> OutbeEvmSigner {
    OutbeEvmSigner::from_secret_bytes([seed; 32]).expect("valid test signer")
}

fn phase1_calldata() -> Bytes {
    input_for(SystemTxKind::CertifiedParentAccounting)
        .encode()
        .expect("phase1 input encodes")
}

fn signed_phase1(
    signer: &OutbeEvmSigner,
    block_number: u64,
    chain_id: u64,
    calldata: Bytes,
) -> TransactionSigned {
    let unsigned = build_unsigned_system_tx(
        SystemTxKind::CertifiedParentAccounting,
        0,
        block_number,
        chain_id,
        calldata,
    )
    .expect("phase1 tx builds");
    signer.sign_unsigned(unsigned).expect("phase1 signs")
}

#[test]
fn phase1_witness_validation_accepts_canonical_signed_tx() {
    let signer = test_signer(1);
    let calldata = phase1_calldata();
    let signed = signed_phase1(&signer, 42, CHAIN_ID, calldata.clone());

    let validated =
        validate_phase1_witness_against(&signed, calldata.as_ref(), signer.address(), CHAIN_ID, 42)
            .expect("canonical phase1 witness validates");

    assert_eq!(validated, signed.signature_hash());
}

#[test]
fn phase1_witness_validation_rejects_wrong_signer() {
    let signer = test_signer(1);
    let other = test_signer(2);
    let calldata = phase1_calldata();
    let signed = signed_phase1(&signer, 42, CHAIN_ID, calldata.clone());

    let err =
        validate_phase1_witness_against(&signed, calldata.as_ref(), other.address(), CHAIN_ID, 42)
            .expect_err("wrong proposer must be rejected");

    assert!(matches!(
        err,
        SystemTxError::Phase1SignerMismatch { expected, actual }
            if expected == other.address() && actual == signer.address()
    ));
}

#[test]
fn phase1_witness_validation_rejects_wrong_chain_id_and_nonce() {
    let signer = test_signer(1);
    let calldata = phase1_calldata();
    let signed = signed_phase1(&signer, 42, CHAIN_ID, calldata.clone());

    let wrong_chain = validate_phase1_witness_against(
        &signed,
        calldata.as_ref(),
        signer.address(),
        CHAIN_ID + 1,
        42,
    )
    .expect_err("wrong chain id must be rejected");
    assert!(matches!(
        wrong_chain,
        SystemTxError::Phase1ChainIdMismatch { expected, actual }
            if expected == CHAIN_ID + 1 && actual == Some(CHAIN_ID)
    ));

    let wrong_nonce =
        validate_phase1_witness_against(&signed, calldata.as_ref(), signer.address(), CHAIN_ID, 43)
            .expect_err("wrong block-number nonce must be rejected");
    assert!(matches!(
        wrong_nonce,
        SystemTxError::Phase1NonceMismatch { .. }
    ));
}

#[test]
fn phase1_witness_validation_rejects_noncanonical_envelope_shape() {
    let signer = test_signer(1);
    let calldata = phase1_calldata();
    let base = build_unsigned_system_tx(
        SystemTxKind::CertifiedParentAccounting,
        0,
        42,
        CHAIN_ID,
        calldata.clone(),
    )
    .expect("phase1 tx builds");

    let mut wrong_gas = base.clone();
    wrong_gas.gas_limit = wrong_gas.gas_limit.saturating_add(1);
    let signed_wrong_gas = signer.sign_unsigned(wrong_gas).expect("signs");
    assert!(matches!(
        validate_phase1_witness_against(
            &signed_wrong_gas,
            calldata.as_ref(),
            signer.address(),
            CHAIN_ID,
            42
        ),
        Err(SystemTxError::Phase1GasLimitMismatch { .. })
    ));

    let mut wrong_value = base.clone();
    wrong_value.value = U256::from(1);
    let signed_wrong_value = signer.sign_unsigned(wrong_value).expect("signs");
    assert!(matches!(
        validate_phase1_witness_against(
            &signed_wrong_value,
            calldata.as_ref(),
            signer.address(),
            CHAIN_ID,
            42
        ),
        Err(SystemTxError::Phase1NonZeroValue)
    ));

    let mut wrong_recipient = base;
    wrong_recipient.to = TxKind::Call(address!("0x5555555555555555555555555555555555555555"));
    let signed_wrong_recipient = signer.sign_unsigned(wrong_recipient).expect("signs");
    assert!(matches!(
        validate_phase1_witness_against(
            &signed_wrong_recipient,
            calldata.as_ref(),
            signer.address(),
            CHAIN_ID,
            42
        ),
        Err(SystemTxError::Phase1WrongRecipient)
    ));
}

#[test]
fn phase1_witness_validation_rejects_wrong_calldata_or_kind() {
    let signer = test_signer(1);
    let calldata = phase1_calldata();
    let signed = signed_phase1(&signer, 42, CHAIN_ID, calldata.clone());

    let mut altered = sample_metadata();
    altered.finalized_block_hash = B256::repeat_byte(0x99);
    let altered_calldata = SystemTxInputV2::CertifiedParentAccounting { metadata: altered }
        .encode()
        .expect("altered phase1 input encodes");
    assert!(matches!(
        validate_phase1_witness_against(
            &signed,
            altered_calldata.as_ref(),
            signer.address(),
            CHAIN_ID,
            42
        ),
        Err(SystemTxError::Phase1CalldataMismatch)
    ));

    let cycle_calldata = input_for(SystemTxKind::CycleTick)
        .encode()
        .expect("cycle input encodes");
    let cycle_unsigned = build_unsigned_system_tx(
        SystemTxKind::CycleTick,
        0,
        42,
        CHAIN_ID,
        cycle_calldata.clone(),
    )
    .expect("cycle tx builds");
    let signed_cycle = signer.sign_unsigned(cycle_unsigned).expect("cycle signs");
    assert!(matches!(
        validate_phase1_witness_against(
            &signed_cycle,
            cycle_calldata.as_ref(),
            signer.address(),
            CHAIN_ID,
            42
        ),
        Err(SystemTxError::CalldataKindMismatch {
            expected: SystemTxKind::CertifiedParentAccounting,
            actual: SystemTxKind::CycleTick,
        })
    ));
}

#[test]
fn canonical_phase1_calldata_changes_signature_hash() {
    let mut left_meta = sample_metadata();
    left_meta.finalized_block_hash = B256::repeat_byte(0x11);
    let left = SystemTxInputV2::CertifiedParentAccounting {
        metadata: left_meta,
    }
    .encode()
    .expect("left input encodes");

    let mut right_meta = sample_metadata();
    right_meta.finalized_block_hash = B256::repeat_byte(0x22);
    let right = SystemTxInputV2::CertifiedParentAccounting {
        metadata: right_meta,
    }
    .encode()
    .expect("right input encodes");

    let left_tx = build_unsigned_system_tx(
        SystemTxKind::CertifiedParentAccounting,
        0,
        42,
        CHAIN_ID,
        left,
    )
    .expect("left tx builds");
    let right_tx = build_unsigned_system_tx(
        SystemTxKind::CertifiedParentAccounting,
        0,
        42,
        CHAIN_ID,
        right,
    )
    .expect("right tx builds");

    assert_ne!(left_tx.signature_hash(), right_tx.signature_hash());
}

#[test]
fn recover_phase1_proposer_rejects_trailing_eip2718_bytes() {
    let signer = test_signer(1);
    let calldata = phase1_calldata();
    let signed = signed_phase1(&signer, 42, CHAIN_ID, calldata);
    let mut encoded = Vec::new();
    signed.encode_2718(&mut encoded);
    encoded.push(0);

    let err = recover_phase1_proposer(&encoded, CHAIN_ID, 42).expect_err("trailing bytes rejected");

    assert!(
        matches!(err, SystemTxError::Phase1TxDecode(message) if message.contains("trailing bytes"))
    );
}
