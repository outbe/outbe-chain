use crate::transport::tests::*;

#[test]
fn initialized_request_dispatch_rejects_handshake_commands() {
    let mut enclave = Enclave::new(0x34);
    for request in [
        EnclaveRequest::OpenSession,
        EnclaveRequest::GetInitializationChallenge,
    ] {
        assert!(matches!(
            enclave.call(request),
            EnclaveResponse::Error { message }
                if message == "pre-handshake request is not valid inside a Noise session"
        ));
        assert!(enclave.offer_key.get().is_none());
    }
}

#[test]
fn account_key_authorization_cannot_cross_ledger_domains() {
    let mut enclave = resident_enclave(3);
    let (owner, account) = evm_signer(7);
    let ephemeral = [0x24; 32];
    let signature = owner_sig(&owner, account, ephemeral);
    for ledger in [
        outbe_tee::protocol::Ledger::Promis,
        outbe_tee::protocol::Ledger::Fidelity,
        outbe_tee::protocol::Ledger::Tribute,
        outbe_tee::protocol::Ledger::Nod,
    ] {
        assert!(matches!(
            enclave.call(EnclaveRequest::DeriveAccountKeys {
                ledger,
                account,
                requester_ephemeral_pubkey: ephemeral,
                owner_sig: signature.clone(),
            }),
            EnclaveResponse::Error { message }
                if message == "DeriveAccountKeys: owner signature does not control account"
        ));
    }
}

#[test]
fn health_reports_counters_uptime_and_offer_key_state() {
    let keys = EnclaveKeys::new([0x51; 32], Some([0x51; 32])).unwrap();
    let mut dkg = DkgSessionStore::new();
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let chain_id = B256::repeat_byte(0x52);

    let response = dispatch(
        EnclaveRequest::Health,
        &keys,
        &mut dkg,
        &offer_key,
        chain_id,
    );
    let EnclaveResponse::HealthStatus { status } = response else {
        panic!("expected HealthStatus, got {response:?}");
    };
    assert!(!status.offer_key_ready, "keyless enclave reports not-ready");

    // Counters are process-global: record a ready-class request and observe
    // the delta through a second Health probe.
    let before_ready = status.classes.ready;
    crate::telemetry::record_request(
        crate::telemetry::RequestClassLabel::Ready,
        crate::telemetry::RequestOutcome::Ok,
    );
    let resident = DerivedTributeOfferKey::from_secret_and_group_sig(
        Zeroizing::new([0x53; 32]),
        Zeroizing::new(vec![0x54; 96]),
    );
    offer_key.set(resident).ok().expect("install offer key");
    let response = dispatch(
        EnclaveRequest::Health,
        &keys,
        &mut dkg,
        &offer_key,
        chain_id,
    );
    let EnclaveResponse::HealthStatus { status } = response else {
        panic!("expected HealthStatus, got {response:?}");
    };
    assert!(status.offer_key_ready, "resident key reports ready");
    assert!(
        status.classes.ready > before_ready,
        "ready-class counter must grow"
    );
    assert!(status.requests.total >= status.classes.ready);
}

#[test]
fn public_keys_distinguish_onboarding_recipient_from_permanent_offer_key() {
    let mut enclave = Enclave::new(0x31);
    let onboarding_recipient = enclave.keys.tribute_offer_public();
    match enclave.call(EnclaveRequest::GetPublicKeys) {
        EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        } => {
            assert!(!offer_key_ready);
            assert_eq!(recipient_x25519_pub, onboarding_recipient);
        }
        other => panic!("unexpected pre-ready response: {other:?}"),
    }

    let permanent = DerivedTributeOfferKey::from_secret_and_group_sig(
        Zeroizing::new([0x6a; 32]),
        Zeroizing::new(vec![0x44; 96]),
    );
    let permanent_public = permanent.public();
    assert!(enclave.offer_key.set(permanent).is_ok(), "install once");

    match enclave.call(EnclaveRequest::GetPublicKeys) {
        EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        } => {
            assert!(offer_key_ready);
            assert_eq!(recipient_x25519_pub, permanent_public);
            assert_ne!(recipient_x25519_pub, onboarding_recipient);
        }
        other => panic!("unexpected ready response: {other:?}"),
    }
}

#[test]
fn derive_account_keys_requires_owner_sig() {
    let mut enclave = resident_enclave(1);
    let (owner, account) = evm_signer(7);
    let ephemeral = [0x22_u8; 32];

    // Correct owner signature -> keys sealed.
    let good = owner_sig(&owner, account, ephemeral);
    match enclave.call(EnclaveRequest::DeriveAccountKeys {
        ledger: outbe_tee::protocol::Ledger::Gratis,
        account,
        requester_ephemeral_pubkey: ephemeral,
        owner_sig: good,
    }) {
        EnclaveResponse::AccountKeysSealed { account: a, .. } => assert_eq!(a, account),
        other => panic!("expected AccountKeysSealed, got {other:?}"),
    }

    // A signature by a DIFFERENT key over the same account: an attacker who does
    // not control `account` (e.g. a compromised host) cannot obtain its keys.
    let (attacker, attacker_addr) = evm_signer(9);
    assert_ne!(attacker_addr, account);
    let forged = owner_sig(&attacker, account, ephemeral);
    assert!(
        matches!(
            enclave.call(EnclaveRequest::DeriveAccountKeys {
                ledger: outbe_tee::protocol::Ledger::Gratis,
                account,
                requester_ephemeral_pubkey: ephemeral,
                owner_sig: forged,
            }),
            EnclaveResponse::Error { .. }
        ),
        "keys must not be released without a signature by `account`"
    );
}

#[test]
fn derive_account_keys_ephemeral_binding() {
    let mut enclave = resident_enclave(2);
    let (owner, account) = evm_signer(7);
    let e1 = [0x11_u8; 32];
    let e2 = [0x99_u8; 32];

    // A valid signature over ephemeral E1 cannot be replayed to seal keys to a
    // different requester ephemeral E2 (the signed message binds the ephemeral).
    let sig_over_e1 = owner_sig(&owner, account, e1);
    assert!(
        matches!(
            enclave.call(EnclaveRequest::DeriveAccountKeys {
                ledger: outbe_tee::protocol::Ledger::Gratis,
                account,
                requester_ephemeral_pubkey: e2,
                owner_sig: sig_over_e1,
            }),
            EnclaveResponse::Error { .. }
        ),
        "owner_sig must bind the requester ephemeral (no cross-target replay)"
    );
}

fn unseal_derived_view_key(
    enclave: &mut Enclave,
    ledger: outbe_tee::protocol::Ledger,
    owner: &k256::ecdsa::SigningKey,
    account: alloy_primitives::Address,
    secret_seed: u8,
) -> [u8; 32] {
    let secret = [secret_seed; 32];
    let ephemeral =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret)).to_bytes();
    let owner_sig = owner_sig_for_ledger(ledger, owner, account, ephemeral);
    let EnclaveResponse::AccountKeysSealed {
        sealed,
        nonce,
        enclave_ephemeral_pubkey,
        ..
    } = enclave.call(EnclaveRequest::DeriveAccountKeys {
        ledger,
        account,
        requester_ephemeral_pubkey: ephemeral,
        owner_sig,
    })
    else {
        panic!("expected AccountKeysSealed for {ledger:?}");
    };
    let plaintext = crate::crypto::decrypt_share(
        &secret,
        &crate::crypto::EncryptedShare {
            ephemeral_pub: enclave_ephemeral_pubkey,
            nonce,
            ciphertext: sealed,
        },
    )
    .expect("unseal account keys");
    assert_eq!(plaintext.len(), 64);
    plaintext[..32].try_into().unwrap()
}

fn sample_encrypted_offer(
    recipient_pub: &[u8; 32],
    owner: alloy_primitives::Address,
    day: outbe_primitives::time::WorldwideDay,
) -> outbe_tee::protocol::EncryptedTributeOffer {
    use alloy_primitives::U256;
    let payload = serde_json::json!({
        "creator": owner,
        "tribute_draft_id": B256::repeat_byte(0x11),
        "amount_base": "100",
        "amount_micro": "0",
        "su_hashes": [B256::repeat_byte(0x22)],
    });
    let cipher_text = outbe_tee::offer_encrypt::encrypt_tribute_offer_with(
        recipient_pub,
        [9; 32],
        [1; 12],
        &serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();
    let eph = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([9; 32])).to_bytes();
    outbe_tee::protocol::EncryptedTributeOffer {
        owner,
        cipher_text,
        nonce: vec![1; 12],
        ephemeral_pubkey: U256::from_be_bytes(eph),
        worldwide_day: day,
        tribute_currency: 840,
        reference_currency: 978,
        issuance_wwd_vwap_minor: U256::from(2_000_000),
        reference_wwd_vwap_minor: U256::from(2_000_000),
        reference_scurve_minor: U256::ZERO,
        zk_context: None,
    }
}

#[test]
fn derive_account_keys_opens_tribute_and_nod_ciphertexts_from_dispatchers() {
    use alloy_primitives::U256;
    use outbe_primitives::{
        nod_encryption::NodTermsV1, time::WorldwideDay, tribute_encryption::TributeAmountsV2,
    };

    let mut enclave = resident_enclave(4);
    let (owner_sk, owner_addr) = evm_signer(7);
    let day = WorldwideDay::new(20250115);
    let recipient_pub = enclave.offer_key.get().expect("resident").public();
    let offer = sample_encrypted_offer(&recipient_pub, owner_addr, day);

    let EnclaveResponse::EncryptedTributeOfferBatchV2 { results, .. } =
        enclave.call(EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 {
            offers: vec![offer],
        })
    else {
        panic!("expected EncryptedTributeOfferBatchV2");
    };
    let tribute = results[0].tribute.clone().expect("created tribute");
    let tribute_view = unseal_derived_view_key(
        &mut enclave,
        outbe_tee::protocol::Ledger::Tribute,
        &owner_sk,
        owner_addr,
        0x41,
    );
    assert_eq!(
        outbe_tee::tribute_decrypt::decrypt_tribute_for_owner(&tribute_view, &tribute).unwrap(),
        TributeAmountsV2 {
            issuance_amount_minor: U256::from(100_000_000),
            nominal_amount_minor: U256::from(50_000_000),
        }
    );

    let terms = NodTermsV1 {
        chain_id: outbe_primitives::chain::TESTNET_CHAIN_ID,
        nod_id: outbe_primitives::nod_rules::derive_nod_id(owner_addr, day),
        owner: owner_addr,
        worldwide_day: day,
        league_id: 0,
        entry_price_minor: U256::from(700),
        issuance_currency: 840,
        reference_currency: 978,
    };
    let EnclaveResponse::NodEncryptedForTributeV1 { nod, .. } =
        enclave.call(EnclaveRequest::EncryptNodForTributeV1 {
            tribute: Box::new(tribute),
            terms,
            amount: U256::from(777),
        })
    else {
        panic!("expected NodEncryptedForTributeV1");
    };
    let nod_view = unseal_derived_view_key(
        &mut enclave,
        outbe_tee::protocol::Ledger::Nod,
        &owner_sk,
        owner_addr,
        0x42,
    );
    assert_eq!(
        outbe_tee::nod_decrypt::decrypt_nod_for_owner(&nod_view, &nod).unwrap(),
        U256::from(777)
    );
    assert!(outbe_tee::nod_decrypt::decrypt_nod_for_owner(&tribute_view, &nod).is_err());
}
