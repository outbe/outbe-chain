//! Independent Python HMAC/HKDF/Keccak/ChaCha20Poly1305 answers.
use super::fixtures::*;
use alloy_primitives::{hex, B256, U256};
use outbe_enclave::{confidential, nod_encryption, tribute_day};

#[test]
fn day_and_fractions_operation_cipher_vectors() {
    let request = parse(
        r#"{"chain_id":1,"worldwide_day":20261116,"previous":null,"public_state_hash":"0x4444444444444444444444444444444444444444444444444444444444444444","operation":{"AdjustTransient":{"nominal_amount_minor":"0x11","add":true}}}"#,
    );
    let record = tribute_day::apply_day_operation(&[0x11; 32], &[0x22; 32], &request).unwrap();
    assert_eq!(
        hex::encode(record.operation_hash),
        "53c4ddef5ae4bc813af83ea2edef9df28470bbab6a435c1f081bf95c0429ed8f"
    );
    assert_eq!(hex::encode(&record.encrypted_amount), "00000000000000017e0a0ac94bae297284f94dacf4d1f90e2b93d7ff95146ffdf4624daa091a2510291cf7b9fda1e9ce07a5421067a1291a");
    assert_eq!(
        tribute_day::read_day_amount(&[0x11; 32], &record).unwrap(),
        U256::from(17)
    );
    let fractions = std::collections::BTreeMap::from([(3, 17), (7, 19)]);
    let record = tribute_day::seal_day_fractions(
        &[0x11; 32],
        (1, 20261116.into(), 20261115.into(), 123),
        B256::repeat_byte(0x33),
        &fractions,
    )
    .unwrap();
    assert_eq!(
        hex::encode(record.operation_hash),
        "1c1f9b6b73c40a67c5600a7e5e32a994632891d620bb2e0fb71f7ce85a690f13"
    );
    assert_eq!(
        hex::encode(&record.encrypted_fractions),
        "0000000000000001ec968e84b69d4f893ff8e439f2fa5e3a9cf5d96b0e"
    );
    assert_eq!(
        tribute_day::read_day_fractions(&[0x11; 32], &record).unwrap(),
        fractions
    );
}

#[test]
fn group_load_operation_cipher_vector() {
    let record = nod_encryption::seal_group_load(
        &[0x11; 32],
        (1, B256::repeat_byte(0x22)),
        (4, B256::repeat_byte(0x33)),
        U256::from(17),
    )
    .unwrap();
    assert_eq!(
        hex::encode(record.operation_hash),
        "19a504024cb859540b7e5f16c23ea58e63a9a686b7b3d4fc77aadebfce515728"
    );
    assert_eq!(hex::encode(&record.encrypted_load), "00000000000000050df785dc512c51a800f6a87718dc2b42527e8506f38b3a58d346adac0043beac9ecc66369afc09a12d5b6fe3c170a337");
    assert_eq!(
        nod_encryption::read_nod_group_load(&[0x11; 32], &record).unwrap(),
        U256::from(17)
    );
}

#[test]
fn nod_quote_cipher_vector() {
    let nod = nod_encryption::encrypt_nod(&[0x11; 32], parse(TERMS), U256::from(17)).unwrap();
    let context = parse(QUOTE_CONTEXT);
    let actual =
        nod_encryption::encrypt_nod_quote(&[0x11; 32], &nod, &context, U256::from(18)).unwrap();
    assert_eq!(hex::encode(actual), "000000000000000182cfc1644fef4b01724c9f47659e77518492bdf99def8013e2774ec4e97da20695839161d5f79728ab26f93db62c2cf1");
}

#[test]
fn event_and_state_source_binding_vectors() {
    assert_eq!(
        hex::encode(confidential::event_amount_input_hash(B256::repeat_byte(
            0x33
        ))),
        "5dea5dcd766111745d06eeca38638fc31594f1e02d93f02f65a13bd9beb4c541"
    );
    assert_eq!(
        hex::encode(
            outbe_enclave::tribute_source::source_binding(
                &parse(TRIBUTE),
                5,
                &[(B256::repeat_byte(0x44), U256::from(17))]
            )
            .unwrap()
        ),
        "bb06c8ae5a8d25b4b4bf4477fa4ad5d37d5ba0a2e4f3cbcb21ae7321d636fa6c"
    );
}
