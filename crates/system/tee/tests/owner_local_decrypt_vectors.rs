//! Fixed external HKDF-SHA256 and ChaCha20-Poly1305 vectors for owner-local views.
//! Ciphertexts were generated independently with Python cryptography 41.0.7.

use alloy_primitives::{hex, Address, B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV2, NodTermsV2},
    time::WorldwideDay,
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::{
    fidelity_decrypt::decrypt_fidelity_cohorts, gratis_decrypt::decrypt_gratis_balance,
    nod_decrypt::decrypt_nod_for_owner, tribute_decrypt::decrypt_tribute_for_creator,
};

const FIDELITY_BLOB: &str = "01020304050607084649443231313131313131313131313131313131313131313131313131313131313131316f227d375d26e7d0c69faf0aa386d6b2f7e739b4de99fc0828f54a8b22d4e223";
const GRATIS_BLOB: &str = "111213141516171847524132515151515151515151515151515151515151515151515151515151515151515146185e002fe9d80f2f80daea54dcfbd2faa5ce7014252c82b2d6dccd3455cc7a79fdb10771779339ce5f26d6aa2f2027";
// Independently generated with Python cryptography 41.0.7 and Foundry cast keccak.
// The public-key blobs are encrypted literal 32-byte X25519 public keys; their
// full versioned bytes are committed into each amount HKDF info string.
const ENCLAVE_PUBLIC: &str = "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20";
const NOD_CONTEXT_DIGEST: &str = "9f5ed554cb633715b10ef9300eee7572dd8d7bd9b7ee96f47b23f87152ae6720";
const NOD_CREATOR_BLOB: &str = "0000000000000001e61bd19649c699bec83af492872209e7c8097b19f8c2e2cecf425c57db99a5958b012576f5015e4e70e289169abb62a9";
const NOD_AMOUNT_BLOB: &str = "000000000000000180659b0633bd77c6b51dea420ff70bb2288cd6a1f282a9cf7fdbedcc651c41f1afc6d3e21537750cde151f4ca9c10c0d";
const NOD_WRONG_NONCE_DOMAIN_BLOB: &str = "0000000000000001a850187c31ee54d13f4586dd8e1917c06c8f657fdbd6d09cb7f7d60f0df86b0209349a0265f21b1b90e38d68733853ca";
const TRIBUTE_CONTEXT_DIGEST: &str =
    "bf2aeb00d99f4fb3a3e3c65f921e35ec92faef7547ce133c3381aa7d8f9d2905";
const TRIBUTE_CREATOR_BLOB: &str = "000000000000000151d8b0ba9ed9c55483aa2ae2997845f050a185b3e57e09bc8c29e163a15af42504e0f20c819ac698c3fd5537f699c07e";
const TRIBUTE_AMOUNT_BLOB: &str = "0000000000000001209db04214512d85c7b00fbca520e98fd1a748e7cb95f91de5c236d9b8856d683ac28c2b6c7b53b8029fae6ceddd5a72dd04ffe1b23e3441fc31accd0924f1de98c5f9cceb78fef3786403afe5bfb3c8";
const TRIBUTE_WRONG_NONCE_DOMAIN_BLOB: &str = "00000000000000013cbe3bd290320d4efb40a2c38c0a3242e1b81a482ae621741331bf87baac24f3ab66707698c13d67e307b9898f8e1bfde8a7ba74caec550da3035df99a9de6bc1cd18dc6cec86b70a9a3f971e49b427a";

fn enclave_public() -> [u8; 32] {
    hex::decode(ENCLAVE_PUBLIC).unwrap().try_into().unwrap()
}

fn fixed_nod() -> EncryptedNodV2 {
    let day = WorldwideDay::new(20_260_824);
    let nod = EncryptedNodV2 {
        terms: NodTermsV2 {
            chain_id: 31_337,
            nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(0xab)),
            owner: Address::repeat_byte(0x42),
            worldwide_day: day,
            league_id: 7,
            entry_price_minor: U256::from(123_456u64),
            issuance_currency: 840,
            reference_currency: 978,
        },
        encryption_binding: B256::repeat_byte(0xa5),
        encrypted_creator_public_key: hex::decode(NOD_CREATOR_BLOB).unwrap(),
        encrypted_gratis_amount: hex::decode(NOD_AMOUNT_BLOB).unwrap(),
    };
    assert_eq!(
        nod.context_digest().as_slice(),
        hex::decode(NOD_CONTEXT_DIGEST).unwrap().as_slice()
    );
    nod
}

fn fixed_tribute() -> EncryptedTributeV2 {
    let day = WorldwideDay::new(20_260_824);
    let tribute = EncryptedTributeV2 {
        context: TributeContextV2 {
            chain_id: 31_337,
            tribute_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(0xcd)),
            owner: Address::repeat_byte(0x53),
            worldwide_day: day,
            issuance_currency: 978,
            reference_currency: 840,
            tribute_price_minor: U256::from(7_654_321u64),
            exclude_from_intex_issuance: true,
            offer_input_hash: B256::repeat_byte(0xb6),
        },
        encrypted_creator_public_key: hex::decode(TRIBUTE_CREATOR_BLOB).unwrap(),
        encrypted_amounts: hex::decode(TRIBUTE_AMOUNT_BLOB).unwrap(),
    };
    assert_eq!(
        tribute.context.digest().as_slice(),
        hex::decode(TRIBUTE_CONTEXT_DIGEST).unwrap().as_slice()
    );
    tribute
}

#[test]
fn fixed_owner_local_view_vectors_open_exact_plaintext() {
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x42);
    let fidelity = hex::decode(FIDELITY_BLOB).unwrap();
    let gratis = hex::decode(GRATIS_BLOB).unwrap();
    assert_eq!(
        decrypt_fidelity_cohorts(&key, account, &fidelity).unwrap(),
        b"cohort-v2-golden".to_vec()
    );
    assert_eq!(
        decrypt_gratis_balance(&key, account, &gratis).unwrap(),
        U256::from(123_456_789_u64)
    );
}

#[test]
fn owner_local_views_reject_wrong_context_and_tampered_tag() {
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x42);
    let mut fidelity = hex::decode(FIDELITY_BLOB).unwrap();
    let mut gratis = hex::decode(GRATIS_BLOB).unwrap();
    assert_eq!(
        decrypt_fidelity_cohorts(&key, Address::repeat_byte(0x43), &fidelity).unwrap_err(),
        "Fidelity cohort decryption failed"
    );
    assert_eq!(
        decrypt_gratis_balance(&key, Address::repeat_byte(0x43), &gratis).unwrap_err(),
        "Gratis balance decryption failed"
    );
    *fidelity.last_mut().unwrap() ^= 1;
    *gratis.last_mut().unwrap() ^= 1;
    assert_eq!(
        decrypt_fidelity_cohorts(&key, account, &fidelity).unwrap_err(),
        "Fidelity cohort decryption failed"
    );
    assert_eq!(
        decrypt_gratis_balance(&key, account, &gratis).unwrap_err(),
        "Gratis balance decryption failed"
    );
}

#[test]
fn fixed_nod_and_tribute_ciphertexts_open_exact_amounts() {
    let owner_secret = [0x11; 32];
    let enclave_public = enclave_public();
    assert_eq!(
        decrypt_nod_for_owner(&owner_secret, &enclave_public, &fixed_nod()).unwrap(),
        U256::from(987_654_321u64)
    );
    assert_eq!(
        decrypt_tribute_for_creator(&owner_secret, &enclave_public, &fixed_tribute()).unwrap(),
        TributeAmountsV2 {
            issuance_amount_minor: U256::from(123_456_789u64),
            nominal_amount_minor: U256::from(9_876_543_210u64),
        }
    );
}

#[test]
fn nod_rejects_wrong_owner_context_and_nonce_domain() {
    let secret = [0x11; 32];
    let enclave_public = enclave_public();
    let nod = fixed_nod();
    assert_eq!(
        decrypt_nod_for_owner(&[0x12; 32], &enclave_public, &nod).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_context = nod.clone();
    wrong_context.terms.owner = Address::repeat_byte(0x43);
    assert_eq!(
        decrypt_nod_for_owner(&secret, &enclave_public, &wrong_context).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_binding = nod.clone();
    wrong_binding.encryption_binding = B256::repeat_byte(0xa6);
    assert_eq!(
        decrypt_nod_for_owner(&secret, &enclave_public, &wrong_binding).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_domain = nod;
    wrong_domain.encrypted_gratis_amount = hex::decode(NOD_WRONG_NONCE_DOMAIN_BLOB).unwrap();
    assert_eq!(
        decrypt_nod_for_owner(&secret, &enclave_public, &wrong_domain).unwrap_err(),
        "NOD amount decryption failed"
    );
}

#[test]
fn tribute_rejects_wrong_owner_context_and_nonce_domain() {
    let secret = [0x11; 32];
    let enclave_public = enclave_public();
    let tribute = fixed_tribute();
    assert_eq!(
        decrypt_tribute_for_creator(&[0x12; 32], &enclave_public, &tribute).unwrap_err(),
        "Tribute amount decryption failed"
    );
    let mut wrong_context = tribute.clone();
    wrong_context.context.owner = Address::repeat_byte(0x54);
    assert_eq!(
        decrypt_tribute_for_creator(&secret, &enclave_public, &wrong_context).unwrap_err(),
        "Tribute amount decryption failed"
    );
    let mut wrong_offer = tribute.clone();
    wrong_offer.context.offer_input_hash = B256::repeat_byte(0xb7);
    assert_eq!(
        decrypt_tribute_for_creator(&secret, &enclave_public, &wrong_offer).unwrap_err(),
        "Tribute amount decryption failed"
    );
    let mut wrong_domain = tribute;
    wrong_domain.encrypted_amounts = hex::decode(TRIBUTE_WRONG_NONCE_DOMAIN_BLOB).unwrap();
    assert_eq!(
        decrypt_tribute_for_creator(&secret, &enclave_public, &wrong_domain).unwrap_err(),
        "Tribute amount decryption failed"
    );
}

struct MalformedCiphertextCase<T> {
    amount_blob: fn(&mut T) -> &mut Vec<u8>,
    creator_blob: fn(&mut T) -> &mut Vec<u8>,
    invalid_encoding: &'static str,
    decryption_failed: &'static str,
}

impl MalformedCiphertextCase<EncryptedNodV2> {
    fn nod() -> Self {
        Self {
            amount_blob: |value| &mut value.encrypted_gratis_amount,
            creator_blob: |value| &mut value.encrypted_creator_public_key,
            invalid_encoding: "invalid encrypted NOD encoding",
            decryption_failed: "NOD amount decryption failed",
        }
    }
}

impl MalformedCiphertextCase<EncryptedTributeV2> {
    fn tribute() -> Self {
        Self {
            amount_blob: |value| &mut value.encrypted_amounts,
            creator_blob: |value| &mut value.encrypted_creator_public_key,
            invalid_encoding: "invalid encrypted Tribute encoding",
            decryption_failed: "Tribute amount decryption failed",
        }
    }
}

fn assert_malformed_ciphertext_cases<T: Clone, R: std::fmt::Debug>(
    original: &T,
    decrypt: impl Fn(&[u8; 32], &T) -> Result<R, String>,
    enclave_public: &[u8; 32],
    case: MalformedCiphertextCase<T>,
) {
    let mut short = original.clone();
    (case.amount_blob)(&mut short).pop();
    assert_eq!(
        decrypt(enclave_public, &short).unwrap_err(),
        case.invalid_encoding
    );

    let mut bad_version = original.clone();
    (case.amount_blob)(&mut bad_version)[7] = 2;
    assert_eq!(
        decrypt(enclave_public, &bad_version).unwrap_err(),
        case.invalid_encoding
    );

    let mut bad_creator_version = original.clone();
    (case.creator_blob)(&mut bad_creator_version)[7] = 2;
    assert_eq!(
        decrypt(enclave_public, &bad_creator_version).unwrap_err(),
        case.invalid_encoding
    );

    let mut bad_creator_ciphertext = original.clone();
    (case.creator_blob)(&mut bad_creator_ciphertext)[8] ^= 1;
    assert_eq!(
        decrypt(enclave_public, &bad_creator_ciphertext).unwrap_err(),
        case.decryption_failed
    );

    let mut bad_ciphertext = original.clone();
    (case.amount_blob)(&mut bad_ciphertext)[8] ^= 1;
    assert_eq!(
        decrypt(enclave_public, &bad_ciphertext).unwrap_err(),
        case.decryption_failed
    );

    let mut bad_tag = original.clone();
    let tag_blob = (case.amount_blob)(&mut bad_tag);
    assert!(!tag_blob.is_empty(), "golden ciphertext must have a tag");
    let tag_index = tag_blob.len() - 1;
    tag_blob[tag_index] ^= 1;
    assert_eq!(
        decrypt(enclave_public, &bad_tag).unwrap_err(),
        case.decryption_failed
    );

    assert_eq!(
        decrypt(&[0; 32], original).unwrap_err(),
        "invalid network X25519 public key"
    );
}

#[test]
fn nod_rejects_malformed_length_version_tag_and_network_key() {
    let secret = [0x11; 32];
    let enclave_public = enclave_public();
    let nod = fixed_nod();
    assert_malformed_ciphertext_cases(
        &nod,
        |public, value| decrypt_nod_for_owner(&secret, public, value),
        &enclave_public,
        MalformedCiphertextCase::nod(),
    );
}

#[test]
fn tribute_rejects_malformed_length_version_tag_and_network_key() {
    let secret = [0x11; 32];
    let enclave_public = enclave_public();
    let tribute = fixed_tribute();
    assert_malformed_ciphertext_cases(
        &tribute,
        |public, value| decrypt_tribute_for_creator(&secret, public, value),
        &enclave_public,
        MalformedCiphertextCase::tribute(),
    );
}
