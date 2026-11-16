//! Independent big-endian field/length framing and Keccak request commitments.
use alloy_primitives::{hex, Address, B256};
use outbe_enclave::protocol::*;

#[test]
fn canonical_request_hash_vectors() {
    {
        let request: GratisOpRequest = serde_json::from_str(r#"{"op":"Pledge","chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","amount":"0x11","current_balance":[81,82],"current_pledged":[97],"modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5},"fidelity":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#).unwrap();
        assert_eq!(
            hex::encode(gratis_op_canonical_hash(&request)),
            "a80bc02929ffdf886b83af91c966299bb115dfc047bb9d0a3eba6e14a5b4e621"
        );
    }
    {
        let request: PromisOpRequest = serde_json::from_str(r#"{"op":"Burn","chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","amount":"0x12","current_balance":[81,82],"modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5}}"#).unwrap();
        assert_eq!(
            hex::encode(promis_op_canonical_hash(&request)),
            "69bd5fdefafb569bbad4c4fe862e6fc43f17a3779da15c52921853f459851c66"
        );
    }
    {
        let request: MineGratisFromPromisRequestV1 = serde_json::from_str(r#"{"chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","encrypted_promis_amount":[113],"current_promis_balance":[81,82],"promis_modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5},"current_gratis_balance":[97],"gratis_modify_auth":{"mac":[52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52,52],"op_nonce":6},"fidelity":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#).unwrap();
        assert_eq!(
            hex::encode(mine_gratis_from_promis_canonical_hash(&request)),
            "0901f0aa6c47d79e683062736af5d09f408ad6743aae7628e5ec8e1366bc57bc"
        );
    }
    {
        let request: FidelityCohortRequest = serde_json::from_str(r#"{"chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","amount":"0x13","section":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#).unwrap();
        assert_eq!(
            hex::encode(fidelity_cohort_canonical_hash(&request)),
            "5da2eb60b60c61698fdfc5e33bee21efa6dc22fa919f7c6707ce0a94a535eea7"
        );
    }
    {
        let request: FidelitySnapshotRequest = serde_json::from_str(r#"{"timestamp":123,"first_qualified_start":124,"entries":[{"owner":"0x2222222222222222222222222222222222222222","cohort_blob":[65,66]}]}"#).unwrap();
        assert_eq!(
            hex::encode(fidelity_snapshot_canonical_hash(&request)),
            "04619ece1794b86dd62696ba00527fb9ca594b4aea5a06a91b1f9cc8c82bfa59"
        );
    }
    {
        let request: FidelityQueryRequest = serde_json::from_str(r#"{"chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","cohort_blob":[65,66],"query_timestamp":123,"block_timestamp":124,"first_qualified_start":125,"expiry":126,"owner_sig":[81,82]}"#).unwrap();
        assert_eq!(
            hex::encode(fidelity_query_canonical_hash(&request)),
            "4f79b764454883a1a6b57ec3175ec05e14b5e4ff10e7bf61e6a27a34b68c7f69"
        );
    }
    {
        let request: Vec<EncryptedTributeOffer> = serde_json::from_str(r#"[{"owner":"0x2222222222222222222222222222222222222222","cipher_text":[65,66],"nonce":[81],"ephemeral_pubkey":"0x11","worldwide_day":20261116,"tribute_currency":840,"reference_currency":978,"issuance_wwd_vwap_minor":"0x12","reference_wwd_vwap_minor":"0x13","reference_scurve_minor":"0x14","zk_context":{"derived_owner":"0x3333333333333333333333333333333333333333333333333333333333333333","chain_id":1,"l2_chain_id":2}}]"#).unwrap();
        assert_eq!(
            hex::encode(inputs_canonical_hash(&request)),
            "e385617faaf871285c7fca02c61a52373a09264376c36c98d440498a9f06c572"
        );
    }
}

#[test]
fn fidelity_owner_auth_preimage_vector() {
    let actual =
        fidelity_query_auth_message(B256::repeat_byte(0x11), Address::repeat_byte(0x22), 123);
    assert_eq!(hex::encode(&actual), "6f757462652f666964656c6974792f71756572792d617574682f763111111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222222222222000000000000007b");
    assert_eq!(
        hex::encode(eip191_hash(&actual)),
        "a5b532994528a0e506925ae1198367a9ad1d1c4434b8a8a438e127e97fa4d662"
    );
}

#[test]
fn legacy_gratis_owner_message_alias_vector() {
    let actual = derive_gratis_keys_message(Address::repeat_byte(0x22), B256::repeat_byte(0x33));
    assert_eq!(hex::encode(actual), "6f757462652f6772617469732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333");
}
