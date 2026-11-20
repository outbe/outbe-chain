//! Frozen postcard, hash, attestation, cipher, and owner-decrypt vectors.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decode_canonical_body, decode_tagged_payload, derive_account_keys_message, BalanceTransition,
    Ledger, WitnessResponseTag, WitnessSchema,
};
use outbe_promis::cipher::{read_amount, write_amount};
use outbe_promis::{
    decrypt_promis_balance, derive_promis_keys_message, modify_mac, promis_op_attestation_preimage,
    promis_op_canonical_hash, ModifyOperation, PromisOp, PromisOpRequest, PromisOpResult,
};
use outbe_tee::resident::{AccountKeysHandle, StateKeyHandle};

const PROMIS_PREIMAGE: &str = "6f757462652f7465652f70726f6d69732d6174746573746174696f6e2f76311111111111111111111111111111111111111111111111111111111111111111000000df7b22737461747573223a224170706c696564222c226e65775f62616c616e6365223a5b33335d2c226576656e745f616d6f756e74223a2230783131222c226e6578745f6f705f6e6f6e6365223a352c22696e707574735f63616e6f6e6963616c5f68617368223a22307831313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131313131222c226174746573746174696f6e5f746167223a5b5d2c22656e637279707465645f6576656e745f616d6f756e74223a5b33355d7d";
const PROMIS_OWNER_BLOB: &str = "111213141516171850524f325151515151515151515151515151515151515151515151515151515151515151950f76eb13183701977ca3510b80c426f3dacad91248fa9961b5857074a00025091a5da1a6224be9617c475e65c22d71";
const FIELD0_BLOB: &str = "000000000000000550524f328ab66079822a11c7a53c086d827f104690251044a05eb599c2abd3c3d3ad40d11677d683be6a5cfec2681a4184c710ded588be72b9e95f7fae6935d40221b7fcbaac0060ab0604ebf88c867bcb9ba40d";
const FIELD1_BLOB: &str = "000000000000000550524f3255681f26ff5de06b7ea8b7b9c7cee1926b0f90a43db3a83c0477a7b29d9de774d8ac47647684b95ec7e4e21342a302401cd6e8a630bf833e87e4459c47c38fce52e53659e63d920333dad8b8636d74ca";

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
fn response_postcard_vectors_round_trip_tag_2() {
    let applied: PromisOpResult = serde_json::from_str(r#"{"status":"Applied","new_balance":[33],"event_amount":"0x11","next_op_nonce":5,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    let rejected: PromisOpResult = serde_json::from_str(r#"{"status":{"Rejected":{"reason":"promis rejected"}},"new_balance":[33],"event_amount":"0x11","next_op_nonce":5,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    assert_tagged(
        &applied,
        WitnessResponseTag::PromisOpApplied,
        "02000121200000000000000000000000000000000000000000000000000000000000000011052011111111111111111111111111111111111111111111111111111111111111110251520123",
    );
    assert_tagged(
        &rejected,
        WitnessResponseTag::PromisOpApplied,
        "02010f70726f6d69732072656a65637465640121200000000000000000000000000000000000000000000000000000000000000011052011111111111111111111111111111111111111111111111111111111111111110251520123",
    );
}

#[test]
fn canonical_hash_and_attestation_vectors() {
    let burn: PromisOpRequest = serde_json::from_str(r#"{"op":"Burn","chain_id":"0x1111111111111111111111111111111111111111111111111111111111111111","account":"0x2222222222222222222222222222222222222222","amount":"0x12","current_balance":[81,82],"modify_auth":{"mac":[51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51,51],"op_nonce":5}}"#).unwrap();
    assert_eq!(
        hex::encode(promis_op_canonical_hash(&burn)),
        "69bd5fdefafb569bbad4c4fe862e6fc43f17a3779da15c52921853f459851c66"
    );
    let applied: PromisOpResult = serde_json::from_str(r#"{"status":"Applied","new_balance":[33],"event_amount":"0x11","next_op_nonce":5,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}"#).unwrap();
    let preimage = promis_op_attestation_preimage(B256::repeat_byte(0x11), &applied).unwrap();
    assert_eq!(hex::encode(&preimage), PROMIS_PREIMAGE);
    assert_eq!(
        hex::encode(keccak256(&preimage)),
        "7e0bc5c10c3e70bd9a4d4b9b31c53ee8855d3fa39a91575c1ab5e82e6906bf17"
    );
    let account = Address::repeat_byte(0x22);
    let ephemeral = B256::repeat_byte(0x33);
    assert_eq!(
        derive_promis_keys_message(account, ephemeral),
        derive_account_keys_message(Ledger::Promis, account, ephemeral)
    );
    assert_eq!(
        hex::encode(derive_promis_keys_message(account, ephemeral)),
        "6f757462652f70726f6d69732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333"
    );
}

fn hex_word(text: &str) -> [u8; 32] {
    hex::decode(text).unwrap().try_into().unwrap()
}

#[test]
fn mcp_promis_modify_macs_and_balance_fields_match() {
    let rows = [
        (
            "1111111111111111111111111111111111111111",
            PromisOp::Mint,
            U256::from(1_000u64),
            0u64,
            "0000000000000000000000000000000000000000000000000000000000000001",
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            "3d33ac4e239f595c5d87e9db9bd4e180ff4f080811f22150cfc0b7b7eba2467b",
        ),
        (
            "abcdef0123456789abcdef0123456789abcdef01",
            PromisOp::Burn,
            U256::from(123_456_789_012_345_678_901_234_567_890u128),
            7,
            "c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2",
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            "94bbfa7708394cf45466594357d2dc942f65e92ef0aa2509e03dfec5a5861f84",
        ),
        (
            "63467B02a7382408A845a5EB85b5238b8a4dD0eD",
            PromisOp::Mint,
            U256::from(7_000_000u64),
            9,
            "00000000000000000000000000000000000000000000000000000000033ce4a9",
            "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b",
            "7b3938033a1bb7b1ef93540f3fb86d01122bc8772b2ef4a8d1f6d3377ee1e02d",
        ),
    ];
    for (account, op, amount, op_nonce, chain_id, modify_key, mac) in rows {
        let account: Address = account.parse().unwrap();
        let actual = modify_mac(
            &AccountKeysHandle::for_test(account, [0; 32], hex_word(modify_key)),
            &ModifyOperation {
                account,
                op,
                amount,
                op_nonce,
                chain_id: B256::from(hex_word(chain_id)),
            },
        );
        assert_eq!(hex::encode(actual), mac);
    }

    let view = hex_word("5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a");
    let view_key = StateKeyHandle::for_test(view);
    let account: Address = "1111111111111111111111111111111111111111".parse().unwrap();
    let amount = U256::from(123_456_789u64);
    let input_hash = B256::from(hex_word(
        "0606060606060606060606060606060606060606060606060606060606060606",
    ));
    for (field, blob_hex) in [(0u8, FIELD0_BLOB), (1u8, FIELD1_BLOB)] {
        let blob = write_amount(
            &view_key,
            BalanceTransition {
                account,
                field,
                previous_version: 4,
                amount,
                input_hash,
            },
        )
        .unwrap();
        assert_eq!(hex::encode(&blob), blob_hex, "field {field}");
        assert_eq!(
            read_amount(&view_key, account, field, &blob).unwrap().1,
            amount
        );
        if field == 0 {
            assert_eq!(
                decrypt_promis_balance(&view, account, &blob).unwrap(),
                amount
            );
        }
    }
}

#[test]
fn fixed_owner_local_promis_blob_opens_123456789_and_rejects_context_changes() {
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x42);
    let promis = hex::decode(PROMIS_OWNER_BLOB).unwrap();
    assert_eq!(
        decrypt_promis_balance(&key, account, &promis).unwrap(),
        U256::from(123_456_789u64)
    );
    assert_eq!(
        decrypt_promis_balance(&key, Address::repeat_byte(0x43), &promis).unwrap_err(),
        "Promis balance decryption failed"
    );
    let mut flipped = promis;
    *flipped.last_mut().unwrap() ^= 1;
    assert_eq!(
        decrypt_promis_balance(&key, account, &flipped).unwrap_err(),
        "Promis balance decryption failed"
    );
}

#[test]
fn pro2_cipher_kat_writes_a0da28be_and_opens_seventeen() {
    const FROZEN: &str = "000000000000000550524f32a0da28be8b5eae47acd5ff7d780f9af69f684ae69bf79607a10669cae33289fe0f9aa44218f93e4fff57a2be4de213b4f586d48ef0fbb007619920a7c46774c710a9cfe3f7d31af4452aefcac0d4904c";
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x22);
    let written = write_amount(
        &StateKeyHandle::for_test(key),
        BalanceTransition {
            account,
            field: 0,
            previous_version: 4,
            amount: U256::from(17),
            input_hash: B256::repeat_byte(0x33),
        },
    )
    .unwrap();
    assert_eq!(hex::encode(&written), FROZEN);
    let historical = hex::decode(FROZEN).unwrap();
    assert_eq!(
        decrypt_promis_balance(&key, account, &historical).unwrap(),
        U256::from(17)
    );
}
