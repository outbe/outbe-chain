use super::*;
use outbe_primitives::{
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationOperationV1, NoAttestationDevEvidenceV1,
        RegistrationIntentV1, ValidatorNodeBindingV1,
    },
    tee_bootstrap_v2::{
        TeeBootstrapAuthorityV2, TeeBootstrapParticipantSubmissionV2, TeeBootstrapV2,
    },
    tee_test_utils::{
        no_attestation_dev_policy_v1, sign_node_host_hash_for_test, RegistrationIntentFixture,
    },
};

pub(super) const VALIDATOR_SEEDS: [u8; 4] = [1, 9, 17, 18];

pub(super) struct Bootstrap {
    pub(super) extra: Bytes,
    pub(super) payload: TeeBootstrapV2,
}

impl Bootstrap {
    pub(super) fn new(prepared: &Prepared) -> Self {
        let snapshot = committee_snapshot(prepared);
        let mut boundary = fixtures_execution::boundary_with(
            true,
            snapshot
                .committee
                .iter()
                .map(|entry| (entry.address, entry.consensus_pubkey))
                .collect(),
        );
        boundary.vrf_group_public_key = keccak256(&snapshot.vrf_group_public_key_bytes);
        boundary.vrf_group_public_key_bytes = snapshot.vrf_group_public_key_bytes.clone().into();
        boundary.committee_set_hash = snapshot.committee_set_hash_v2(0);
        boundary.outcome = prepared.committee.outcome().into();
        boundary.is_full_dkg = true;
        let extra = encode_outbe_block_artifacts(
            &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
                ..Default::default()
            },
        )
        .unwrap();
        let policy = no_attestation_dev_policy_v1(CHAIN_ID, prepared.spec.genesis_hash()).unwrap();
        let authority = TeeBootstrapAuthorityV2 {
            policy: policy.clone(),
            committee_snapshot_hash: snapshot.committee_set_hash_v2(0),
            committee_snapshot_block: 1,
            key_epoch: 0,
            network_encryption_epoch: 0,
            dkg_transcript_hash: B256::ZERO,
            network_encryption_public_key: prepared
                .leader
                .network_encryption_public()
                .unwrap()
                .into(),
            network_encryption_group_public_key: Bytes::from(vec![0x24; 96]),
        };
        let policy_hash = policy.policy_hash().unwrap();
        let submissions = VALIDATOR_SEEDS
            .iter()
            .enumerate()
            .map(|(index, &seed)| {
                let signer = OutbeEvmSigner::from_secret_bytes([seed; 32]).unwrap();
                let tee = if index == 0 {
                    Arc::clone(&prepared.leader)
                } else {
                    resident_context(seed, prepared.spec.genesis_hash())
                };
                submission(&tee, &signer, policy_hash, seed)
            })
            .collect();
        let mut payload =
            outbe_primitives::tee_bootstrap_v2::assembly::assemble_unsigned(authority, submissions)
                .unwrap();
        let hash = payload.signing_hash().unwrap();
        for seed in VALIDATOR_SEEDS {
            let signer = OutbeEvmSigner::from_secret_bytes([seed; 32]).unwrap();
            let signature = payload
                .committee_signatures
                .iter_mut()
                .find(|sig| sig.validator == signer.address())
                .unwrap();
            signature.signature = signer.sign_hash(&hash).unwrap();
        }
        Self { extra, payload }
    }
}

fn submission(
    tee: &outbe_tee::in_process::InProcessTeeContext,
    signer: &OutbeEvmSigner,
    policy_hash: B256,
    seed: u8,
) -> TeeBootstrapParticipantSubmissionV2 {
    let manifest = tee.manifest();
    let intent: RegistrationIntentV1 = RegistrationIntentFixture {
        operation: AttestationOperationV1::RegisterEnclave,
        policy_hash,
        binding_id: B256::repeat_byte(seed),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: TEST_BLOCK_TIMESTAMP_BASE + 3601,
    }
    .for_manifest(manifest)
    .unwrap();
    let validator_binding = ValidatorNodeBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        validator: signer.address().into_array(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
    };
    let binding_hash = validator_binding.binding_hash().unwrap();
    let intent_hash = intent.intent_hash().unwrap();
    let node = parity_node_signer(seed);
    let enclave_signature = tee.sign_domain_attestation(intent_hash.as_slice());
    TeeBootstrapParticipantSubmissionV2 {
        evidence: AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
            intent,
            transition_key_ready_proof: None,
            dev_attestation_public: tee.attestation_public(),
            dev_signature: enclave_signature,
        }),
        validator_binding,
        validator_signature: signer.sign_hash(&binding_hash).unwrap(),
        node_binding_signature: sign_node_host_hash_for_test(&node, binding_hash),
        node_signature: sign_node_host_hash_for_test(&node, intent_hash),
        enclave_signature,
    }
}

pub(super) fn parity_node_signer(seed: u8) -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap()
}
