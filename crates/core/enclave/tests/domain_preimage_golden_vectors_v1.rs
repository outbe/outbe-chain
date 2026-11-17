//! CHARACTERIZATION: domain response order and signed preimages survive transport retirement.
//! Postcard vectors use the Serde data model: unsigned LEB128 lengths/integers,
//! fixed-width big-endian U256 bytes, and declaration-order struct fields.
//! HKDF answers were independently calculated with Python hashlib/hmac (RFC 5869).
use alloy_primitives::{hex, Address, B256, U256};
use outbe_enclave::{
    confidential::{domain_for, ModifyAuthorization},
    crypto::hkdf_sha256,
    protocol::{derive_account_keys_message, eip191_hash, Ledger},
};
use outbe_tee::resident::{AccountKeysHandle, StateKeyHandle};

#[test]
fn owner_derive_account_keys_eip191_vectors() {
    for (ledger, preimage, digest) in [
(Ledger::Gratis, "6f757462652f6772617469732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "77543b6a6a9d609001a95b206671a7546f624787aa3ddad02db4ff553ae0a114"),
(Ledger::Promis, "6f757462652f70726f6d69732f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "4358a5160a44f31ab9bd747e2829922960336f30d54af3d1e2b62c5b00708326"),
(Ledger::Fidelity, "6f757462652f666964656c6974792f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "0253bd228725e355d543f755abffbaae4b7a317d9d6c7cd09ffd8213abdb15cb"),
(Ledger::Tribute, "6f757462652f747269627574652f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "44dc4c5ed328d65e056f5cee5156dea2005c20ecf49fd604a0e21c29b0b1ba69"),
(Ledger::Nod, "6f757462652f6e6f642f6465726976652d6b6579732f763122222222222222222222222222222222222222223333333333333333333333333333333333333333333333333333333333333333", "4bc1efbf18d8381d779979ea9dfdb8d87b1cdb2acc6cef8b42ee5ef60652af8b"),
    ] {
        let message = derive_account_keys_message(ledger, Address::repeat_byte(0x22), B256::repeat_byte(0x33));
        assert_eq!(hex::encode(&message), preimage);
        assert_eq!(hex::encode(eip191_hash(&message)), digest);
    }
}

#[test]
fn confidential_ledger_domain_vectors() {
    let request = ModifyAuthorization {
        account: Address::repeat_byte(0x22),
        op_tag: 3,
        amount: U256::from(17),
        op_nonce: 5,
        chain_id: B256::repeat_byte(0x11),
    };
    for (ledger, state, view, modify, mac) in [
        (
            Ledger::Gratis,
            "e013387bceef27a343ed0a944ad6da239cbd3417b3bf6d5013e22b67eca56aed",
            "34e065f26cd8de1bef9cdbee177dd98fecc503548706012a462014e77ba4ce9a",
            "743337cd82800c7d896ce426a094fb0f1a2280a5d30d8af35fb45860ea2cca57",
            "9b134699089490db390711a5c5e4c8d648ed168babad9c4105c17a36655c882b",
        ),
        (
            Ledger::Promis,
            "d50d5b049862d893c48d0991e5fcff5b8d45b573b323905c0cdae3778a895f36",
            "c3bd3c43c45fcdad0093cb3f6758b3f69c4a16c27e0c5e50c68d53079c1dabfb",
            "0320763f3ed5479059f29142165691624c3849c9b869b403ae354ab31d58b3eb",
            "d8704b22c0854b641b9cae741c4c7f995c3e5ee7b305a3bb78ba42d95d869afc",
        ),
        (
            Ledger::Fidelity,
            "5e96b66f685c7e2dcb2a1a7adeb6f027c107ac3b022c9b0bab3f7036eaaa20eb",
            "8ac47b1effa55d1c7b8d287055a49a4ced9b67214cb2d7e0e51750b7b5d0f1d0",
            "a7c69dcb52cf5651c2031e5a4594f62dd243c7525f9f02f12445bd459afb6cd3",
            "77f9857ad831c6e5950e73549fae8e9e37a1e8421fb8d0f9910e5da319832c01",
        ),
        (
            Ledger::Tribute,
            "d874becdca15a3e897070aac769e39397c2293526e702bc930c9e58ed6836e3c",
            "80909ae81fb557fc5a4b11de87fa17aaaf10ca2db4e0265e4676e7448d605925",
            "e9c40040205c342b36d1d76affe9aa28b488274ceb5c828c5ee780499209dabf",
            "2089316487fe407b9584c368a62244d9b187a3b69eb41b04c658fa99750bd4bf",
        ),
        (
            Ledger::Nod,
            "f068ed0d51419d40863b308b94592ae3dee981b77ad94c8eb7b97f819c061410",
            "2754605a328a631e5caa9f0da9bc07366b624b761dcbd7c92ac19f14805e97e1",
            "2c9e9247c5b980297f47a7154fee2372e015efc36ca21960705d49c79425ccc6",
            "7d3d88e09726be021d3b4ae05fd8cf83ae3e65dcc8a87e5c56d50a089491ea4c",
        ),
    ] {
        let domain = domain_for(ledger);
        let state_key = domain
            .derive_state_key(
                &outbe_enclave::dev::offer_material(&[0x44; 32], B256::repeat_byte(0x11)),
                B256::repeat_byte(0x11),
                7,
            )
            .unwrap();
        assert_key_vector(&state_key, state);
        let view_key = domain
            .account_keys
            .derive_view_key(&state_key, request.account)
            .unwrap();
        let modify_key = domain
            .account_keys
            .derive_account_keys(&state_key, request.account)
            .unwrap();
        assert_key_vector(&view_key, view);
        assert_eq!(
            domain.authorization.modify_mac(&modify_key, &request),
            domain.authorization.modify_mac(
                &AccountKeysHandle::for_test(
                    request.account,
                    [0; 32],
                    hex::decode(modify).unwrap().try_into().unwrap()
                ),
                &request
            ),
        );
        assert_eq!(
            hex::encode(domain.authorization.modify_mac(&modify_key, &request)),
            mac
        );
    }
}

// S2-T09 (outbe-chain-tfl.10): production outbe-tee derivation must reproduce
// these five HKDF-SHA256 outputs for salt/seed 00..1f, empty IKM, label=info.
// Production key outputs are additionally pinned in the enclave library integration suite.
#[test]
fn tee_identity_kdf_reference_vectors() {
    for (info, output) in [
        (
            b"outbe/tee/noise-static/v1".as_slice(),
            "6b9b55813d4152187af7c8c4c9e26b367ff6f2a1c8915cdc9c23bc9b0ea2b945",
        ),
        (
            b"outbe/tee/recipient-x25519/v1".as_slice(),
            "d1aaf721ba2468cbf4da3120453d68a4eb095028dea9ddbf17922d152e44a69e",
        ),
        (
            b"outbe/tee/attestation-ed25519/v1".as_slice(),
            "3107bac161498f5fc7604daba904fb44f7c445cb43eff88241390e37edc9f585",
        ),
        (
            b"outbe/tee/dkg-enc/v1".as_slice(),
            "8690d36dae706b74d992dc4d7b4c6a761b5c4945a5185dcad3b6b910a8941fce",
        ),
        (
            b"outbe/tee/dkg-bls-seed/v1".as_slice(),
            "9b0b2b00e3dd2a234db5bcc2e573f28ea74e5c56edc80a37cab8c793ffccba15",
        ),
    ] {
        let seed: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap());
        assert_eq!(hex::encode(hkdf_sha256(&seed, b"", info).unwrap()), output);
    }
}

#[test]
fn production_attestation_identity_public_key_vector() {
    // Python cryptography Ed25519 public key from HKDF secret 3107bac1... on seed 00..1f.
    let identity_seed: [u8; 32] = std::array::from_fn(|i| u8::try_from(i).unwrap());
    let key = outbe_tee::resident::ResidentTeeKeys::from_seed(identity_seed).unwrap();
    assert_eq!(
        hex::encode(key.attestation_pub()),
        "4eca380880bfe19d2e38151e9c97aade3a8b9f7c169c1b212af5e57a78dd9ace"
    );
}

#[path = "domain_preimage_golden_vectors_v1/attestations.rs"]
mod attestations;
#[path = "domain_preimage_golden_vectors_v1/canonical_hashes.rs"]
mod canonical_hashes;
#[path = "domain_preimage_golden_vectors_v1/ciphers.rs"]
mod ciphers;
#[path = "domain_preimage_golden_vectors_v1/fixtures.rs"]
mod fixtures;
#[path = "domain_preimage_golden_vectors_v1/operation_ciphers.rs"]
mod operation_ciphers;
#[path = "domain_preimage_golden_vectors_v1/responses.rs"]
mod responses;
#[path = "domain_preimage_golden_vectors_v1/result_bindings.rs"]
mod result_bindings;
#[path = "domain_preimage_golden_vectors_v1/signed_clients.rs"]
mod signed_clients;

// Compare an opaque derivation through a cryptographic operation, never through key extraction.
fn assert_key_vector(handle: &StateKeyHandle, expected: &str) {
    let expected = StateKeyHandle::for_test(hex::decode(expected).unwrap().try_into().unwrap());
    assert_eq!(
        handle.compute_mac(b"golden", b"key parity"),
        expected.compute_mac(b"golden", b"key parity")
    );
}
