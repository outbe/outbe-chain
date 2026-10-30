//! Fixed external HKDF-SHA256 and ChaCha20-Poly1305 vectors for owner-local views.
//! Ciphertexts were generated independently with Python cryptography 41.0.7.

use alloy_primitives::{hex, Address, B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV1, NodTermsV1},
    time::WorldwideDay,
    tribute_encryption::{EncryptedTributeV2, TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::{
    fidelity_decrypt::decrypt_fidelity_cohorts, gratis_decrypt::decrypt_gratis_balance,
    nod_decrypt::decrypt_nod_for_owner, promis_decrypt::decrypt_promis_balance,
    tribute_decrypt::decrypt_tribute_for_owner,
};

const FIDELITY_BLOB: &str = "01020304050607084649443231313131313131313131313131313131313131313131313131313131313131316f227d375d26e7d0c69faf0aa386d6b2f7e739b4de99fc0828f54a8b22d4e223";
const GRATIS_BLOB: &str = "111213141516171847524132515151515151515151515151515151515151515151515151515151515151515146185e002fe9d80f2f80daea54dcfbd2faa5ce7014252c82b2d6dccd3455cc7a79fdb10771779339ce5f26d6aa2f2027";
const PROMIS_BLOB: &str = "111213141516171850524f325151515151515151515151515151515151515151515151515151515151515151950f76eb13183701977ca3510b80c426f3dacad91248fa9961b5857074a00025091a5da1a6224be9617c475e65c22d71";
const NOD_CONTEXT_DIGEST: &str = "9f5ed554cb633715b10ef9300eee7572dd8d7bd9b7ee96f47b23f87152ae6720";
const NOD_AMOUNT_BLOB: &str = "000000000000000137f1780dda06389d4099f641acdc47b29f3834a3915aebccfda1eeb5cb1be0eb266f7e6a40d529099d3ea3df0ee44c17";
const NOD_WRONG_NONCE_DOMAIN_BLOB: &str = "0000000000000001b4cb27b25a97fb3f37dc0a8ee5525ce83bce9c4fdd1efb170fd412e8a76ea5bcd8feaf71b8df565d8b0d2b7d838b26cd";
const TRIBUTE_CONTEXT_DIGEST: &str =
    "79d19e3d69598d12698496e748a6ddef52b72392b6722778db2de3c2ef493d8a";
const TRIBUTE_AMOUNT_BLOB: &str = "000000000000000179a55ad0e1010fe5e34697e51f7cd2437e3825a0b0fd20c9095ca5186559021c48a5068d2190e57034954031961c1261d83647abd7ca0081e6c6d3f80aa4dcde6ef60e38fb95d1696d93c830a06309f3";
const TRIBUTE_WRONG_NONCE_DOMAIN_BLOB: &str = "0000000000000001ba08d886d9013e403c4668ff1ddad52207e01f20700ba87fbb3f49751b47325443f62a6997f16843e213f5e0ce6a52dc508fde6afd0c84511998055fb26ae1f70db5ce08a32f5cfc5217bdd630937bc0";

fn fixed_nod() -> EncryptedNodV1 {
    let day = WorldwideDay::new(20_260_824);
    let nod = EncryptedNodV1 {
        terms: NodTermsV1 {
            chain_id: 31_337,
            // Fixed external crypto vector, not a production issuance identity.
            nod_id: U256::from_str_radix(
                "013527d8abababababababababababababababababababababababababababab",
                16,
            )
            .unwrap(),
            owner: Address::repeat_byte(0x42),
            worldwide_day: day,
            league_id: 7,
            entry_price_minor: U256::from(123_456u64),
            issuance_currency: 840,
            reference_currency: 978,
        },
        encryption_binding: B256::repeat_byte(0xa5),
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
            offer_input_hash: B256::repeat_byte(0xb6),
        },
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
fn fixed_nod_and_tribute_ciphertexts_open_exact_amounts() {
    let view_key = [0x11; 32];
    assert_eq!(
        decrypt_nod_for_owner(&view_key, &fixed_nod()).unwrap(),
        U256::from(987_654_321u64)
    );
    assert_eq!(
        decrypt_tribute_for_owner(&view_key, &fixed_tribute()).unwrap(),
        TributeAmountsV2 {
            issuance_amount_minor: U256::from(123_456_789u64),
            nominal_amount_minor: U256::from(9_876_543_210u64),
        }
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

#[test]
fn tribute_rejects_wrong_owner_context_and_nonce_domain() {
    let view_key = [0x11; 32];
    let tribute = fixed_tribute();
    assert_eq!(
        decrypt_tribute_for_owner(&[0x12; 32], &tribute).unwrap_err(),
        "Tribute amount decryption failed"
    );
    let mut wrong_context = tribute.clone();
    wrong_context.context.owner = Address::repeat_byte(0x54);
    assert_eq!(
        decrypt_tribute_for_owner(&view_key, &wrong_context).unwrap_err(),
        "Tribute amount decryption failed"
    );
    let mut wrong_offer = tribute.clone();
    wrong_offer.context.offer_input_hash = B256::repeat_byte(0xb7);
    assert_eq!(
        decrypt_tribute_for_owner(&view_key, &wrong_offer).unwrap_err(),
        "Tribute amount decryption failed"
    );
    let mut wrong_domain = tribute;
    wrong_domain.encrypted_amounts = hex::decode(TRIBUTE_WRONG_NONCE_DOMAIN_BLOB).unwrap();
    assert_eq!(
        decrypt_tribute_for_owner(&view_key, &wrong_domain).unwrap_err(),
        "Tribute amount decryption failed"
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

impl MalformedCiphertextCase<EncryptedTributeV2> {
    fn tribute() -> Self {
        Self {
            amount_blob: |value| &mut value.encrypted_amounts,
            invalid_encoding: "invalid encrypted Tribute encoding",
            decryption_failed: "Tribute amount decryption failed",
        }
    }
}

fn assert_malformed_ciphertext_cases<T: Clone, R: std::fmt::Debug>(
    original: &T,
    decrypt: impl Fn(&T) -> Result<R, outbe_tee::LocalCryptoError>,
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

#[test]
fn tribute_rejects_malformed_length_version_and_tag() {
    let view_key = [0x11; 32];
    let tribute = fixed_tribute();
    assert_malformed_ciphertext_cases(
        &tribute,
        |value| decrypt_tribute_for_owner(&view_key, value),
        MalformedCiphertextCase::tribute(),
    );
}
