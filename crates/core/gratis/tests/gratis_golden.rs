//! Frozen postcard, hash, attestation, cipher, and signed-refusal vectors.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_core_common::confidential_tee::{
    attestation, decode_canonical_body, decode_tagged_payload, BalanceTransition,
    WitnessResponseTag, WitnessSchema,
};
use outbe_gratis::cipher::{read_amount, write_amount};
use outbe_gratis::nod_mine::{EncryptedNodMintRejectedV1, MineEncryptedNodResultV1};
use outbe_gratis::{
    decrypt_gratis_balance, decrypt_pledged, derive_gratis_keys_message,
    gratis_op_attestation_preimage, gratis_op_canonical_hash, modify_mac, GratisOp, GratisOpResult,
    ModifyOperation,
};
use outbe_tee::resident::{AccountKeysHandle, StateKeyHandle};
use outbe_tee::tee_call::{verify_signed_rejection, SignedRejectionRef};

fn assert_tagged<T>(value: &T, tag: WitnessResponseTag, populated_hex: &str)
where
    T: WitnessSchema + PartialEq + std::fmt::Debug + Clone,
{
    assert_eq!(T::TAG, tag);
    let mut populated = vec![tag.value()];
    populated.extend(postcard::to_allocvec(value).unwrap());
    assert_eq!(hex::encode(&populated), populated_hex);
    let mut cleared = value.clone();
    cleared.set_attestation_tag(Vec::new());
    let body = postcard::to_allocvec(&cleared).unwrap();
    let golden = hex::decode(populated_hex).unwrap();
    let window = golden[1..]
        .windows(3)
        .position(|bytes| bytes == [2, 0x51, 0x52])
        .expect("populated attestation window");
    let mut tag_free = golden[1..].to_vec();
    tag_free.splice(window..window + 3, [0]);
    assert_eq!(body, tag_free);
    let decoded: T = decode_canonical_body(&golden[1..]).unwrap();
    assert_eq!(&decoded, value);
    let mut wire = vec![tag.value()];
    wire.extend_from_slice(&tag_free);
    let mut from_wire: T = decode_tagged_payload(tag, &wire).unwrap();
    assert!(from_wire.attestation_tag().is_empty());
    from_wire.set_attestation_tag(vec![0x51, 0x52]);
    assert_eq!(&from_wire, value);
}

#[test]
fn response_postcard_vectors_round_trip_tags_1_8_and_9() {
    let applied: GratisOpResult = serde_json::from_str(r#"{"status":"Applied","new_balance":[33],"new_pledged":[34],"event_amount":"0x11","next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    let rejected: GratisOpResult = serde_json::from_str(r#"{"status":{"Rejected":{"reason":"gratis rejected"}},"new_balance":[33],"new_pledged":[34],"event_amount":"0x11","next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    let nod_rejected: EncryptedNodMintRejectedV1 = serde_json::from_str(r#"{"reason":"rejected","inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}"#).unwrap();
    let mined: MineEncryptedNodResultV1 = serde_json::from_str(r#"{"new_balance":[33],"next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    assert_tagged(
        &applied,
        WitnessResponseTag::GratisOpApplied,
        "01000121012220000000000000000000000000000000000000000000000000000000000000001105010121017b072011111111111111111111111111111111111111111111111111111111111111110251520123",
    );
    assert_tagged(
        &rejected,
        WitnessResponseTag::GratisOpApplied,
        "01010f6772617469732072656a65637465640121012220000000000000000000000000000000000000000000000000000000000000001105010121017b072011111111111111111111111111111111111111111111111111111111111111110251520123",
    );
    assert_tagged(
        &nod_rejected,
        WitnessResponseTag::EncryptedNodMintRejectedV1,
        "080872656a6563746564201111111111111111111111111111111111111111111111111111111111111111025152",
    );
    assert_tagged(
        &mined,
        WitnessResponseTag::EncryptedNodMinedV1,
        "090121050121017b072011111111111111111111111111111111111111111111111111111111111111110251520123",
    );
}

#[test]
fn canonical_hash_and_attestation_vectors() {
    let pledge: outbe_gratis::GratisOpRequest = serde_json::from_str(r#"{"op":"Pledge","chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","amount":"0x11","current_balance":[81,82],"current_pledged":[97],"modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5},"fidelity":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#).unwrap();
    assert_eq!(
        hex::encode(gratis_op_canonical_hash(&pledge)),
        "a80bc02929ffdf886b83af91c966299bb115dfc047bb9d0a3eba6e14a5b4e621"
    );
    let applied: GratisOpResult = serde_json::from_str(r#"{"status":"Applied","new_balance":[33],"new_pledged":[34],"event_amount":"0x11","next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    let preimage = gratis_op_attestation_preimage(B256::repeat_byte(0x11), &applied).unwrap();
    assert_eq!(hex::encode(&preimage), "6f757462652f7465652f6772617469732d6174746573746174696f6e2f763311111111111111111111111111111111111111111111111111111111111111110000013c7b22737461747573223a224170706c696564222c226e65775f62616c616e6365223a5b33335d2c226e65775f706c6564676564223a5b33345d2c226576656e745f616d6f756e74223a2230783131222c226e6578745f6f705f6e6f6e6365223a352c22666964656c697479223a7b226e65775f626c6f62223a5b33335d2c227175616c69666965645f73746172745f696e697469616c697a6564223a3132332c226c6561677565223a377d2c22696e707574735f63616e6f6e6963616c5f68617368223a22307831313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131222c226174746573746174696f6e5f746167223a5b5d2c22656e637279707465645f6576656e745f616d6f756e74223a5b33355d7d");
    assert_eq!(
        hex::encode(keccak256(&preimage)),
        "c712ea15201e08c9452b2fca321cbed2315754023a3ce667b46ceb4c6de64c92"
    );
    let mine_request: outbe_gratis::nod_mine::MineEncryptedNodRequestV1 = serde_json::from_str(r#"{"nod":{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65],"encrypted_nominal_amount":[66]},"current_balance":[81,82],"modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5},"fidelity":{"op":"Out","timestamp":123,"first_qualified_start":124,"current_blob":[65,66]}}"#).unwrap();
    assert_eq!(
        hex::encode(outbe_gratis::nod_mine::inputs_hash(&mine_request).unwrap()),
        "e66620bdb18c3bed4b4f00a9d2c64efcb870fa3cff2d6cf6b3043541bf91f855"
    );
    let mine_result: MineEncryptedNodResultV1 = serde_json::from_str(r#"{"new_balance":[33],"next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    let mine_preimage = outbe_gratis::nod_mine::result_preimage(&mine_result).unwrap();
    assert!(mine_preimage.starts_with(b"outbe/nod/mine-result/v1"));
    assert_eq!(
        hex::encode(keccak256(&mine_preimage)),
        "52b8387221eb04c32aa367f8aeee2ac62868682b694b60c267d52cad90067fda"
    );
    assert_eq!(
        hex::encode(derive_gratis_keys_message(
            Address::repeat_byte(0x22),
            B256::repeat_byte(0x33)
        )),
        "6f757462652f6772617469732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333"
    );
}

#[test]
fn gratis_balance_cipher_vector_opens_to_17() {
    let blob = write_amount(
        &StateKeyHandle::for_test([0x11; 32]),
        BalanceTransition {
            account: Address::repeat_byte(0x22),
            field: 0,
            previous_version: 4,
            amount: U256::from(17u64),
            input_hash: B256::repeat_byte(0x33),
        },
    )
    .unwrap();
    let expected = "000000000000000547524132487af11f7a8410e9cc3c0a1e9ed6a40e2e322e9c6f68aff3a10870102fb4973f3ea68cb7a480699247f9259e9915748b1df8c1aeff474f9bf56851dc7534672f365421ed5f4e4de356aa5f9ed900186a";
    assert_eq!(hex::encode(&blob), expected);
    assert_eq!(
        decrypt_gratis_balance(
            &[0x11; 32],
            Address::repeat_byte(0x22),
            &hex::decode(expected).unwrap()
        )
        .unwrap(),
        U256::from(17u64)
    );
}

#[test]
fn signed_nod_refusal_accepts_the_frozen_reason_and_rejects_a_changed_one() {
    let inputs = B256::repeat_byte(0x11);
    let reason = "bad \"quote\"\nline";
    let domain = b"outbe/nod/mint-rejected/v1";
    let preimage = attestation(domain, inputs, reason).unwrap();
    assert_eq!(
        hex::encode(&preimage),
        "6f757462652f6e6f642f6d696e742d72656a65637465642f7631111111111111111111111111111111111111111111111111111111111111111122626164205c2271756f74655c225c6e6c696e6522"
    );
    let tag = hex::decode("3dd25c93a3b7d62ebb5cae88db35be6860281e07e8338dc2e16ca948025e28a36b5aa597a2ac6e555c60864b05a817acb95c638202c257eeb52dbfadc49ee40e").unwrap();
    let key: [u8; 32] =
        hex::decode("f80cccdce4ae1c07ae208a2adf99a310ae4207e0306fa0236110b06827bbb8d0")
            .unwrap()
            .try_into()
            .unwrap();
    let claim = SignedRejectionRef {
        domain,
        expected_inputs_hash: inputs,
        actual_inputs_hash: inputs,
        reason,
        tag: &tag,
    };
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

const GRATIS_OWNER_BLOB: &str = "111213141516171847524132515151515151515151515151515151515151515151515151515151515151515146185e002fe9d80f2f80daea54dcfbd2faa5ce7014252c82b2d6dccd3455cc7a79fdb10771779339ce5f26d6aa2f2027";

fn error_sentence(error: &impl std::fmt::Display) -> String {
    format!("{error}")
}

fn hex_word(text: &str) -> [u8; 32] {
    hex::decode(text).unwrap().try_into().unwrap()
}

#[test]
fn mcp_gratis_modify_macs_match() {
    let rows = [
        (
            "1111111111111111111111111111111111111111",
            GratisOp::Mint,
            1_000u64,
            0u64,
            "0000000000000000000000000000000000000000000000000000000000000001",
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            "ec11651be45952198f63a0c982bbe045c3f3a31131c8afe655f76b243dc50aa1",
        ),
        (
            "1111111111111111111111111111111111111111",
            GratisOp::Pledge,
            10_000_000,
            3,
            "0000000000000000000000000000000000000000000000000000000000000001",
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            "f7d19e989e00dc99df7168cbca111ac11c7328fa0e12c4b0e7873b3ce4e84046",
        ),
        (
            "63467B02a7382408A845a5EB85b5238b8a4dD0eD",
            GratisOp::Burn,
            2_500_000,
            4,
            "00000000000000000000000000000000000000000000000000000000033ce4a9",
            "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
            "3eb5525660b7f79650a2a6ab2b25b67644e72e5d56dd6ef10454badb322f3d6b",
        ),
    ];
    for (account, op, amount, op_nonce, chain_id, modify_key, mac) in rows {
        let account: Address = account.parse().unwrap();
        let amount = U256::from(amount);
        let chain_id = B256::from(hex_word(chain_id));
        let actual = modify_mac(
            &AccountKeysHandle::for_test(account, [0; 32], hex_word(modify_key)),
            &ModifyOperation {
                account,
                op,
                amount,
                op_nonce,
                chain_id,
            },
        );
        assert_eq!(hex::encode(actual), mac);
    }
}

#[test]
fn mcp_gratis_ciphertext_vectors_open() {
    let view = hex_word("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a");
    let view_key = StateKeyHandle::for_test(view);
    let account: Address = "1111111111111111111111111111111111111111".parse().unwrap();
    let rows = [
        (
            0u8,
            2u64,
            "0404040404040404040404040404040404040404040404040404040404040404",
            2_500_000u64,
            "000000000000000347524132bdd4691da65390638406af421d6e89f2ce3a4d71e5b5f2f3fc767ca2ab18e06a690188274592e8e970304a3ee88ff9283e3cef1444b0b2816f4a2629d84fc51bdccef2c8c0b2af2901404c556259922a",
        ),
        (
            1,
            6,
            "0505050505050505050505050505050505050505050505050505050505050505",
            900_000,
            "000000000000000747524132278f700859eac2c84eda4d4b77fe2a09881f54030247d3c8e4b4e4cd7752664ec69d725721dba1c36af0eed950af5a1501cb8cf53aaaea3715f73b03a829b4b921f8aa7ab7c368f9bef2adfb82787207",
        ),
    ];
    for (field, previous_version, input_hash, amount, blob_hex) in rows {
        let amount = U256::from(amount);
        let blob = write_amount(
            &view_key,
            BalanceTransition {
                account,
                field,
                previous_version,
                amount,
                input_hash: B256::from(hex_word(input_hash)),
            },
        )
        .unwrap();
        assert_eq!(hex::encode(&blob), blob_hex);
        assert_eq!(
            read_amount(&view_key, account, field, &blob).unwrap().1,
            amount
        );
        let opened = if field == 0 {
            decrypt_gratis_balance(&view, account, &blob).unwrap()
        } else {
            decrypt_pledged(&view_key, account, &blob).unwrap()
        };
        assert_eq!(opened, amount);
    }
}

#[test]
fn fixed_owner_local_gratis_blob_opens_123456789_and_rejects_context_changes() {
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x42);
    let gratis = hex::decode(GRATIS_OWNER_BLOB).unwrap();
    assert_eq!(
        decrypt_gratis_balance(&key, account, &gratis).unwrap(),
        U256::from(123_456_789u64)
    );
    assert_eq!(
        error_sentence(
            &decrypt_gratis_balance(&key, Address::repeat_byte(0x43), &gratis).unwrap_err()
        ),
        "Gratis balance decryption failed"
    );
    let mut tampered = gratis;
    *tampered.last_mut().unwrap() ^= 1;
    assert_eq!(
        error_sentence(&decrypt_gratis_balance(&key, account, &tampered).unwrap_err()),
        "Gratis balance decryption failed"
    );
}

#[test]
fn gratis_attestation_rejects_a_cross_domain_tag_a_bad_key_and_a_mutated_event() {
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_gratis::{
        verify_gratis_op_attestation, GratisOpResult, GratisOpStatus, GRATIS_OP_ATTESTATION,
    };
    use outbe_tee::{TeeAttestationError, TransportError};

    let signer = SigningKey::from_bytes(&[7u8; 32]);
    let key = signer.verifying_key().to_bytes();
    let hash = B256::repeat_byte(0xab);
    let mut gratis = GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: vec![1, 2, 3],
        new_pledged: Vec::new(),
        event_amount: U256::from(3u64),
        next_op_nonce: 4,
        fidelity: None,
        inputs_canonical_hash: hash,
        attestation_tag: Vec::new(),
        encrypted_event_amount: vec![4, 5, 6, 7],
        pledge_account: None,
    };
    let gratis_preimage = gratis_op_attestation_preimage(hash, &gratis).unwrap();
    let gratis_tag = signer.sign(&gratis_preimage).to_bytes();
    verify_gratis_op_attestation(&key, hash, &gratis, &gratis_tag).unwrap();

    let mut promis_preimage = b"outbe/tee/promis-attestation/v1".to_vec();
    promis_preimage.extend_from_slice(hash.as_slice());
    promis_preimage.extend_from_slice(&4u32.to_be_bytes());
    promis_preimage.extend_from_slice(b"nope");
    let promis_tag = signer.sign(&promis_preimage).to_bytes();
    let cross = verify_gratis_op_attestation(&key, hash, &gratis, &promis_tag).unwrap_err();
    assert_eq!(cross.metric_class(), "gratis_op_attestation");
    assert!(matches!(
        &cross,
        TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::SignatureMismatch(_),
        } if *descriptor == GRATIS_OP_ATTESTATION
    ));

    let mut malformed = [0u8; 32];
    malformed[0] = 2;
    let bad_key = verify_gratis_op_attestation(&malformed, hash, &gratis, &[0u8; 1]).unwrap_err();
    assert!(matches!(
        &bad_key,
        TransportError::AppAttestation {
            descriptor,
            source: TeeAttestationError::InvalidPublicKey(_),
        } if *descriptor == GRATIS_OP_ATTESTATION
    ));

    gratis.encrypted_event_amount[0] ^= 0xff;
    assert!(verify_gratis_op_attestation(&key, hash, &gratis, &gratis_tag).is_err());
}
