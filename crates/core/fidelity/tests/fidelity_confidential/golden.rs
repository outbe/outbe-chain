//! Fidelity rows from the historical domain preimage vectors.
use alloy_primitives::{hex, keccak256, Address, B256};
use outbe_core_common::confidential_tee::{
    attestation, decode_canonical_body, decode_tagged_payload, WitnessResponseTag, WitnessSchema,
};
use outbe_fidelity::{
    decrypt_fidelity_cohorts, fidelity_cohort_attestation_preimage, fidelity_cohort_canonical_hash,
    fidelity_query_attestation_preimage, fidelity_query_auth_message,
    fidelity_query_canonical_hash, fidelity_snapshot_attestation_preimage,
    fidelity_snapshot_canonical_hash, write_blob, CohortTransition, FidelityCohortRequest,
    FidelityCohortResult, FidelityLeagueEntry, FidelityLeagueSnapshot, FidelityQueryRejection,
    FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotRequest,
};
use outbe_tee::resident::StateKeyHandle;
use outbe_tee::tee_call::{verify_signed_rejection, SignedRejectionRef};

#[test]
fn canonical_request_hash_vectors() {
    let cohort: FidelityCohortRequest = serde_json::from_str(r#"{"chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","amount":"0x13","section":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#).unwrap();
    assert_eq!(
        hex::encode(fidelity_cohort_canonical_hash(&cohort)),
        "5da2eb60b60c61698fdfc5e33bee21efa6dc22fa919f7c6707ce0a94a535eea7"
    );
    let snapshot: FidelitySnapshotRequest = serde_json::from_str(r#"{"timestamp":123,"first_qualified_start":124,"entries":[{"owner":"0x2222222222222222222222222222222222222222","cohort_blob":[65,66]}]}"#).unwrap();
    assert_eq!(
        hex::encode(fidelity_snapshot_canonical_hash(&snapshot)),
        "04619ece1794b86dd62696ba00527fb9ca594b4aea5a06a91b1f9cc8c82bfa59"
    );
    let query: FidelityQueryRequest = serde_json::from_str(r#"{"chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","cohort_blob":[65,66],"query_timestamp":123,"block_timestamp":124,"first_qualified_start":125,"expiry":126,"owner_sig":[81,82]}"#).unwrap();
    assert_eq!(
        hex::encode(fidelity_query_canonical_hash(&query)),
        "4f79b764454883a1a6b57ec3175ec05e14b5e4ff10e7bf61e6a27a34b68c7f69"
    );
}

#[test]
fn fidelity_owner_auth_preimage_vector() {
    let actual =
        fidelity_query_auth_message(B256::repeat_byte(0x11), Address::repeat_byte(0x22), 123);
    assert_eq!(hex::encode(&actual), "6f757462652f666964656c6974792f71756572792d617574682f763111111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222222222222000000000000007b");
    assert_eq!(
        hex::encode(outbe_fidelity::eip191_hash(&actual)),
        "a5b532994528a0e506925ae1198367a9ad1d1c4434b8a8a438e127e97fa4d662"
    );
}

#[test]
fn signed_attestation_preimage_vectors() {
    let cohort: FidelityCohortResult = serde_json::from_str(r#"{"outcome":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}"#).unwrap();
    let actual = fidelity_cohort_attestation_preimage(B256::repeat_byte(0x11), &cohort).unwrap();
    assert_eq!(hex::encode(&actual), "6f757462652f7465652f666964656c6974792d636f686f72742d6174746573746174696f6e2f76311111111111111111111111111111111111111111111111111111111111111111000000bc7b226f7574636f6d65223a7b226e65775f626c6f62223a5b33335d2c227175616c69666965645f73746172745f696e697469616c697a6564223a3132332c226c6561677565223a377d2c22696e707574735f63616e6f6e6963616c5f68617368223a22307831313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131222c226174746573746174696f6e5f746167223a5b5d7d");
    assert_eq!(
        hex::encode(keccak256(&actual)),
        "6ef0099eed8692c7d44bb73ea5764b46de28f087665551b9ef439b30515b8d91"
    );

    let leagues: Vec<FidelityLeagueEntry> = serde_json::from_str(
        r#"[{"owner":"0x2222222222222222222222222222222222222222","league":7,"rcfi":"0x11"}]"#,
    )
    .unwrap();
    let actual = fidelity_snapshot_attestation_preimage(B256::repeat_byte(0x11), &leagues).unwrap();
    assert_eq!(hex::encode(&actual), "6f757462652f7465652f666964656c6974792d736e617073686f742d6174746573746174696f6e2f76311111111111111111111111111111111111111111111111111111111111111111000000515b7b226f776e6572223a22307832323232323232323232323232323232323232323232323232323232323232323232323232323232222c226c6561677565223a372c2272636669223a2230783131227d5d");
    assert_eq!(
        hex::encode(keccak256(&actual)),
        "6b0597000ab7b67d60bd9f92af329cdc74b11beede63c58070426ef21a676049"
    );

    let query: FidelityQueryResult = serde_json::from_str(r#"{"rcfi":"0x11","efficiency":"0x12","league":7,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}"#).unwrap();
    let actual = fidelity_query_attestation_preimage(B256::repeat_byte(0x11), &query).unwrap();
    assert_eq!(hex::encode(&actual), "6f757462652f7465652f666964656c6974792d71756572792d6174746573746174696f6e2f76311111111111111111111111111111111111111111111111111111111111111111000000a07b2272636669223a2230783131222c22656666696369656e6379223a2230783132222c226c6561677565223a372c22696e707574735f63616e6f6e6963616c5f68617368223a22307831313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131222c226174746573746174696f6e5f746167223a5b5d7d");
    assert_eq!(
        hex::encode(keccak256(&actual)),
        "93fbc343fa62bf4967009e856b9ee3c17e5133575c7ff45f324452c81e958071"
    );
}

#[test]
fn cohort_cipher_vector_opens_to_padded_state() {
    let expected = "000000000000000546494432f9dbf39c09d417d4df25eaf1cfb658174cc0507208c43fccfa86fb53e38b47c38c567f2a464d290791d9cc03d17c7a131e618aa8";
    let actual = write_blob(
        &StateKeyHandle::for_test([0x11; 32]),
        CohortTransition {
            account: Address::repeat_byte(0x22),
            previous_version: 4,
            previous_blob: &[0x41, 0x42],
            padded_state: &[0x51, 0x52, 0x53, 0x54],
        },
    )
    .unwrap();
    assert_eq!(hex::encode(actual), expected);
    assert_eq!(
        decrypt_fidelity_cohorts(
            &[0x11; 32],
            Address::repeat_byte(0x22),
            &hex::decode(expected).unwrap()
        )
        .unwrap(),
        vec![0x51, 0x52, 0x53, 0x54]
    );
}

#[test]
fn response_postcard_vectors_are_tag_free_on_the_tape() {
    let snapshot: FidelityLeagueSnapshot = serde_json::from_str(r#"{"leagues":[{"owner":"0x2222222222222222222222222222222222222222","league":7,"rcfi":"0x11"}],"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}"#).unwrap();
    let query: FidelityQueryResult = serde_json::from_str(r#"{"rcfi":"0x11","efficiency":"0x12","league":7,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}"#).unwrap();
    let rejected: FidelityQueryRejection = serde_json::from_str(r#"{"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","reason":"rejected","attestation_tag":[81,82]}"#).unwrap();
    assert_tagged(
        &snapshot,
        WitnessResponseTag::FidelityLeaguesSnapshotted,
        "040114222222222222222222222222222222222222222207200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152",
    );
    assert_tagged(
        &query,
        WitnessResponseTag::FidelityIndexQueried,
        "0520000000000000000000000000000000000000000000000000000000000000001120000000000000000000000000000000000000000000000000000000000000001207201111111111111111111111111111111111111111111111111111111111111111025152",
    );
    assert_tagged(
        &rejected,
        WitnessResponseTag::FidelityQueryRejected,
        "0f2011111111111111111111111111111111111111111111111111111111111111110872656a6563746564025152",
    );
}

fn assert_tagged<T>(value: &T, tag: WitnessResponseTag, populated_hex: &str)
where
    T: WitnessSchema + PartialEq + std::fmt::Debug,
{
    assert_eq!(T::TAG, tag);
    let mut populated = vec![tag.value()];
    populated.extend(postcard::to_allocvec(value).unwrap());
    assert_eq!(hex::encode(&populated), populated_hex);
    let mut cleared = value.clone();
    cleared.set_attestation_tag(Vec::new());
    let body = postcard::to_allocvec(&cleared).unwrap();
    assert_eq!(*body.last().unwrap(), 0);
    let golden = hex::decode(populated_hex).unwrap();
    assert_eq!(&golden[golden.len() - 3..], &[2, 0x51, 0x52]);
    let mut tag_free = golden[1..golden.len() - 3].to_vec();
    tag_free.push(0);
    assert_eq!(body, tag_free);
    let decoded: T = decode_canonical_body(&golden[1..]).unwrap();
    assert_eq!(decoded, *value);
    let mut wire = vec![tag.value()];
    wire.extend_from_slice(&tag_free);
    let mut from_wire: T = decode_tagged_payload(tag, &wire).unwrap();
    assert!(from_wire.attestation_tag().is_empty());
    from_wire.set_attestation_tag(vec![0x51, 0x52]);
    assert_eq!(from_wire, *value);
}

#[test]
fn cohort_result_postcard_omits_the_old_enum_discriminant() {
    let expected: FidelityCohortResult = serde_json::from_str(r#"{"outcome":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}"#).unwrap();
    let body = "0121017b07201111111111111111111111111111111111111111111111111111111111111111025152";
    assert_eq!(hex::encode(postcard::to_allocvec(&expected).unwrap()), body);
    let decoded: FidelityCohortResult = decode_canonical_body(&hex::decode(body).unwrap()).unwrap();
    assert_eq!(decoded, expected);
}

#[test]
fn query_rejected_signed_client_vector() {
    let inputs = B256::repeat_byte(0x11);
    let reason = "bad \"quote\"\nline";
    let domain = b"outbe/fidelity/query-rejected/v1";
    let preimage = attestation(domain, inputs, reason).unwrap();
    assert_eq!(
        hex::encode(preimage),
        "6f757462652f666964656c6974792f71756572792d72656a65637465642f7631111111111111111111111111111111111111111111111111111111111111111122626164205c2271756f74655c225c6e6c696e6522"
    );
    let tag = hex::decode("f2f6cbd5817c2c233ec02c10ca7b15ca4bc03f96384825365661757b72a5dbf642988500a301612615975da9969407fda8e20ac0622433418203e889628b8e0d").unwrap();
    let claim = SignedRejectionRef {
        domain,
        expected_inputs_hash: inputs,
        actual_inputs_hash: inputs,
        reason,
        tag: &tag,
    };
    let key =
        hex::decode("f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0").unwrap();
    let key: [u8; 32] = key.try_into().unwrap();
    verify_signed_rejection(&key, claim).unwrap();
    assert!(verify_signed_rejection(
        &key,
        SignedRejectionRef {
            reason: "changed",
            ..claim
        },
    )
    .is_err());
}
