use super::*;
use crate::transition_key_ready::signed_transition_key_ready_proof;
use outbe_primitives::tee_test_utils::sign_node_host_hash_for_test;

#[derive(Clone, Copy)]
pub(super) enum DirectorySync {
    Skip,
    Sync,
}

pub(super) fn stage_existing_record_as_next(
    final_path: &std::path::Path,
    next_path: &std::path::Path,
    root: &std::path::Path,
    directory_sync: DirectorySync,
) -> Vec<u8> {
    let bytes = std::fs::read(final_path).unwrap();
    std::fs::remove_file(final_path).unwrap();
    if matches!(directory_sync, DirectorySync::Sync) {
        File::open(root).unwrap().sync_all().unwrap();
    }
    write_bytes_once(next_path, &bytes, root).unwrap();
    bytes
}

pub(super) struct ReplacementFixture {
    _root: tempfile::TempDir,
    pub(super) node_data_dir: PathBuf,
    pub(super) paths: NodeHostPaths,
    pub(super) node_host_public: [u8; 32],
    pub(super) active: EnclaveInitializationManifestV1,
    pub(super) candidate: EnclaveInitializationManifestV1,
    pub(super) authorization: FinalizedReplacementAuthorizationV1,
}

/// A promotable DirectDev RegisterEnclave fixture with the evidence and both
/// signatures of its durable candidate submission.
pub(super) struct DirectDevRegistration {
    pub(super) fixture: ReplacementFixture,
    pub(super) evidence: AttestationEvidenceV1,
    pub(super) node_signature: [u8; 65],
    pub(super) enclave_signature: [u8; 64],
}

pub(super) fn direct_dev_registration() -> DirectDevRegistration {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::GramineDirectDev,
    );
    let candidate_submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence =
        AttestationEvidenceV1::decode_canonical(candidate_submission.evidence()).unwrap();
    let node_signature = *candidate_submission.node_signature();
    let enclave_signature = *candidate_submission.enclave_signature();
    DirectDevRegistration {
        fixture,
        evidence,
        node_signature,
        enclave_signature,
    }
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
    let node_signature = sign_node_host_hash_for_test(&node_signer, intent_hash);
    let enclave_signature = enclave_signer.sign(intent_hash.as_slice()).to_bytes();
    let transition_key_ready_proof =
        (operation == AttestationOperationV1::TransitionEnclaveMeasurement).then(|| {
            signed_transition_key_ready_proof(
                &intent,
                intent_hash,
                candidate.authorization_hash().unwrap(),
                [0x71; 32],
                &enclave_signer,
            )
        });
    let evidence = match attestation_mode {
        AttestationMode::DcapRequired => AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote: vec![0x51],
            components: crate::test_utils::canonical_dcap_collateral_fixture(),
            transition_key_ready_proof,
        }),
        AttestationMode::GramineDirectDev => AttestationEvidenceV1::GramineDirectDev(
            outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1 {
                transition_key_ready_proof: None,
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

#[derive(Clone, Copy)]
pub(super) enum RelayRecordKind {
    CommittedJoin,
    Replacement,
}

impl RelayRecordKind {
    fn path(self, paths: &NodeHostPaths) -> &std::path::Path {
        match self {
            Self::CommittedJoin => &paths.committed_join_relay,
            Self::Replacement => &paths.replacement_relay,
        }
    }

    const fn length_offset(self) -> usize {
        match self {
            Self::CommittedJoin => 105,
            Self::Replacement => 97,
        }
    }

    const fn record_name(self) -> &'static str {
        match self {
            Self::CommittedJoin => "committed join relay",
            Self::Replacement => "replacement relay",
        }
    }

    fn read_error(self, path: &std::path::Path) -> String {
        match self {
            Self::CommittedJoin => read_committed_join_relay(path).unwrap_err().to_string(),
            Self::Replacement => read_replacement_relay(path).unwrap_err().to_string(),
        }
    }
}

pub(super) fn assert_relay_decode_precedence(
    paths: &NodeHostPaths,
    relay: &[u8],
    kind: RelayRecordKind,
) -> std::io::Result<()> {
    let path = kind.path(paths);
    let length_offset = kind.length_offset();
    let record_name = kind.record_name();

    let mut invalid_frame = relay.to_vec();
    invalid_frame[0] = 2;
    invalid_frame[length_offset..length_offset + 4].fill(0);
    std::fs::write(path, invalid_frame)?;
    assert_eq!(
        kind.read_error(path),
        format!("codec error: {record_name} framing is invalid")
    );

    let mut invalid_length = relay.to_vec();
    invalid_length[1..97].fill(0);
    invalid_length[length_offset..length_offset + 4].fill(0);
    std::fs::write(path, invalid_length)?;
    assert_eq!(
        kind.read_error(path),
        format!("codec error: {record_name} raw transaction length is invalid")
    );

    let mut invalid_commitment = relay.to_vec();
    invalid_commitment[1..33].fill(0);
    std::fs::write(path, invalid_commitment)?;
    assert_eq!(
        kind.read_error(path),
        format!("codec error: {record_name} commitments are invalid")
    );
    Ok(())
}
