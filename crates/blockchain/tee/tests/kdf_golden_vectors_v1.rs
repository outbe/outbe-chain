//! Production identity golden vectors, independent of the implementation under test.
//! HKDF inputs: salt=identity_seed, IKM=[], info=the five versioned TEE labels.
//! X25519/Ed25519 outputs: Python cryptography 48.0.0 from independent RFC5869 secrets.
//! BLS outputs: independent Python RFC9380 SHA256 XMD (L=48), scalar reduction,
//! and affine BLS12-381 G1 multiplication; [0x01;32] also matches the existing known answer.
//!
use alloy_primitives::hex;
use outbe_tee::resident::ResidentTeeKeys;

#[test]
fn production_identity_key_derivation_vectors() {
    for (identity_seed, noise, recipient, attestation, dkg_enc, bls) in [
(hex!("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
"8448c6a773b7a972f7081d884d5db43748fab3050493c12199636581fd77687f",
"71bc0a41d38aa2390a5b5c89c75f7afe23927b1eb51dc0c2c042585d3e9e7f06",
"4eca380880bfe19d2e38151e9c97aade3a8b9f7c169c1b212af5e57a78dd9ace",
"6a5fc62a553e7823cc3b095f81775d0569a319c33ed7947120b6ae7269181338",
"af8d2d0142a8b6ccada4891bf331ce042c60affb87568d6dcc8d861f6c701d4ea2ee07dbe62f75662057c03c4f2387bb",
),
(hex!("0101010101010101010101010101010101010101010101010101010101010101"),
"2dc97022634f70a7eceffc6370d3c94e097582ad40dd5f00ceeee9d1983a4a42",
"f226dd3e87a77d95d979264613cb73408253e4c9cb384eda48e8d7ab18ed1200",
"40ca3f8183669f002f35f3ce55e90dcb37e1a5121d74ee425f61c0220515ae6a",
"1cfc918c6812791cf1f142e7b522594432aacebc9e8026e5cb44202d07c9d319",
"b6178141d30a5a91df599cf9a9b089651b1f7f25945a51a4cd02542276b43a30815322d7a002335ca563124e86f624ce",
),
    ] {
        let keys = ResidentTeeKeys::from_seed(identity_seed).unwrap();
        assert_eq!(hex::encode(keys.noise_public()), noise);
        assert_eq!(hex::encode(keys.tribute_offer_public()), recipient);
        assert_eq!(hex::encode(keys.attestation_pub()), attestation);
        assert_eq!(hex::encode(keys.dkg_enc_public()), dkg_enc);
        assert_eq!(hex::encode(keys.tee_bls_public_bytes()), bls);
    }
}

#[test]
fn production_threshold_offer_signing_message() {
    assert_eq!(
        outbe_tee::resident::TEE_OFFER_MESSAGE,
        b"outbe/tee/offer/v1"
    );
}
