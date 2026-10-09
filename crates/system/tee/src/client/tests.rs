use super::*;
use crate::transition_key_ready::signed_transition_key_ready_proof;

fn registration_intent(attestation_ed25519: [u8; 32]) -> RegistrationIntentV1 {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, AttestationOperationV1, NodeIdV1};

    let reth_p2p_public = k256::ecdsa::SigningKey::from_bytes((&[0x14; 32]).into())
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();

    let mut intent = RegistrationIntentV1 {
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x12),
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: B256::repeat_byte(0x13),
        node_id: NodeIdV1 { reth_p2p_public },
        enclave_id: B256::repeat_byte(0x16),
        binding_id: B256::repeat_byte(0x17),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: [0x18; 32],
        attestation_ed25519,
        noise_responder_x25519: [0x19; 32],
        node_host_authorization_hash: B256::repeat_byte(0x1a),
    };
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    intent
}

fn dcap_response(canonical_intent: Vec<u8>, enclave_signature: Vec<u8>) -> EnclaveResponse {
    EnclaveResponse::DcapQuote {
        intent: canonical_intent,
        quote_body: vec![0x51; 512],
        enclave_signature,
        transition_key_ready_proof: Vec::new(),
    }
}

#[test]
fn generated_dcap_quote_requires_exact_echo_and_enclave_pop() {
    use ed25519_dalek::{Signer as _, SigningKey};

    let signer = SigningKey::from_bytes(&[0x21; 32]);
    let intent = registration_intent(signer.verifying_key().to_bytes());
    let canonical = intent.encode_canonical().unwrap();
    let signature = signer
        .sign(intent.intent_hash().unwrap().as_slice())
        .to_bytes()
        .to_vec();

    let accepted = validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        dcap_response(canonical.clone(), signature.clone()),
    )
    .unwrap();
    assert_eq!(accepted.quote_body.len(), 512);
    assert_eq!(accepted.enclave_signature.as_slice(), signature);

    let mut wrong_echo = canonical.clone();
    wrong_echo.push(0);
    assert!(validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        dcap_response(wrong_echo, signature.clone()),
    )
    .is_err());
    assert!(validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        dcap_response(canonical.clone(), vec![0; 63]),
    )
    .is_err());
    let mut bad_signature = signature;
    bad_signature[0] ^= 1;
    assert!(validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        dcap_response(canonical.clone(), bad_signature),
    )
    .is_err());
    assert!(validate_generated_dcap_quote(
        &intent,
        &canonical,
        [0x22; 32],
        EnclaveResponse::Error {
            message: "not trusted".into(),
        },
    )
    .is_err());
}

#[test]
fn generated_transition_quote_requires_valid_key_ready_proof() {
    use ed25519_dalek::{Signer as _, SigningKey};

    let signer = SigningKey::from_bytes(&[0x23; 32]);
    let mut intent = registration_intent(signer.verifying_key().to_bytes());
    intent.operation = AttestationOperationV1::TransitionEnclaveMeasurement;
    intent.registration_version = 1;
    intent.transition_nonce = 4;
    let canonical = intent.encode_canonical().unwrap();
    let enclave_signature = signer
        .sign(intent.intent_hash().unwrap().as_slice())
        .to_bytes()
        .to_vec();
    let mut proof = signed_transition_key_ready_proof(
        &intent,
        intent.intent_hash().unwrap(),
        B256::repeat_byte(0x24),
        [0x25; 32],
        &signer,
    );
    let response = EnclaveResponse::DcapQuote {
        intent: canonical.clone(),
        quote_body: vec![0x51; 512],
        enclave_signature: enclave_signature.clone(),
        transition_key_ready_proof: proof.encode_canonical().unwrap(),
    };
    let accepted = validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        response,
    )
    .unwrap();
    assert_eq!(accepted.transition_key_ready_proof, Some(proof));

    assert!(validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        dcap_response(canonical.clone(), enclave_signature.clone()),
    )
    .is_err());
    proof.resident_offer_public[0] ^= 1;
    assert!(validate_generated_dcap_quote(
        &intent,
        &canonical,
        signer.verifying_key().to_bytes(),
        EnclaveResponse::DcapQuote {
            intent: canonical.clone(),
            quote_body: vec![0x51; 512],
            enclave_signature,
            transition_key_ready_proof: proof.encode_canonical().unwrap(),
        },
    )
    .is_err());
}

#[test]
fn dcap_verification_response_requires_exact_hash_canonical_outcome_and_signature() {
    use ed25519_dalek::{Signer as _, SigningKey};

    let signer = SigningKey::from_bytes(&[0x71; 32]);
    let public = signer.verifying_key().to_bytes();
    let request_hash = B256::repeat_byte(0x72);
    let outcome = DcapVerificationOutcomeV1::Rejected(
        crate::dcap_protocol::DcapRejectCodeV1::PlatformTcbRejected,
    )
    .encode_canonical()
    .unwrap();
    let tag = signer
        .sign(&dcap_verification_attestation_preimage(request_hash, &outcome).unwrap())
        .to_bytes()
        .to_vec();
    let response = || EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash,
        outcome: outcome.clone(),
        attestation_tag: tag.clone(),
    };
    assert_eq!(
        validate_dcap_verification_response(public, request_hash, response()).unwrap(),
        DcapVerificationOutcomeV1::Rejected(
            crate::dcap_protocol::DcapRejectCodeV1::PlatformTcbRejected
        )
    );

    assert!(
        validate_dcap_verification_response(public, B256::repeat_byte(0x73), response()).is_err()
    );
    let mut bad_outcome = response();
    if let EnclaveResponse::DcapVerificationFinishedV1 { outcome, .. } = &mut bad_outcome {
        outcome.push(0);
    }
    assert!(validate_dcap_verification_response(public, request_hash, bad_outcome).is_err());
    let mut bad_tag = response();
    if let EnclaveResponse::DcapVerificationFinishedV1 {
        attestation_tag, ..
    } = &mut bad_tag
    {
        attestation_tag[0] ^= 1;
    }
    assert!(validate_dcap_verification_response(public, request_hash, bad_tag).is_err());
}

#[test]
fn onboarding_response_requires_signed_exact_artifact_and_valid_pairing() {
    use ed25519_dalek::{Signer as _, SigningKey};

    let signer = SigningKey::from_bytes(&[0x31; 32]);
    let public = signer.verifying_key().to_bytes();
    let request_hash = B256::repeat_byte(0x32);
    let outcome = DcapVerificationOutcomeV1::Accepted(crate::dcap_protocol::DcapVerdictV1 {
        mrenclave: B256::repeat_byte(0x33),
        mrsigner: B256::repeat_byte(0x34),
        isv_prod_id: 1,
        isv_svn: 2,
        pck_ca: crate::dcap_protocol::DcapPckCaV1::Processor,
        fmspc: [0x35; 6],
        pce_id: 3,
        platform_tcb_status: crate::dcap_protocol::DcapPlatformTcbStatusV1::UpToDate,
        advisory_ids: Vec::new(),
        tcb_evaluation_data_number: 4,
        qe_tcb_evaluation_data_number: 5,
        collateral_valid_until: 6,
    })
    .encode_canonical()
    .unwrap();
    let artifact = DcapOnboardingArtifactV1 {
        context: crate::test_utils::onboarding_context_fixture(
            [0x41, 0x42, 0x43, 0x44, 0x45, 0x4a, 0x4b, 0x46, 0x47],
            7,
            8,
        ),
        nonce: [0x48; 12],
        ciphertext: vec![0x49; 112],
    }
    .encode_canonical()
    .unwrap();
    let tag = signer
        .sign(&dcap_onboarding_attestation_preimage(request_hash, &outcome, &artifact).unwrap())
        .to_bytes()
        .to_vec();
    let response = || EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
        request_hash,
        outcome: outcome.clone(),
        onboarding_artifact: artifact.clone(),
        attestation_tag: tag.clone(),
    };
    let result = validate_dcap_onboarding_response(public, request_hash, response()).unwrap();
    assert!(matches!(
        result.outcome,
        DcapVerificationOutcomeV1::Accepted(_)
    ));
    assert!(result.artifact.is_some());

    let mut tampered = response();
    if let EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
        onboarding_artifact,
        ..
    } = &mut tampered
    {
        let last = onboarding_artifact.len() - 1;
        onboarding_artifact[last] ^= 1;
    }
    assert!(validate_dcap_onboarding_response(public, request_hash, tampered).is_err());

    let rejected_with_artifact = EnclaveResponse::DcapOnboardingVerificationFinishedV1 {
        request_hash,
        outcome: DcapVerificationOutcomeV1::Rejected(
            crate::dcap_protocol::DcapRejectCodeV1::NodeSignatureInvalid,
        )
        .encode_canonical()
        .unwrap(),
        onboarding_artifact: artifact,
        attestation_tag: tag,
    };
    assert!(
        validate_dcap_onboarding_response(public, request_hash, rejected_with_artifact).is_err()
    );
}

#[test]
fn enclave_timeout_is_bounded_and_configurable_for_sgx() {
    assert_eq!(timeout_seconds_from(None), 30);
    assert_eq!(timeout_seconds_from(Some("120")), 120);
    assert_eq!(timeout_seconds_from(Some("0")), 30);
    assert_eq!(timeout_seconds_from(Some("invalid")), 30);
}

#[test]
fn socket_eagain_is_reported_as_a_phase_timeout() {
    let error = TransportError::Io(std::io::Error::from(std::io::ErrorKind::WouldBlock));
    let mapped = with_io_phase::<()>(Err(error), "DKG response read").unwrap_err();
    assert!(matches!(
        mapped,
        TransportError::IoTimeout {
            operation: "DKG response read",
            timeout_secs: 30
        }
    ));
}
use crate::quote::MIN_QUOTE_LEN;

const RB: usize = 48; // report-body offset inside the quote

#[derive(Default)]
struct QuoteFields {
    mrenclave: B256,
    mrsigner: B256,
    isv_svn: u16,
    quote_body: Vec<u8>,
}

/// Build a Quote response with a correct report_data key binding.
fn quote_with(
    noise: [u8; 32],
    offer: [u8; 32],
    attest: [u8; 32],
    fields: QuoteFields,
) -> EnclaveResponse {
    let mut p = Vec::new();
    p.extend_from_slice(&noise);
    p.extend_from_slice(&offer);
    p.extend_from_slice(&attest);
    EnclaveResponse::Quote {
        mrenclave: fields.mrenclave,
        mrsigner: fields.mrsigner,
        isv_svn: fields.isv_svn,
        report_data: keccak256(&p),
        recipient_x25519_pub: offer,
        attestation_pub: attest,
        noise_static_pub: noise,
        quote_body: fields.quote_body,
        attestation: "none (test)".to_string(),
    }
}

/// gramine-direct/bare: an empty quote is accepted and the bound key is the
/// enclave Noise static key.
#[test]
fn accepts_unattested_empty_quote() {
    let q = quote_with([1; 32], [2; 32], [3; 32], QuoteFields::default());
    let pinned = verify_quote(&q).unwrap();
    assert_eq!(pinned, [1u8; 32]);
}

/// A valid per-offer attestation tag verifies. The verifier rejects any
/// tampering with the results, the inputs hash, the key, or the tag length.
#[test]
fn verify_tribute_offer_attestation_accepts_valid_rejects_tampering() {
    use crate::protocol::{TributeOfferResult, TributeOfferStatus};
    use alloy_primitives::{Address, U256};
    use ed25519_dalek::{Signer, SigningKey};

    let results = vec![TributeOfferResult {
        token_id: B256::repeat_byte(0x11),
        owner: Address::repeat_byte(0x22),
        issuance_amount_minor: U256::from(1_000u64),
        nominal_amount_minor: U256::from(2_000u64),
        effective_reference_price_minor: U256::from(3_000u64),
        su_hashes: vec!["0xabc".to_string()],
        wallet_addresses: vec![],
        sra_addresses: vec![],
        zk_expected_hashes: None,
        status: TributeOfferStatus::Created,
    }];
    let hash = B256::repeat_byte(0xAB);

    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let pk = sk.verifying_key().to_bytes();
    let preimage = crate::protocol::tribute_offer_attestation_preimage(hash, &results);
    let tag = sk.sign(&preimage).to_bytes();

    // Happy path.
    verify_tribute_offer_attestation(&pk, hash, &results, &tag).expect("valid tag verifies");

    // Tampered result -> reject.
    let mut tampered = results.clone();
    tampered[0].effective_reference_price_minor += U256::ONE;
    assert!(verify_tribute_offer_attestation(&pk, hash, &tampered, &tag).is_err());

    // Tampered inputs hash -> reject.
    assert!(
        verify_tribute_offer_attestation(&pk, B256::repeat_byte(0xCD), &results, &tag).is_err()
    );

    // Wrong key -> reject.
    let other = SigningKey::from_bytes(&[8u8; 32])
        .verifying_key()
        .to_bytes();
    assert!(verify_tribute_offer_attestation(&other, hash, &results, &tag).is_err());

    // Bad tag length -> reject.
    assert!(verify_tribute_offer_attestation(&pk, hash, &results, &[0u8; 10]).is_err());
}

#[test]
fn gratis_promis_and_fidelity_tags_keep_domains_and_error_order() {
    use crate::protocol::{
        FidelityQueryResult, GratisOpResult, GratisOpStatus, PromisOpResult, PromisOpStatus,
    };
    use alloy_primitives::U256;
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

    let sk = SigningKey::from_bytes(&[7u8; 32]);
    let pk = sk.verifying_key().to_bytes();
    let hash = B256::repeat_byte(0xAB);
    let gratis = GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: vec![1],
        new_pledged: vec![2],
        event_amount: U256::from(3),
        next_op_nonce: 4,
        fidelity: None,
        inputs_canonical_hash: hash,
        attestation_tag: vec![],
    };
    let promis = PromisOpResult {
        status: PromisOpStatus::Applied,
        new_balance: vec![1],
        event_amount: U256::from(3),
        next_op_nonce: 4,
        inputs_canonical_hash: hash,
        attestation_tag: vec![],
    };
    let fidelity = FidelityQueryResult {
        rcfi: U256::from(5),
        efficiency: U256::from(6),
        league: 7,
        inputs_canonical_hash: hash,
        attestation_tag: vec![],
    };
    let gratis_tag = sk
        .sign(&crate::protocol::gratis_op_attestation_preimage(
            hash, &gratis,
        ))
        .to_bytes();
    let promis_tag = sk
        .sign(&crate::protocol::promis_op_attestation_preimage(
            hash, &promis,
        ))
        .to_bytes();
    let fidelity_tag = sk
        .sign(&crate::protocol::fidelity_query_attestation_preimage(
            hash, &fidelity,
        ))
        .to_bytes();

    verify_gratis_op_attestation(&pk, hash, &gratis, &gratis_tag).expect("gratis tag");
    verify_promis_op_attestation(&pk, hash, &promis, &promis_tag).expect("promis tag");
    verify_fidelity_query_attestation(&pk, hash, &fidelity, &fidelity_tag).expect("fidelity tag");
    assert!(matches!(
        verify_promis_op_attestation(&pk, hash, &promis, &gratis_tag),
        Err(TransportError::PromisOpAttestation(message))
            if message.starts_with("signature invalid:")
    ));
    assert!(matches!(
        verify_gratis_op_attestation(&pk, hash, &gratis, &promis_tag),
        Err(TransportError::GratisOpAttestation(message))
            if message.starts_with("signature invalid:")
    ));
    assert!(matches!(
        verify_fidelity_query_attestation(&pk, hash, &fidelity, &gratis_tag),
        Err(TransportError::FidelityAttestation(message))
            if message.starts_with("signature invalid:")
    ));

    // Canonical y = 2 has no Edwards x coordinate over 2^255 - 19.
    let mut malformed_key = [0u8; 32];
    malformed_key[0] = 2;
    assert!(VerifyingKey::from_bytes(&malformed_key).is_err());
    assert!(matches!(
        verify_gratis_op_attestation(&malformed_key, hash, &gratis, &[0u8; 1]),
        Err(TransportError::GratisOpAttestation(message))
            if message.starts_with("bad attestation key:")
    ));
    assert!(matches!(
        verify_promis_op_attestation(&pk, hash, &promis, &[0u8; 1]),
        Err(TransportError::PromisOpAttestation(message))
            if message == "bad tag length 1"
    ));
}

/// Pin the REPORT_DATA preimage byte order on the host side. The host
/// binds `keccak256(noise || recipient || attestation)`. A quote whose
/// report_data uses any other field order must fail the binding. Mirrors the
/// enclave's `report_data_preimage_order_is_pinned`.
#[test]
fn report_data_preimage_order_is_pinned_host() {
    let (noise, offer, attest) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    // Wrong order (noise || attest || offer) must NOT satisfy the binding.
    let mut wrong = Vec::new();
    wrong.extend_from_slice(&noise);
    wrong.extend_from_slice(&attest);
    wrong.extend_from_slice(&offer);
    let q = EnclaveResponse::Quote {
        mrenclave: B256::ZERO,
        mrsigner: B256::ZERO,
        isv_svn: 0,
        report_data: keccak256(&wrong),
        recipient_x25519_pub: offer,
        attestation_pub: attest,
        noise_static_pub: noise,
        quote_body: vec![],
        attestation: "none (test)".to_string(),
    };
    assert!(
        verify_quote(&q).is_err(),
        "a non-canonical preimage order must fail the report_data binding"
    );
}

/// A tampered cleartext public key breaks the report_data binding.
#[test]
fn rejects_report_data_binding_mismatch() {
    let mut q = quote_with([1; 32], [2; 32], [3; 32], QuoteFields::default());
    if let EnclaveResponse::Quote {
        noise_static_pub, ..
    } = &mut q
    {
        *noise_static_pub = [9; 32]; // no longer hashes to report_data
    }
    assert!(verify_quote(&q).is_err());
}

/// Cleartext measurements that disagree with the quote are rejected (the
/// quote bytes are the source of truth for this structural check).
#[test]
fn rejects_cleartext_measurements_not_matching_quote() {
    let (noise, offer, attest) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let mut body = vec![0u8; MIN_QUOTE_LEN];
    body[RB + 64..RB + 96].copy_from_slice(&[0xAA; 32]); // quote mrenclave
    body[RB + 128..RB + 160].copy_from_slice(&[0xBB; 32]); // quote mrsigner
    let mut p = Vec::new();
    p.extend_from_slice(&noise);
    p.extend_from_slice(&offer);
    p.extend_from_slice(&attest);
    let binding = keccak256(&p);
    body[RB + 320..RB + 352].copy_from_slice(binding.as_slice()); // quote report_data
                                                                  // cleartext claims mrenclave=CC, but the quote says AA -> reject.
    let q = quote_with(
        noise,
        offer,
        attest,
        QuoteFields {
            mrenclave: B256::from([0xCC; 32]),
            mrsigner: B256::from([0xBB; 32]),
            quote_body: body,
            ..QuoteFields::default()
        },
    );
    assert!(verify_quote(&q).is_err());
}

#[test]
fn node_host_key_store_is_write_once_owner_only_and_never_regenerates() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node-host-noise.key");
    let created = NodeHostNoiseKey::create_new(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        NodeHostNoiseKey::load(&path).unwrap().public(),
        created.public()
    );

    let create_error = NodeHostNoiseKey::create_new(&path).unwrap_err();
    assert!(matches!(
        create_error,
        TransportError::Io(ref error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
    ));

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(NodeHostNoiseKey::load(&path)
        .unwrap_err()
        .to_string()
        .contains("exactly 0600"));

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, [0x11; 31]).unwrap();
    assert!(NodeHostNoiseKey::load(&path).is_err());
    std::fs::write(&path, [0x11; 33]).unwrap();
    assert!(NodeHostNoiseKey::load(&path)
        .unwrap_err()
        .to_string()
        .contains("exactly 32 bytes"));

    std::fs::remove_file(&path).unwrap();
    let target = root.path().join("node-host-noise-target.key");
    std::fs::write(&target, [0x22; 32]).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(NodeHostNoiseKey::load(&path).is_err());
    std::fs::remove_file(&path).unwrap();

    assert!(matches!(
        NodeHostNoiseKey::load(&path),
        Err(TransportError::Io(ref error))
            if error.kind() == std::io::ErrorKind::NotFound
    ));
}
