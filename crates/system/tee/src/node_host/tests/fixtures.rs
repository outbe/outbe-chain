use super::*;

pub(super) struct ReplacementFixture {
    _root: tempfile::TempDir,
    pub(super) node_data_dir: PathBuf,
    pub(super) paths: NodeHostPaths,
    pub(super) node_host_public: [u8; 32],
    pub(super) active: EnclaveInitializationManifestV1,
    pub(super) candidate: EnclaveInitializationManifestV1,
    pub(super) authorization: FinalizedReplacementAuthorizationV1,
}

pub(super) fn replacement_fixture() -> ReplacementFixture {
    replacement_fixture_for_operation(AttestationOperationV1::ReplaceEnclaveBinding)
}

pub(super) fn replacement_fixture_for_operation(
    operation: AttestationOperationV1,
) -> ReplacementFixture {
    replacement_fixture_for_mode(operation, AttestationMode::DcapRequired)
}

pub(super) fn replacement_fixture_for_mode(
    operation: AttestationOperationV1,
    attestation_mode: AttestationMode,
) -> ReplacementFixture {
    let root = tempfile::tempdir().unwrap();
    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();
    let paths = NodeHostPaths::new(&node_data_dir);
    ensure_private_directory(&paths.root).unwrap();
    let node_host = NodeHostNoiseKey::create_new(&paths.noise_key).unwrap();
    let node_host_public = node_host.public();
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
    let reth_p2p_public = node_signer
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
    let node_id = NodeIdV1 { reth_p2p_public };
    let active = EnclaveInitializationManifestV1 {
        chain_id: alloy_primitives::U256::from(19_u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x14),
        attestation_mode,
        node_id: node_id.clone(),
        initialization_challenge: [0x41; 32],
        node_host_noise_x25519: node_host.public(),
        recipient_x25519: [0x51; 32],
        attestation_ed25519: [0x52; 32],
        noise_responder_x25519: [0x53; 32],
    };
    let candidate = EnclaveInitializationManifestV1 {
        initialization_challenge: [0x42; 32],
        recipient_x25519: [0x61; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [0x63; 32],
        ..active.clone()
    };
    write_manifest_once(&paths.manifest, &active, &paths.root).unwrap();
    let record = ReplacementCandidateRecordV1 {
        predecessor_manifest_hash: active.authorization_hash().unwrap(),
        manifest: candidate.clone(),
    };
    write_bytes_once(
        &paths.replacement_candidate,
        &record.encode_canonical().unwrap(),
        &paths.root,
    )
    .unwrap();

    let intent = RegistrationIntentV1 {
        chain_id: candidate.chain_id,
        genesis_hash: candidate.genesis_hash,
        operation,
        attestation_mode,
        policy_hash: B256::repeat_byte(0x21),
        node_id,
        enclave_id: candidate.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x45),
        binding_version: 2,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: u64::from(
            operation == AttestationOperationV1::TransitionEnclaveMeasurement,
        ),
        requested_valid_until: 20_000,
        recipient_x25519: candidate.recipient_x25519,
        attestation_ed25519: candidate.attestation_ed25519,
        noise_responder_x25519: candidate.noise_responder_x25519,
        node_host_authorization_hash: candidate.node_host_authorization_hash().unwrap(),
    };
    let intent_hash = intent.intent_hash().unwrap();
    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        node_signer.sign_prehash(intent_hash.as_slice()).unwrap();
    let mut node_signature = [0_u8; 65];
    node_signature[..64].copy_from_slice(signature.to_bytes().as_slice());
    node_signature[64] = recovery.to_byte();
    let enclave_signature = enclave_signer.sign(intent_hash.as_slice()).to_bytes();
    let transition_key_ready_proof =
        (operation == AttestationOperationV1::TransitionEnclaveMeasurement).then(|| {
            let mut proof = TransitionKeyReadyProofV1 {
                chain_id: intent.chain_id,
                genesis_hash: intent.genesis_hash,
                transition_intent_hash: intent_hash,
                candidate_manifest_hash: candidate.authorization_hash().unwrap(),
                transition_nonce: intent.transition_nonce,
                resident_offer_public: [0x71; 32],
                candidate_attestation_signature: [0; 64],
            };
            proof.candidate_attestation_signature = enclave_signer
                .sign(proof.signing_hash().unwrap().as_slice())
                .to_bytes();
            proof
        });
    let evidence = match attestation_mode {
        AttestationMode::DcapRequired => AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote: vec![0x51],
            components: (1_u8..=8)
                .map(|kind| DcapCollateralComponentV1 {
                    kind: DcapCollateralKind::try_from(kind).unwrap(),
                    bytes: vec![kind],
                })
                .collect(),
            transition_key_ready_proof,
        }),
        AttestationMode::GramineDirectDev => AttestationEvidenceV1::GramineDirectDev(
            outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1 {
                intent,
                dev_attestation_public: enclave_signer.verifying_key().to_bytes(),
                dev_signature: enclave_signature,
            },
        ),
    };
    persist_replacement_candidate_submission(
        &node_data_dir,
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    let candidate_hash = candidate.authorization_hash().unwrap();
    let authorization = FinalizedReplacementAuthorizationV1::for_test(intent_hash, candidate_hash);
    ReplacementFixture {
        _root: root,
        node_data_dir,
        paths,
        node_host_public,
        active,
        candidate,
        authorization,
    }
}

impl FinalizedReplacementAuthorizationV1 {
    #[cfg(test)]
    pub(super) fn for_test(intent_hash: B256, candidate_manifest_hash: B256) -> Self {
        Self {
            intent_hash,
            candidate_manifest_hash,
        }
    }
}
