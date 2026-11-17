//! Compatibility contracts use frozen vectors and the unchanged client primitives.
use super::crypto_ops::{derive, export_sealed_account_keys_inner, open, shared};
use super::*;
use alloy_primitives::{hex, keccak256, Address, B256};
use zeroize::Zeroizing;

#[test]
fn identity_hkdf_known_answers_and_secret_debug_redaction() {
    let seed = std::array::from_fn::<_, 32, _>(|i| i as u8);
    for (label, expected) in [
        (
            "noise-static",
            "6b9b55813d4152187af7c8c4c9e26b367ff6f2a1c8915cdc9c23bc9b0ea2b945",
        ),
        (
            "recipient-x25519",
            "d1aaf721ba2468cbf4da3120453d68a4eb095028dea9ddbf17922d152e44a69e",
        ),
        (
            "attestation-ed25519",
            "3107bac161498f5fc7604daba904fb44f7c445cb43eff88241390e37edc9f585",
        ),
        (
            "dkg-enc",
            "8690d36dae706b74d992dc4d7b4c6a761b5c4945a5185dcad3b6b910a8941fce",
        ),
        (
            "dkg-bls-seed",
            "9b0b2b00e3dd2a234db5bcc2e573f28ea74e5c56edc80a37cab8c793ffccba15",
        ),
    ] {
        let info = format!("outbe/tee/{label}/v1");
        assert_eq!(
            hex::encode(*derive(&seed, b"", info.as_bytes()).unwrap()),
            expected
        );
    }
    let keys = ResidentTeeKeys::from_seed(seed).unwrap();
    let material = material();
    let state = material
        .derive_state_key_handle(b"state/", B256::ZERO, 0)
        .unwrap();
    let account = derive_account_keys_handle(&state, Address::ZERO, b"view", b"modify").unwrap();
    assert_eq!(format!("{keys:?}"), "ResidentTeeKeys([REDACTED])");
    assert_eq!(format!("{material:?}"), "ResidentOfferMaterial([REDACTED])");
    assert_eq!(format!("{state:?}"), "StateKeyHandle([REDACTED])");
    assert_eq!(format!("{account:?}"), "AccountKeysHandle([REDACTED])");
    assert_eq!(
        format!("{:?}", material.offer_key_handle()),
        "OfferKeyHandle([REDACTED])"
    );
    let msg = b"domain attestation";
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&keys.attestation_pub()).unwrap();
    vk.verify_strict(
        msg,
        &ed25519_dalek::Signature::from_bytes(&keys.sign_domain_attestation(msg)),
    )
    .unwrap();
}

fn material() -> ResidentOfferMaterial {
    ResidentOfferMaterial::for_test(
        b"founding group signature".to_vec(),
        B256::repeat_byte(0x21),
        3,
        4,
    )
    .unwrap()
}

#[test]
fn handles_preserve_state_account_slot_mac_and_context_derivation() {
    let group = material();
    let chain = B256::repeat_byte(0x21);
    let account = Address::repeat_byte(0x42);
    let state = group
        .derive_state_key_handle(b"outbe/gratis/state-key/v1/", chain, 0)
        .unwrap();
    let raw_state = derive(
        chain.as_slice(),
        b"founding group signature",
        b"outbe/gratis/state-key/v1/0",
    )
    .unwrap();
    let keys = derive_account_keys_handle(&state, account, b"view", b"modify").unwrap();
    let raw_view = derive(raw_state.as_ref(), account.as_slice(), b"view").unwrap();
    let raw_modify = derive(raw_state.as_ref(), account.as_slice(), b"modify").unwrap();
    let blob = slot_encrypt(&keys, b"nonce", (account, 7), 4, b"private balance").unwrap();
    assert_eq!(&blob[..8], &5u64.to_be_bytes());
    let ctx = crate::tee_decrypt::counter_slot_nonce_context(account.as_slice(), 7, 5);
    assert_eq!(
        crate::tee_decrypt::tee_decrypt_raw(&raw_view, &ctx, b"nonce", &blob[8..])
            .unwrap()
            .as_slice(),
        b"private balance"
    );
    let (v, pt) = slot_decrypt(&keys, b"nonce", account, 7, &blob).unwrap();
    assert_eq!((v, pt.as_slice()), (5, b"private balance".as_slice()));
    assert!(slot_decrypt(&keys, b"other nonce", account, 7, &blob).is_err());
    assert!(slot_encrypt(&keys, b"nonce", (Address::ZERO, 7), 4, b"x").is_err());
    let expected = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, raw_modify.as_ref()),
        b"tagmessage",
    );
    let tag = compute_modify_mac(&keys, b"tag", b"message");
    assert_eq!(tag.as_slice(), expected.as_ref());
    assert!(verify_modify_mac(&keys, b"tag", b"message", &tag));
    assert!(!verify_modify_mac(&keys, b"tag", b"changed", &tag));
    let subkey = keys
        .view_key_handle()
        .derive_with_salt(b"digest", b"record")
        .unwrap();
    let raw_subkey = derive(b"digest", raw_view.as_ref(), b"record").unwrap();
    let cipher = subkey.aead_encrypt(&[1; 12], b"record").unwrap();
    assert_eq!(
        open(&raw_subkey, &[1; 12], &cipher).unwrap().as_slice(),
        b"record"
    );
    assert_eq!(
        subkey.aead_decrypt(&[1; 12], &cipher).unwrap().as_slice(),
        b"record"
    );
}

#[test]
fn v2_owner_local_fixed_vector_and_transition_binding() {
    use crate::tee_decrypt::V2EnvelopeDomain;
    let key = StateKeyHandle::for_test([0x11; 32]);
    let account = Address::repeat_byte(0x42);
    let domain = V2EnvelopeDomain {
        scope: &[],
        marker: b"FID2",
        key_info: b"outbe/fidelity/cohort-key/v2",
        nonce_info: b"outbe/fidelity/cohort-nonce/v2",
    };
    // Independently generated Python cryptography owner-local vector.
    let blob = hex::decode("01020304050607084649443231313131313131313131313131313131313131313131313131313131313131316f227d375d26e7d0c69faf0aa386d6b2f7e739b4de99fc0828f54a8b22d4e223").unwrap();
    let (version, plaintext) = key.decrypt_v2_envelope(account, domain, &blob).unwrap();
    assert_eq!(version, 0x0102030405060708);
    assert_eq!(plaintext.as_slice(), b"cohort-v2-golden");
    let seal = |payload| {
        key.encrypt_v2_envelope(account, domain, b"binding", (0, payload), b"state")
            .unwrap()
    };
    let first = seal(b"first predecessor");
    assert_eq!(first, seal(b"first predecessor"));
    assert_ne!(first, seal(b"other predecessor"));
    assert_eq!(
        key.decrypt_v2_envelope(account, domain, &first)
            .unwrap()
            .1
            .as_slice(),
        b"state"
    );
    assert!(key
        .decrypt_v2_envelope(Address::ZERO, domain, &first)
        .is_err());
    assert!(key
        .encrypt_v2_envelope(account, domain, b"binding", (u64::MAX, b""), b"")
        .is_err());
}

fn owner() -> (k256::ecdsa::SigningKey, Address) {
    let signing = k256::ecdsa::SigningKey::from_bytes((&[0x41; 32]).into()).unwrap();
    let point = signing.verifying_key().to_encoded_point(false);
    let address = Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..]);
    (signing, address)
}
fn signature(signing: &k256::ecdsa::SigningKey, hash: B256) -> [u8; 65] {
    let (sig, id) = signing.sign_prehash_recoverable(hash.as_slice()).unwrap();
    let mut bytes = [0; 65];
    bytes[..64].copy_from_slice(&sig.to_bytes());
    bytes[64] = id.to_byte();
    bytes
}

#[test]
fn sealed_export_owner_gate_and_client_compatible_payload() {
    let (owner, address) = owner();
    let hash = keccak256(b"owner EIP191 preimage");
    let sig = signature(&owner, hash);
    let state = StateKeyHandle::for_test([0x13; 32]);
    let keys = derive_account_keys_handle(&state, address, b"view", b"modify").unwrap();
    let requester_secret = [0x32; 32];
    let requester = crypto_ops::x25519_public(&requester_secret);
    let export = export_sealed_account_keys_inner(
        &keys,
        (hash, &sig),
        &requester,
        b"outbe/tee/dkg-share/v1",
        (Zeroizing::new([0x33; 32]), [0x34; 12]),
    )
    .unwrap();
    let expected_view = derive(&[0x13; 32], address.as_slice(), b"view").unwrap();
    let expected_modify = derive(&[0x13; 32], address.as_slice(), b"modify").unwrap();
    // Owner-side ECDH + RFC5869 + AEAD opening, unchanged wire layout.
    let shared = shared(&requester_secret, &export.enclave_ephemeral_pubkey).unwrap();
    let key = derive(&requester, shared.as_ref(), b"outbe/tee/dkg-share/v1").unwrap();
    let plain = open(&key, &export.nonce, &export.sealed).unwrap();
    assert_eq!(&plain[..32], expected_view.as_ref());
    assert_eq!(&plain[32..], expected_modify.as_ref());
    assert_eq!(export.sealed.len(), 80);
    // Historical TypeScript MCP sealed-account-key fixture with fixed transport material.
    let fixture_keys = AccountKeysHandle::for_test(address, [0x5a; 32], [0x6b; 32]);
    let fixture = export_sealed_account_keys_inner(
        &fixture_keys,
        (hash, &sig),
        &hex!("ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59"),
        b"outbe/tee/dkg-share/v1",
        (Zeroizing::new([1; 32]), hex!("ab02a2e3c93cbf650bbcb1e5")),
    )
    .unwrap();
    assert_eq!(hex::encode(fixture.sealed), "80a9b32b46568b2ea28136735fa781f6b793f6bbfcb79229b25819b263cc0a00bbc312b2adc6c8993e00454d31a0e22016eb63d08f4116e4160c86db327e0ba2e3cec9b2ebb2720d0e5f3fa104c71c4e");
    let production =
        export_sealed_account_keys(&keys, hash, &sig, &requester, b"outbe/tee/dkg-share/v1")
            .unwrap();
    assert_ne!(
        production.enclave_ephemeral_pubkey,
        export.enclave_ephemeral_pubkey
    );
    let production_shared =
        crypto_ops::shared(&requester_secret, &production.enclave_ephemeral_pubkey).unwrap();
    let production_key = derive(
        &requester,
        production_shared.as_ref(),
        b"outbe/tee/dkg-share/v1",
    )
    .unwrap();
    let production_plaintext =
        open(&production_key, &production.nonce, &production.sealed).unwrap();
    assert_eq!(production_plaintext.len(), 64);
    assert_eq!(&production_plaintext[..32], expected_view.as_ref());
    assert_eq!(&production_plaintext[32..], expected_modify.as_ref());
    let other = derive_account_keys_handle(&state, Address::ZERO, b"view", b"modify").unwrap();
    assert!(matches!(
        export_sealed_account_keys(&other, hash, &sig, &requester, b"info"),
        Err(TeeCryptoError::WrongOwner)
    ));
    assert!(matches!(
        export_sealed_account_keys(&keys, hash, &[0xff; 65], &requester, b"info"),
        Err(TeeCryptoError::OwnerSignature(_))
    ));
    assert!(export_sealed_account_keys(&keys, hash, &sig, &[0; 32], b"info").is_err());
}

#[test]
fn deterministic_onboarding_is_exact_and_rejects_nonce_recipient_or_context_changes() {
    let group = material();
    let recipient = ResidentTeeKeys::from_seed([0x32; 32]).unwrap();
    let context = crate::dcap_protocol::DcapOnboardingContextV1 {
        chain_id: [0x21; 32],
        genesis_hash: B256::repeat_byte(0x22),
        intent_hash: B256::repeat_byte(0x23),
        node_id_hash: B256::repeat_byte(0x24),
        enclave_id: B256::repeat_byte(0x25),
        binding_id: B256::repeat_byte(0x26),
        policy_hash: B256::repeat_byte(0x27),
        recipient_x25519: recipient.tribute_offer_public(),
        network_encryption_public: group.public(),
        key_epoch: 3,
        network_encryption_epoch: 4,
    };
    let mut low_order_context = context;
    low_order_context.recipient_x25519 = [0; 32];
    assert!(encrypt_onboarding_artifact_v1(&group, low_order_context).is_err());
    let artifact = encrypt_onboarding_artifact_v1(&group, context).unwrap();
    assert_eq!(
        artifact,
        encrypt_onboarding_artifact_v1(&group, context).unwrap()
    );
    let imported = decrypt_onboarding_artifact_v1(&recipient, &artifact).unwrap();
    assert_eq!(imported.public(), group.public());
    let a = imported
        .derive_state_key_handle(b"ledger/", B256::ZERO, 0)
        .unwrap()
        .compute_mac(b"", b"check");
    let b = group
        .derive_state_key_handle(b"ledger/", B256::ZERO, 0)
        .unwrap()
        .compute_mac(b"", b"check");
    assert_eq!(a, b);
    let mut tampered = artifact.clone();
    tampered.nonce[0] ^= 1;
    assert!(decrypt_onboarding_artifact_v1(&recipient, &tampered).is_err());
    tampered = artifact.clone();
    tampered.context.intent_hash = B256::ZERO;
    assert!(decrypt_onboarding_artifact_v1(&recipient, &tampered).is_err());
    assert!(decrypt_onboarding_artifact_v1(
        &ResidentTeeKeys::from_seed([9; 32]).unwrap(),
        &artifact
    )
    .is_err());
}

#[test]
fn stage0_onboarding_and_v2_sealing_known_answers() {
    use crate::dcap_protocol::DcapOnboardingContextV1;
    use crate::tee_decrypt::V2EnvelopeDomain;
    let material = ResidentOfferMaterial {
        secret: OfferKeyHandle::for_test([0x11; 32]),
        public: crypto_ops::x25519_public(&[0x11; 32]),
        group_sig: Zeroizing::new(vec![0x44; 48]),
        key_epoch: 1,
        tribute_offer_epoch: 2,
    };
    let context = DcapOnboardingContextV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        intent_hash: B256::repeat_byte(0x33),
        node_id_hash: B256::repeat_byte(0x44),
        enclave_id: B256::repeat_byte(0x55),
        binding_id: B256::repeat_byte(0x66),
        policy_hash: B256::repeat_byte(0x77),
        recipient_x25519: hex!("0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20"),
        network_encryption_public: material.public(),
        key_epoch: 1,
        network_encryption_epoch: 2,
    };
    let artifact = encrypt_onboarding_artifact_v1(&material, context).unwrap();
    assert_eq!(hex::encode(artifact.nonce), "be5a7377c545f58c6176fadc");
    assert_eq!(hex::encode(artifact.ciphertext), "749e350209694623d6c6eb931d5950d5631ed922568376d9603417176f69accd548a1655caf00ffc264d5b70ea5d21443a6f3793a39021f3a55756ac902b4ff1");
    let key = StateKeyHandle::for_test([0x11; 32]);
    let mut amount = [0; 32];
    amount[31] = 17;
    let mut binding_payload = vec![0x33; 32];
    binding_payload.extend_from_slice(&amount);
    for (marker, tag, info, nonce, expected) in [
        (b"GRA2", b"outbe/gratis/transition-binding/v2".as_slice(), b"outbe/gratis/amount-key/v2".as_slice(), b"outbe/gratis/amount-nonce/v2".as_slice(),
        "000000000000000547524132487af11f7a8410e9cc3c0a1e9ed6a40e2e322e9c6f68aff3a10870102fb4973f3ea68cb7a480699247f9259e9915748b1df8c1aeff474f9bf56851dc7534672f365421ed5f4e4de356aa5f9ed900186a"),
        (b"PRO2", b"outbe/promis/transition-binding/v2".as_slice(), b"outbe/promis/amount-key/v2".as_slice(), b"outbe/promis/amount-nonce/v2".as_slice(),
        "000000000000000550524f32a0da28be8b5eae47acd5ff7d780f9af69f684ae69bf79607a10669cae33289fe0f9aa44218f93e4fff57a2be4de213b4f586d48ef0fbb007619920a7c46774c710a9cfe3f7d31af4452aefcac0d4904c"),
    ] {
        let domain = V2EnvelopeDomain { scope: &[0], marker, key_info: info, nonce_info: nonce };
        let blob = key.encrypt_v2_envelope(Address::repeat_byte(0x22), domain, tag,
            (4, &binding_payload), &amount).unwrap();
        assert_eq!(hex::encode(&blob), expected);
        assert_eq!(key.decrypt_v2_envelope(Address::repeat_byte(0x22), domain, &blob).unwrap().1.as_slice(), amount);
    }
}

#[test]
fn offer_handle_opens_frozen_client_amount_and_rejects_wrong_domain() {
    let key = OfferKeyHandle::for_test([0x22; 32]);
    let salt = b"outbe/tribute/offer-salt/v1\0\0\0\0\0";
    let mut info = b"outbe/promis/mine-gratis/v1".to_vec();
    info.extend_from_slice(&[0x11; 32]);
    info.extend_from_slice(&[0x22; 20]);
    let peer = hex!("7b0d47d93427f8311160781c7c733fd89f88970aef490d8aa0ee19a4cb8a1b14");
    let ciphertext = hex!("15dd34fefbe7ff9dbf9935085336f7d6fcd20f3c5ff206379ce02cb510880409996a3389fb2410081f29e11996f5d88e");
    let plaintext = key
        .ecdhe_decrypt(salt, &info, &peer, &[0x44; 12], &ciphertext)
        .unwrap();
    let mut amount = [0; 32];
    amount[31] = 17;
    assert_eq!(plaintext.as_slice(), amount);
    assert!(key
        .ecdhe_decrypt(salt, b"wrong domain", &peer, &[0x44; 12], &ciphertext)
        .is_err());
    assert!(key
        .ecdhe_decrypt(salt, &info, &[0; 32], &[0x44; 12], &ciphertext)
        .is_err());
}

#[test]
fn offer_and_dkg_low_order_peers_preserve_legacy_ecdhe_behavior() {
    let salt = b"legacy salt";
    let info = b"legacy domain";
    let nonce = [0x54; 12];
    // X25519 with a low-order peer produces zero shared bytes in the original path.
    let key = derive(salt, &[0; 32], info).unwrap();
    let ciphertext = crypto_ops::seal(&key, &nonce, b"legacy offer").unwrap();
    let handle = OfferKeyHandle::for_test([0x22; 32]);
    assert_eq!(
        handle
            .ecdhe_decrypt(salt, info, &[0; 32], &nonce, &ciphertext)
            .unwrap()
            .as_slice(),
        b"legacy offer"
    );
    let share =
        crypto_ops::encrypt_share(&[0; 32], b"outbe/tee/dkg-share/v1", b"legacy share").unwrap();
    let key = derive(&[0; 32], &[0; 32], b"outbe/tee/dkg-share/v1").unwrap();
    assert_eq!(
        open(&key, &share.nonce, &share.ciphertext)
            .unwrap()
            .as_slice(),
        b"legacy share"
    );
}
