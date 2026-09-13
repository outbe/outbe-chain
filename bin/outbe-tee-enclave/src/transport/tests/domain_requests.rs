use crate::transport::tests::*;

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
    let before_ready = status.class_ready;
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
        status.class_ready > before_ready,
        "ready-class counter must grow"
    );
    assert!(status.requests_total >= status.class_ready);
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
