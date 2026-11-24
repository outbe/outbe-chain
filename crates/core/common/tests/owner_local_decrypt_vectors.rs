//! Fixed external HKDF-SHA256 and ChaCha20-Poly1305 vectors for owner-local views.
//! Ciphertexts were generated independently with Python cryptography 41.0.7.

use alloy_primitives::{hex, Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decrypt_fidelity_cohorts, decrypt_gratis_balance, decrypt_nod_for_owner,
    decrypt_nod_nominal_for_owner, decrypt_promis_balance, LocalCryptoError,
};
use outbe_core_common::nod_encryption::{EncryptedNodV1, NodTermsV1};
use outbe_system_common::time::WorldwideDay;

const FIDELITY_BLOB: &str = "01020304050607084649443231313131313131313131313131313131313131313131313131313131313131316f227d375d26e7d0c69faf0aa386d6b2f7e739b4de99fc0828f54a8b22d4e223";
const GRATIS_BLOB: &str = "111213141516171847524132515151515151515151515151515151515151515151515151515151515151515146185e002fe9d80f2f80daea54dcfbd2faa5ce7014252c82b2d6dccd3455cc7a79fdb10771779339ce5f26d6aa2f2027";
const PROMIS_BLOB: &str = "111213141516171850524f325151515151515151515151515151515151515151515151515151515151515151950f76eb13183701977ca3510b80c426f3dacad91248fa9961b5857074a00025091a5da1a6224be9617c475e65c22d71";
const NOD_CONTEXT_DIGEST: &str = "b09e597ba7d0c3f500971679c335da4fec642cad4f3a5f4e4017762e362b8ad3";
const NOD_AMOUNT_BLOB: &str = "0000000000000001e49a4a79c30f2f7fdb34b16e286f66b81d1258a7c3dead888ea07880c8080d7f6bed38808fb49611b17a1224b7748b0d";
const NOD_NOMINAL_BLOB: &str = "000000000000000159f9bcc390542bace5ab522b5a6fbbcbe1a50f0870618bc327a3c53c8deda06d547dd59c1f3960ce825fca6f04188687";
const NOD_WRONG_NONCE_DOMAIN_BLOB: &str = "0000000000000001fba976114a9798d9106c080ee3ba9a1b2c146b31d46a2fcbc35758963fe72c0aed54c67d9ec4366fca285d20d393228e";

fn fixed_nod() -> EncryptedNodV1 {
    let day = WorldwideDay::new(20_260_824);
    let nod = EncryptedNodV1 {
        terms: NodTermsV1 {
            chain_id: 31_337,
            nod_id: U256::from_str_radix(
                "013527d8abababababababababababababababababababababababababababab",
                16,
            )
            .unwrap(),
            owner: Address::repeat_byte(0x42),
            emission_day: day.value(),
            entry_price_minor: U256::from(123_456u64),
            issuance_currency: 840,
            reference_currency: 978,
        },
        encryption_binding: B256::repeat_byte(0xa5),
        encrypted_gratis_amount: hex::decode(NOD_AMOUNT_BLOB).unwrap(),
        encrypted_nominal_amount: hex::decode(NOD_NOMINAL_BLOB).unwrap(),
    };
    assert_eq!(
        nod.context_digest().as_slice(),
        hex::decode(NOD_CONTEXT_DIGEST).unwrap().as_slice()
    );
    nod
}

#[test]
fn fixed_owner_local_view_vectors_open_exact_plaintext() {
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x42);
    let fidelity = hex::decode(FIDELITY_BLOB).unwrap();
    let gratis = hex::decode(GRATIS_BLOB).unwrap();
    let promis = hex::decode(PROMIS_BLOB).unwrap();
    assert_eq!(
        decrypt_fidelity_cohorts(&key, account, &fidelity).unwrap(),
        b"cohort-v2-golden".to_vec()
    );
    assert_eq!(
        decrypt_gratis_balance(&key, account, &gratis).unwrap(),
        U256::from(123_456_789_u64)
    );
    assert_eq!(
        decrypt_promis_balance(&key, account, &promis).unwrap(),
        U256::from(123_456_789_u64)
    );
}

#[test]
fn owner_local_views_reject_wrong_context_and_tampered_tag() {
    let key = [0x11; 32];
    let account = Address::repeat_byte(0x42);
    let mut fidelity = hex::decode(FIDELITY_BLOB).unwrap();
    let mut gratis = hex::decode(GRATIS_BLOB).unwrap();
    let mut promis = hex::decode(PROMIS_BLOB).unwrap();
    assert_eq!(
        decrypt_fidelity_cohorts(&key, Address::repeat_byte(0x43), &fidelity).unwrap_err(),
        "Fidelity cohort decryption failed"
    );
    assert_eq!(
        decrypt_gratis_balance(&key, Address::repeat_byte(0x43), &gratis).unwrap_err(),
        "Gratis balance decryption failed"
    );
    assert_eq!(
        decrypt_promis_balance(&key, Address::repeat_byte(0x43), &promis).unwrap_err(),
        "Promis balance decryption failed"
    );
    *fidelity.last_mut().unwrap() ^= 1;
    *gratis.last_mut().unwrap() ^= 1;
    *promis.last_mut().unwrap() ^= 1;
    assert_eq!(
        decrypt_fidelity_cohorts(&key, account, &fidelity).unwrap_err(),
        "Fidelity cohort decryption failed"
    );
    assert_eq!(
        decrypt_gratis_balance(&key, account, &gratis).unwrap_err(),
        "Gratis balance decryption failed"
    );
    assert_eq!(
        decrypt_promis_balance(&key, account, &promis).unwrap_err(),
        "Promis balance decryption failed"
    );
}

#[test]
fn fixed_nod_ciphertexts_open_exact_amounts() {
    let view_key = [0x11; 32];
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &fixed_nod()).unwrap(),
        U256::from(987_654_321u64)
    );
    assert_eq!(
        decrypt_nod_nominal_for_owner(&view_key, &fixed_nod()).unwrap(),
        U256::from(3_086_419_753u64)
    );
}

#[test]
fn nod_rejects_wrong_owner_context_and_nonce_domain() {
    let view_key = [0x11; 32];
    let nod = fixed_nod();
    assert_eq!(
        decrypt_nod_for_owner(&[0x12; 32], &nod).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_context = nod.clone();
    wrong_context.terms.owner = Address::repeat_byte(0x43);
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &wrong_context).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_binding = nod.clone();
    wrong_binding.encryption_binding = B256::repeat_byte(0xa6);
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &wrong_binding).unwrap_err(),
        "NOD amount decryption failed"
    );
    let mut wrong_domain = nod;
    wrong_domain.encrypted_gratis_amount = hex::decode(NOD_WRONG_NONCE_DOMAIN_BLOB).unwrap();
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &wrong_domain).unwrap_err(),
        "NOD amount decryption failed"
    );
}

struct MalformedCiphertextCase<T> {
    amount_blob: fn(&mut T) -> &mut Vec<u8>,
    invalid_encoding: &'static str,
    decryption_failed: &'static str,
}

impl MalformedCiphertextCase<EncryptedNodV1> {
    fn nod() -> Self {
        Self {
            amount_blob: |value| &mut value.encrypted_gratis_amount,
            invalid_encoding: "invalid encrypted NOD encoding",
            decryption_failed: "NOD amount decryption failed",
        }
    }
}

fn assert_malformed_ciphertext_cases<T: Clone, R: std::fmt::Debug>(
    original: &T,
    decrypt: impl Fn(&T) -> Result<R, LocalCryptoError>,
    case: MalformedCiphertextCase<T>,
) {
    let mut short = original.clone();
    (case.amount_blob)(&mut short).pop();
    assert_eq!(decrypt(&short).unwrap_err(), case.invalid_encoding);

    let mut bad_version = original.clone();
    (case.amount_blob)(&mut bad_version)[7] = 2;
    assert_eq!(decrypt(&bad_version).unwrap_err(), case.invalid_encoding);

    let mut bad_ciphertext = original.clone();
    (case.amount_blob)(&mut bad_ciphertext)[8] ^= 1;
    assert_eq!(
        decrypt(&bad_ciphertext).unwrap_err(),
        case.decryption_failed
    );

    let mut bad_tag = original.clone();
    let tag_blob = (case.amount_blob)(&mut bad_tag);
    assert!(!tag_blob.is_empty(), "golden ciphertext must have a tag");
    let tag_index = tag_blob.len() - 1;
    tag_blob[tag_index] ^= 1;
    assert_eq!(decrypt(&bad_tag).unwrap_err(), case.decryption_failed);
}

#[test]
fn nod_rejects_malformed_length_version_and_tag() {
    let view_key = [0x11; 32];
    let nod = fixed_nod();
    assert_malformed_ciphertext_cases(
        &nod,
        |value| decrypt_nod_for_owner(&view_key, value),
        MalformedCiphertextCase::nod(),
    );
}
