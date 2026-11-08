use super::*;

#[test]
fn tempo_zero_gas_model_returns_zero_for_all_valid_system_txs() {
    for input in inputs() {
        let encoded = input.encode().expect("system tx encodes");
        assert_eq!(system_tx_intrinsic_gas(&encoded), Ok(0));
        assert_eq!(system_tx_visible_gas_limit(&encoded), Ok(0));
    }
}

#[test]
fn gas_helpers_reject_malformed_system_tx_calldata() {
    assert_eq!(
        system_tx_intrinsic_gas(b"OSC2\x02\x00"),
        Err(SystemTxError::UnexpectedBody {
            kind: "CycleTick",
            len: 1,
        })
    );
    assert_eq!(
        system_tx_visible_gas_limit(b"NOPE\x02"),
        Err(SystemTxError::UnknownSelector(*b"NOPE"))
    );
}

#[test]
fn zero_gas_envelopes_preserve_prebuilt_phase1_witness_bytes() {
    let signer = OutbeEvmSigner::from_secret_bytes([7; 32]).unwrap();
    let calldata = accounting().encode().unwrap();
    for height in [2, 42] {
        let prebuilt = signer
            .sign_unsigned(build_unsigned_system_tx(0, height, CHAIN_ID, calldata.clone()).unwrap())
            .unwrap();
        let explicit_zero = signer
            .sign_unsigned(
                build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
                    ordinal: 0,
                    block_number: height,
                    chain_id: CHAIN_ID,
                    calldata: calldata.clone(),
                    gas_limit: 0,
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(prebuilt.encoded_2718(), explicit_zero.encoded_2718());
        assert_eq!(
            validate_phase1_witness_against(
                &explicit_zero,
                &calldata,
                signer.address(),
                CHAIN_ID,
                height,
            ),
            Ok(prebuilt.signature_hash())
        );
    }
}
