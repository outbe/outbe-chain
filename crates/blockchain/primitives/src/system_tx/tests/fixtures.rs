use super::*;

pub(super) const CHAIN_ID: u64 = 2026;

pub(super) fn sample_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: 41,
        finalized_block_hash: B256::repeat_byte(0x41),
        finalized_epoch: 7,
        finalized_view: 42,
        parent_view: 41,
        ordered_committee: vec![address!("0x1111111111111111111111111111111111111111")],
        signer_bitmap: vec![1],
        proof: Bytes::from_static(b"cert"),
        committee_set_hash: B256::repeat_byte(0x77),
        vrf_material_version: 3,
        vrf_group_public_key_hash: B256::repeat_byte(0x88),
        proof_kind: crate::consensus_metadata::ParentParticipationProof::Finalization,
        // V2 contract requires `missed_proposers` to be empty;
        // this test fixture keeps it empty to stay consistent with the
        // verifier rule.
        missed_proposers: Vec::new(),
    }
}

fn sample_boundary() -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: 8,
        dkg_cycle: 2,
        freeze_height: 40,
        planned_activation_height: 42,
        target_set_hash: B256::repeat_byte(0x33),
        vrf_material_version: 3,
        vrf_group_public_key: B256::repeat_byte(0x44),
        vrf_group_public_key_bytes: Bytes::from_static(&[0x44u8; 96]),
        committee_set_hash: B256::repeat_byte(0x66),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"boundary"),
        is_full_dkg: false,
        tee_recipient_pubkeys: Vec::new(),
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![address!("0x3333333333333333333333333333333333333333")],
            active_set_hash: B256::repeat_byte(0x55),
        },
    }
}

fn sample_tee_bootstrap() -> crate::tee_bootstrap_v2::TeeBootstrapV2 {
    use crate::{
        tee_attestation_v1::{
            AttestationMode, AttestationOperationV1, DcapCollateralComponentV1, DcapCollateralKind,
            NodeIdV1, PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1,
            ResourceScheduleV1, TeeMeasurementRuleV1, TeePolicyV1, ValidatorNodeBindingV1,
        },
        tee_bootstrap_v2::{
            TeeBootstrapCommitteeSignatureV2, TeeBootstrapParticipantEvidenceV2,
            TeeBootstrapParticipantV2, TeeBootstrapV2,
        },
    };

    use k256::ecdsa::signature::hazmat::PrehashSigner as _;
    let validator_signer =
        crate::signer::OutbeEvmSigner::from_secret_bytes([0x22; 32]).expect("validator signer");
    let validator = validator_signer.address();
    let node_signer =
        k256::ecdsa::SigningKey::from_bytes((&[0x23; 32]).into()).expect("NodeHost signer");
    let reth_p2p_public = node_signer
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .expect("SEC1-33 NodeHost public key");
    let policy = TeePolicyV1 {
        policy_version: 1,
        chain_id: [0x10; 32],
        genesis_hash: B256::repeat_byte(0x11),
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x72),
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ],
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: ResourceScheduleV1::normative()
            .expect("normative resource schedule")
            .schedule_hash()
            .expect("resource schedule hashes"),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: B256::repeat_byte(0x81),
            mrsigner: B256::repeat_byte(0x82),
            isv_prod_id: 1,
            minimum_isv_svn: 2,
            admit_from_height: 1,
            admit_until_height_exclusive: 1_000,
        }],
    };
    let policy_hash = policy.policy_hash().expect("policy hashes");
    let intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: B256::repeat_byte(0x11),
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash,
        node_id: NodeIdV1 { reth_p2p_public },
        enclave_id: B256::repeat_byte(0x32),
        binding_id: B256::repeat_byte(0x33),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: [0x34; 32],
        attestation_ed25519: [0x35; 32],
        noise_responder_x25519: [0x36; 32],
        node_host_authorization_hash: B256::repeat_byte(0x37),
    };

    let validator_binding = ValidatorNodeBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        validator: validator.into_array(),
        node_id_hash: intent.node_id.node_id_hash().expect("NodeHost hash"),
    };
    let binding_hash = validator_binding.binding_hash().expect("binding hash");
    let validator_signature = validator_signer
        .sign_hash(&binding_hash)
        .expect("validator binding signature");
    let (node_signature_body, node_recovery) = node_signer
        .sign_prehash(binding_hash.as_slice())
        .expect("NodeHost binding signature");
    let mut node_binding_signature = [0_u8; 65];
    node_binding_signature[..64].copy_from_slice(node_signature_body.to_bytes().as_slice());
    node_binding_signature[64] = node_recovery.to_byte();

    TeeBootstrapV2 {
        policy,
        committee_snapshot_hash: B256::repeat_byte(0xB2),
        committee_snapshot_block: 1,
        key_epoch: 1,
        tribute_offer_epoch: 1,
        dkg_transcript_hash: B256::repeat_byte(0xB3),
        tribute_offer_public_key: B256::repeat_byte(0xB4),
        tribute_offer_group_public_key: Bytes::from(vec![0xB5; 96]),
        collateral_pool: (1_u8..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).expect("known collateral kind"),
                bytes: vec![kind],
            })
            .collect(),
        participants: vec![TeeBootstrapParticipantV2 {
            intent,
            validator_binding,
            validator_signature,
            node_binding_signature,
            evidence: TeeBootstrapParticipantEvidenceV2::Dcap {
                quote: vec![0x41; 64],
                collateral_component_indices: [0, 1, 2, 3, 4, 5, 6, 7],
            },
            node_signature: [0x42; 65],
            enclave_signature: [0x43; 64],
        }],
        committee_signatures: vec![TeeBootstrapCommitteeSignatureV2 {
            validator,
            signature: [0x44; 65],
        }],
    }
}

pub(super) fn input_for(kind: SystemTxKind) -> SystemTxInputV2 {
    match kind {
        SystemTxKind::CertifiedParentAccounting => SystemTxInputV2::CertifiedParentAccounting {
            metadata: sample_metadata(),
        },
        SystemTxKind::LateFinalizeCredits => SystemTxInputV2::LateFinalizeCredits {
            artifact: LateFinalizeCreditsArtifact::default(),
        },
        SystemTxKind::OcompLifecycleBegin => SystemTxInputV2::OcompLifecycleBegin,
        SystemTxKind::CycleTick => SystemTxInputV2::CycleTick,
        SystemTxKind::RewardsGemDelivery => SystemTxInputV2::RewardsGemDelivery,
        SystemTxKind::BoundaryOutcome => SystemTxInputV2::BoundaryOutcome {
            artifact: sample_boundary(),
        },
        SystemTxKind::TeeBootstrap => SystemTxInputV2::TeeBootstrap {
            payload: sample_tee_bootstrap(),
        },
        SystemTxKind::OracleSlashWindow => SystemTxInputV2::OracleSlashWindow,
        SystemTxKind::HookEvents => SystemTxInputV2::HookEvents { hyperlane: None },
        SystemTxKind::OcompTerminalRequest => SystemTxInputV2::OcompTerminalRequest,
    }
}
