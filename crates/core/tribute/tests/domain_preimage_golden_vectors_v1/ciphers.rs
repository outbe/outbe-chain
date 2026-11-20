//! Behavioral cipher answers from independent Python HKDF/HMAC/ChaCha20-Poly1305.
use super::support_seal::encrypt_onboarding_artifact_v1;
use super::support_share::encrypt_share_deterministic;
use alloy_primitives::{hex, Address, B256, U256};
use outbe_core_common::confidential_tee::{
    decrypt_fidelity_cohorts, decrypt_gratis_balance, decrypt_promis_balance, BalanceTransition,
    LocalCryptoError, TeeError,
};
use outbe_core_common::{
    nod_encryption::NodTermsV1,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
};
use outbe_fidelity::{write_blob, CohortTransition};
use outbe_gratis::cipher as gratis_cipher;
use outbe_nod::nod_encryption;
use outbe_promis::cipher as promis_cipher;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::resident::StateKeyHandle;
use outbe_tribute::confidential::encrypt_tribute;

#[test]
fn mutable_balance_and_cohort_cipher_vectors() {
    let transition = || BalanceTransition {
        account: Address::repeat_byte(0x22),
        field: 0,
        previous_version: 4,
        amount: U256::from(17),
        input_hash: B256::repeat_byte(0x33),
    };
    type WriteAmount = fn(&StateKeyHandle, BalanceTransition) -> Result<Vec<u8>, TeeError>;
    type ReadAmount = fn(&[u8; 32], Address, &[u8]) -> Result<U256, LocalCryptoError>;
    for (write, read, expected) in [
        (gratis_cipher::write_amount as WriteAmount, decrypt_gratis_balance as ReadAmount, "000000000000000547524132487af11f7a8410e9cc3c0a1e9ed6a40e2e322e9c6f68aff3a10870102fb4973f3ea68cb7a480699247f9259e9915748b1df8c1aeff474f9bf56851dc7534672f365421ed5f4e4de356aa5f9ed900186a"),
        (promis_cipher::write_amount as WriteAmount, decrypt_promis_balance as ReadAmount, "000000000000000550524f32a0da28be8b5eae47acd5ff7d780f9af69f684ae69bf79607a10669cae33289fe0f9aa44218f93e4fff57a2be4de213b4f586d48ef0fbb007619920a7c46774c710a9cfe3f7d31af4452aefcac0d4904c"),
    ] {
        let actual = write(&StateKeyHandle::for_test([0x11; 32]), transition()).unwrap();
        assert_eq!(hex::encode(actual), expected);
        assert_eq!(read(&[0x11; 32], Address::repeat_byte(0x22), &hex::decode(expected).unwrap()).unwrap(), U256::from(17));
    }
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
fn immutable_tribute_and_nod_cipher_vectors() {
    let context: TributeContextV2 = serde_json::from_str(r#"{"chain_id":1,"tribute_id":"0x013528fc63636363636363636363636363636363636363636363636363636363","owner":"0x2222222222222222222222222222222222222222","worldwide_day":20261116,"issuance_currency":840,"reference_currency":978,"tribute_price_minor":"0x13","offer_input_hash":"0x6464646464646464646464646464646464646464646464646464646464646464"}"#).unwrap();
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: U256::from(17),
        nominal_amount_minor: U256::from(18),
    };
    let actual = encrypt_tribute(&StateKeyHandle::for_test([0x11; 32]), context, &amounts).unwrap();
    assert_eq!(hex::encode(actual.encrypted_amounts), "000000000000000198a4b541b8b5c614da53e1220561668ea032a7a6ac51584569aed305006f30580961ef3fe4ab762af8facfb6bd8eb418bd4c54ee144c996b62ae3158c1c92d49ae24a0316f5929cbb541a8c296b5f6a8");
    let terms: NodTermsV1 = serde_json::from_str(r#"{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978}"#).unwrap();
    let actual =
        nod_encryption::encrypt_nod(&StateKeyHandle::for_test([0x11; 32]), terms, U256::from(17))
            .unwrap();
    assert_eq!(
        hex::encode(actual.encryption_binding),
        "d2f75a55db07c33cccf750c02012e077beaaf5ee33877c53943be451c3db6bd9"
    );
    assert_eq!(hex::encode(actual.encrypted_gratis_amount), "0000000000000001669f99a7b9d70b9cb0cd668126a4c34d44acf8f56b90b94c9b8d093a5ba1e066e3d903c2c91c3a479dfa81082b9c147a");
}

#[test]
fn onboarding_and_registry_sealing_vectors() {
    let recipient_public = hex!("0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20");
    let context = DcapOnboardingContextV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        intent_hash: B256::repeat_byte(0x33),
        node_id_hash: B256::repeat_byte(0x44),
        enclave_id: B256::repeat_byte(0x55),
        binding_id: B256::repeat_byte(0x66),
        policy_hash: B256::repeat_byte(0x77),
        recipient_x25519: recipient_public,
        network_encryption_public: hex!(
            "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13"
        ),
        key_epoch: 1,
        network_encryption_epoch: 2,
    };
    assert_eq!(
        hex::encode(context.context_hash()),
        "8856c4e56be0e58a0c80bc23fe6d2225155598faae4cf662f24d8c44d9c2d3ea"
    );
    let artifact = encrypt_onboarding_artifact_v1(&[0x11; 32], context, &[0x44; 48]).unwrap();
    assert_eq!(hex::encode(artifact.nonce), "be5a7377c545f58c6176fadc");
    assert_eq!(hex::encode(artifact.ciphertext), "749e350209694623d6c6eb931d5950d5631ed922568376d9603417176f69accd548a1655caf00ffc264d5b70ea5d21443a6f3793a39021f3a55756ac902b4ff1");
    let sealed = encrypt_share_deterministic(&[0x11; 32], &recipient_public, &[0x44; 48]).unwrap();
    let sealed_bytes = sealed.to_bytes();
    assert_eq!(
        hex::encode(sealed.ephemeral_pub),
        "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13"
    );
    assert_eq!(hex::encode(sealed.nonce), "b964f7d7c875c855b509ec3d");
    assert_eq!(hex::encode(sealed.ciphertext), "9b48118c9544a28b30b914929a793efaccc9fd56b02192afe1c98c563a49604fe209053a6d342c030113e0ed0beb97eca7749c25ebaeb389c08e305a4eca629e");
    let again = encrypt_share_deterministic(&[0x11; 32], &recipient_public, &[0x44; 48]).unwrap();
    assert_eq!(sealed_bytes, again.to_bytes());
    let other_recipient = [0x55; 32];
    let other = encrypt_share_deterministic(&[0x11; 32], &other_recipient, &[0x44; 48]).unwrap();
    assert_ne!(sealed_bytes, other.to_bytes());
}
