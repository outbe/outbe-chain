use crate::transport::tests::*;

pub(in crate::transport::tests) fn testnet_chain_word() -> [u8; 32] {
    alloy_primitives::U256::from(outbe_primitives::chain::TESTNET_CHAIN_ID).to_be_bytes()
}

pub(in crate::transport::tests) fn production_dcap_state(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
) -> InitializationState {
    InitializationState::production_with_challenge_and_attestation(
        boot,
        keys,
        keys.attestation_pub(),
        crate::gramine::AttestationType::Dcap,
    )
    .unwrap()
}

pub(in crate::transport::tests) fn signed_initialization_manifest(
    keys: &EnclaveKeys,
    challenge: [u8; 32],
    node_host_noise_x25519: [u8; 32],
) -> (
    outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    [u8; 65],
) {
    signed_initialization_manifest_for_mode(
        keys,
        challenge,
        node_host_noise_x25519,
        AttestationMode::DcapRequired,
    )
}

pub(in crate::transport::tests) fn signed_initialization_manifest_for_mode(
    keys: &EnclaveKeys,
    challenge: [u8; 32],
    node_host_noise_x25519: [u8; 32],
    attestation_mode: AttestationMode,
) -> (
    outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    [u8; 65],
) {
    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    use outbe_primitives::tee_attestation_v1::{EnclaveInitializationManifestV1, NodeIdV1};

    let signing = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let node_id = NodeIdV1 {
        reth_p2p_public: signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    };
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: testnet_chain_word(),
        genesis_hash: B256::repeat_byte(0x11),
        attestation_mode,
        node_id,
        initialization_challenge: challenge,
        node_host_noise_x25519,
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
        .sign_prehash(manifest.authorization_hash().unwrap().as_slice())
        .unwrap();
    let mut signature_bytes = [0u8; 65];
    signature_bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
    signature_bytes[64] = recovery.to_byte();
    (manifest, signature_bytes)
}

#[cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]
pub(in crate::transport::tests) fn intent_bound_processor_fixture_wire_bytes() -> (Vec<u8>, Vec<u8>)
{
    use outbe_primitives::tee_attestation_v1::{
        AttestationEvidenceV1, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
        RegistrationIntentV1,
    };

    const ROOT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/system/tee/tests/fixtures/",
        "intel-dcap-1.26-intent-bound-processor/"
    );
    let intent = RegistrationIntentV1::decode_canonical(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/system/tee/tests/fixtures/",
        "intel-dcap-1.26-intent-bound-processor/intent.bin"
    )))
    .unwrap();
    let components = [
        (
            DcapCollateralKind::PckCertificateChain,
            "pck-certificate-chain.pem0",
        ),
        (DcapCollateralKind::PckCrl, "pck.crl.der"),
        (
            DcapCollateralKind::PckCrlIssuerChain,
            "pck-crl-issuer-chain.pem",
        ),
        (DcapCollateralKind::RootCaCrl, "root-ca.crl.der"),
        (DcapCollateralKind::TcbInfo, "tcb-info.json"),
        (
            DcapCollateralKind::TcbInfoIssuerChain,
            "tcb-info-issuer-chain.pem",
        ),
        (DcapCollateralKind::QeIdentity, "qe-identity.json"),
        (
            DcapCollateralKind::QeIdentityIssuerChain,
            "qe-identity-issuer-chain.pem",
        ),
    ]
    .into_iter()
    .map(|(kind, name)| DcapCollateralComponentV1 {
        kind,
        bytes: std::fs::read(format!("{ROOT}{name}")).unwrap(),
    })
    .collect();
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent,
        quote: std::fs::read(format!("{ROOT}quote.bin")).unwrap(),
        components,
        transition_key_ready_proof: None,
    })
    .encode_canonical()
    .unwrap();
    let policy = std::fs::read(format!("{ROOT}policy.bin")).unwrap();
    (evidence, policy)
}

// --- Fix A (C1) adversarial: DkgOpen must reject host tampering of the
// (bls, enc, sig) bundle before trusting any pairing ----------------------

pub(in crate::transport::tests) fn honest_announces(
    n: usize,
) -> (
    Vec<Enclave>,
    Vec<outbe_tee::protocol::ParticipantAnnounce>,
    B256,
) {
    let mut enclaves: Vec<Enclave> = (0..n).map(|i| Enclave::new(i as u8 + 1)).collect();
    let participant_bls = enclaves
        .iter_mut()
        .map(Enclave::bls_public)
        .collect::<Vec<_>>();
    let participant_set_hash =
        outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(&participant_bls)
            .unwrap();
    let binding = enclaves[0]
        .initialization
        .network_binding()
        .unwrap()
        .unwrap();
    let ceremony_id =
        outbe_primitives::tee_attestation_v1::dkg_ceremony_id_v1(&binding, 0, participant_set_hash)
            .unwrap();
    let participants = enclaves
        .iter_mut()
        .map(|enclave| enclave.identity(ceremony_id, participant_bls.clone()))
        .collect();
    (enclaves, participants, ceremony_id)
}

pub(in crate::transport::tests) fn open_on(
    enclave: &mut Enclave,
    participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
) -> EnclaveResponse {
    enclave.call(EnclaveRequest::DkgOpen {
        ceremony_id: participants[0].ceremony_id,
        round: 0,
        participants,
    })
}

// ---- Seal / unseal offer secret + share (cfg(test) mock sealing key) ----

pub(in crate::transport::tests) fn install_tribute_offer_key(
    secret: [u8; 32],
    share: Vec<u8>,
) -> (SharedTributeOfferKey, [u8; 32]) {
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let derived = DerivedTributeOfferKey::from_secret_and_group_sig(
        Zeroizing::new(secret),
        Zeroizing::new(share),
    );
    let public = derived.public();
    offer_key.set(derived).ok().expect("set offer key");
    (offer_key, public)
}

pub(in crate::transport::tests) fn sealed_test_binding(chain_id: [u8; 32]) -> NetworkBindingV1 {
    NetworkBindingV1 {
        chain_id,
        genesis_hash: B256::repeat_byte(0xE1),
        attestation_mode: AttestationMode::DcapRequired,
    }
}

/// Deterministic secp256k1 signer and its EVM address.
pub(in crate::transport::tests) fn evm_signer(
    seed: u8,
) -> (k256::ecdsa::SigningKey, alloy_primitives::Address) {
    let sk = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("key");
    let point = sk.verifying_key().to_encoded_point(false);
    let addr = alloy_primitives::Address::from_slice(
        &alloy_primitives::keccak256(&point.as_bytes()[1..])[12..],
    );
    (sk, addr)
}

/// EIP-191 owner signature over the shared derive-keys message - the exact
/// preimage the enclave arm recomputes.
pub(in crate::transport::tests) fn owner_sig(
    sk: &k256::ecdsa::SigningKey,
    account: alloy_primitives::Address,
    ephemeral: [u8; 32],
) -> Vec<u8> {
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    let prehash =
        outbe_tee::protocol::eip191_hash(&outbe_tee::protocol::derive_gratis_keys_message(
            account,
            alloy_primitives::B256::from(ephemeral),
        ));
    let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        sk.sign_prehash(prehash.as_slice()).expect("sign");
    let mut sig65 = [0u8; 65];
    sig65[..64].copy_from_slice(sig.to_bytes().as_slice());
    sig65[64] = recid.to_byte();
    sig65.to_vec()
}
