#![cfg(feature = "tee-attestation-v1")]

use alloy_consensus::{SignableTransaction as _, Transaction as _};
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::{Address, Bytes, Signature, B256};
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_primitives::tee_bootstrap_v2::{assembly, codec};
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::{
    consensus::{DkgBoundaryArtifact, ReshareResult},
    system_tx::{
        build_unsigned_system_tx, protocol_block_gas_limit, split_system_layout,
        system_tx_visible_gas_limit, validate_system_tx_set_for_publication, BlockSystemFlags,
        BlockSystemPlan, SystemTx, CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
        CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX, CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
        STEADY_BLOCK_GAS_LIMIT, SYSTEM_CALLDATA_FRAMING_BYTES, SYSTEM_TX_INPUT_VERSION,
    },
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, CodecError,
        DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1, NodeIdV1,
        PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1, ResourceScheduleV1,
        SystemGasScheduleV1, TeeMeasurementRuleV1, TeePolicyV1, TeeRegistryGasScheduleV1,
        ValidatorNodeBindingV1, MAX_ATTESTATION_EVIDENCE_BYTES, MAX_TEE_BOOTSTRAP_BYTES,
    },
    tee_bootstrap_v2::{
        TeeBootstrapAuthorityV2, TeeBootstrapCommitteeSignatureV2,
        TeeBootstrapParticipantEvidenceV2, TeeBootstrapParticipantSubmissionV2,
        TeeBootstrapParticipantV2, TeeBootstrapV2,
    },
    test_utils::TEST_BLOCK_ORDER,
};
use reth_ethereum::TransactionSigned;

fn policy() -> TeePolicyV1 {
    TeePolicyV1 {
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
        maximum_lease: 7 * SECONDS_PER_DAY,
        collateral_margin: 3_600,
        resource_schedule_hash: ResourceScheduleV1::normative()
            .unwrap()
            .schedule_hash()
            .unwrap(),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: B256::repeat_byte(0x81),
            mrsigner: B256::repeat_byte(0x82),
            isv_prod_id: 1,
            minimum_isv_svn: 2,
            admit_from_height: 1,
            admit_until_height_exclusive: 1_000,
        }],
    }
}

fn intent(address: u8, policy_hash: B256) -> RegistrationIntentV1 {
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[address; 32]).into()).unwrap();
    RegistrationIntentV1 {
        chain_id: [0x10; 32],
        genesis_hash: B256::repeat_byte(0x11),
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash,
        node_id: NodeIdV1 {
            reth_p2p_public: node_signer
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        enclave_id: B256::repeat_byte(address.wrapping_add(2)),
        binding_id: B256::repeat_byte(address.wrapping_add(3)),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 7_200,
        recipient_x25519: [address.wrapping_add(4); 32],
        attestation_ed25519: [address.wrapping_add(5); 32],
        noise_responder_x25519: [address.wrapping_add(6); 32],
        node_host_authorization_hash: B256::repeat_byte(address.wrapping_add(7)),
    }
}

fn validator_binding_authorization(
    seed: u8,
    participant_intent: &RegistrationIntentV1,
) -> (ValidatorNodeBindingV1, [u8; 65], [u8; 65]) {
    let validator_signer =
        k256::ecdsa::SigningKey::from_bytes((&[seed.wrapping_add(0x40); 32]).into()).unwrap();
    let node_signer = k256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).unwrap();
    let validator_public = validator_signer.verifying_key().to_encoded_point(false);
    let validator_hash = alloy_primitives::keccak256(&validator_public.as_bytes()[1..]);
    let binding = ValidatorNodeBindingV1 {
        chain_id: participant_intent.chain_id,
        genesis_hash: participant_intent.genesis_hash,
        validator: Address::from_slice(&validator_hash[12..]).into_array(),
        node_id_hash: participant_intent.node_id.node_id_hash().unwrap(),
    };
    let binding_hash = binding.binding_hash().unwrap();
    let sign = |key: &k256::ecdsa::SigningKey| {
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
            key.sign_prehash(binding_hash.as_slice()).unwrap();
        let mut encoded = [0_u8; 65];
        encoded[..64].copy_from_slice(signature.to_bytes().as_slice());
        encoded[64] = recovery.to_byte();
        encoded
    };
    let validator_signature = sign(&validator_signer);
    let node_binding_signature = sign(&node_signer);
    (binding, validator_signature, node_binding_signature)
}

fn participant(seed: u8, policy_hash: B256) -> TeeBootstrapParticipantV2 {
    let participant_intent = intent(seed, policy_hash);
    let (validator_binding, validator_signature, node_binding_signature) =
        validator_binding_authorization(seed, &participant_intent);
    TeeBootstrapParticipantV2 {
        intent: participant_intent,
        validator_binding,
        validator_signature,
        node_binding_signature,
        evidence: TeeBootstrapParticipantEvidenceV2::Dcap {
            quote: vec![seed; 64],
            collateral_component_indices: [0, 1, 2, 3, 4, 5, 6, 7],
        },
        node_signature: [seed; 65],
        enclave_signature: [seed; 64],
    }
}

fn fixture() -> TeeBootstrapV2 {
    let policy = policy();
    let policy_hash = policy.policy_hash().unwrap();
    let collateral_pool = (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind],
        })
        .collect();
    let mut participants = [0x31_u8, 0x32]
        .into_iter()
        .map(|seed| participant(seed, policy_hash))
        .collect::<Vec<_>>();
    participants.sort_by_key(|participant| Address::from(participant.validator_binding.validator));
    let committee_signatures = participants
        .iter()
        .map(|participant| TeeBootstrapCommitteeSignatureV2 {
            validator: Address::from(participant.validator_binding.validator),
            signature: [participant.intent.recipient_x25519[0]; 65],
        })
        .collect();

    TeeBootstrapV2 {
        authority: TeeBootstrapAuthorityV2 {
            policy,
            committee_snapshot_hash: B256::repeat_byte(0x21),
            committee_snapshot_block: 1,
            key_epoch: 1,
            tribute_offer_epoch: 1,
            dkg_transcript_hash: B256::repeat_byte(0x22),
            tribute_offer_public_key: B256::repeat_byte(0x23),
            tribute_offer_group_public_key: Bytes::from(vec![0x24; 96]),
        },
        collateral_pool,
        participants,
        committee_signatures,
    }
}

fn fixture_with_participant_count(count: u8) -> TeeBootstrapV2 {
    let mut payload = fixture();
    let policy_hash = payload.authority.policy.policy_hash().unwrap();
    payload.participants = (1..=count)
        .map(|seed| participant(seed, policy_hash))
        .collect();
    payload
        .participants
        .sort_by_key(|participant| Address::from(participant.validator_binding.validator));
    payload.committee_signatures = payload
        .participants
        .iter()
        .map(|participant| TeeBootstrapCommitteeSignatureV2 {
            validator: Address::from(participant.validator_binding.validator),
            signature: [participant.intent.recipient_x25519[0]; 65],
        })
        .collect();
    payload
}

fn production_shaped_payload(count: u8) -> TeeBootstrapV2 {
    let mut payload = fixture_with_participant_count(count);
    payload.authority.policy.measurement_rules = (0_u8..4)
        .map(|index| TeeMeasurementRuleV1 {
            mrenclave: B256::repeat_byte(0x81_u8.wrapping_add(index)),
            mrsigner: B256::repeat_byte(0x82_u8.wrapping_add(index)),
            isv_prod_id: u16::from(index) + 1,
            minimum_isv_svn: u16::from(index) + 2,
            admit_from_height: 1,
            admit_until_height_exclusive: 1_000,
        })
        .collect();
    let policy_hash = payload.authority.policy.policy_hash().unwrap();
    payload.collateral_pool = (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind; 4 * 1024],
        })
        .collect();
    for (index, participant) in payload.participants.iter_mut().enumerate() {
        let seed = u8::try_from(index + 1).unwrap();
        participant.intent.policy_hash = policy_hash;
        participant.evidence = TeeBootstrapParticipantEvidenceV2::Dcap {
            quote: vec![seed; 4 * 1024],
            collateral_component_indices: [0, 1, 2, 3, 4, 5, 6, 7],
        };
    }
    payload
}

fn authority(payload: &TeeBootstrapV2) -> TeeBootstrapAuthorityV2 {
    payload.authority.clone()
}

fn submissions(payload: &TeeBootstrapV2) -> Vec<TeeBootstrapParticipantSubmissionV2> {
    payload
        .participants
        .iter()
        .enumerate()
        .map(|(index, participant)| TeeBootstrapParticipantSubmissionV2 {
            evidence: payload.logical_evidence(index).unwrap(),
            validator_binding: participant.validator_binding.clone(),
            validator_signature: participant.validator_signature,
            node_binding_signature: participant.node_binding_signature,
            node_signature: participant.node_signature,
            enclave_signature: participant.enclave_signature,
        })
        .collect()
}

#[test]
fn roundtrip_reconstructs_each_logical_evidence_from_deduplicated_collateral() {
    let payload = fixture();
    let encoded = codec::encode_canonical(&payload).unwrap();
    let decoded = codec::decode_canonical(&encoded).unwrap();
    assert_eq!(decoded, payload);
    assert_eq!(decoded.collateral_pool.len(), 8);
    assert_eq!(decoded.participants.len(), 2);

    let first = decoded.logical_evidence(0).unwrap();
    let second = decoded.logical_evidence(1).unwrap();
    let (AttestationEvidenceV1::Dcap(first), AttestationEvidenceV1::Dcap(second)) = (first, second)
    else {
        panic!("OST3 must reconstruct DCAP evidence");
    };
    assert_eq!(first.components, decoded.collateral_pool);
    assert_eq!(second.components, decoded.collateral_pool);
    assert_ne!(first.quote, second.quote);
}

#[test]
fn signing_hash_binds_the_complete_body_but_not_committee_signature_bytes() {
    let payload = fixture();
    let signing_hash = payload.signing_hash().unwrap();

    let mut signature_changed = payload.clone();
    signature_changed.committee_signatures[0].signature[0] ^= 1;
    assert_eq!(signature_changed.signing_hash().unwrap(), signing_hash);

    let mut evidence_changed = payload;
    let TeeBootstrapParticipantEvidenceV2::Dcap { quote, .. } =
        &mut evidence_changed.participants[0].evidence
    else {
        panic!("fixture must carry DCAP evidence");
    };
    quote[0] ^= 1;
    assert_ne!(evidence_changed.signing_hash().unwrap(), signing_hash);
}

#[test]
fn gas_charges_each_logical_evidence_despite_collateral_deduplication() {
    let payload = fixture();
    let encoded = codec::encode_canonical(&payload).unwrap();
    let full_calldata_len = encoded.len() + SYSTEM_CALLDATA_FRAMING_BYTES;
    let registry = TeeRegistryGasScheduleV1::normative();
    let evidence_lengths = (0..payload.participants.len())
        .map(|index| {
            payload
                .logical_evidence(index)
                .unwrap()
                .encode_canonical()
                .unwrap()
                .len()
        })
        .collect::<Vec<_>>();
    let expected = 300_000
        + u64::try_from(full_calldata_len).unwrap()
        + 100_000 * 2
        + evidence_lengths
            .iter()
            .map(|length| registry.qvl_dcap(*length, 1).unwrap())
            .sum::<u64>()
        + 15_000 * 16
        + 10_000 * 2;

    assert_eq!(
        payload
            .bootstrap_work_gas(&SystemGasScheduleV1::normative(), &registry)
            .unwrap(),
        expected
    );
}

#[test]
fn consensus_system_calldata_roundtrips_the_canonical_bootstrap() {
    let payload = fixture();
    let calldata = SystemTx::Consensus {
        step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
        payload: codec::encode_canonical(&payload).unwrap(),
    }
    .encode()
    .unwrap();
    assert_eq!(&calldata[..4], b"OSCN");
    assert_eq!(calldata[4], SYSTEM_TX_INPUT_VERSION);
    assert_eq!(calldata[5], CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX);
    assert_eq!(
        &calldata[SYSTEM_CALLDATA_FRAMING_BYTES..],
        codec::encode_canonical(&payload).unwrap().as_ref()
    );
    assert_eq!(
        SystemTx::decode(&calldata).unwrap(),
        SystemTx::Consensus {
            step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
            payload: codec::encode_canonical(&payload).unwrap()
        }
    );
}

fn block1_system_inputs(payload: TeeBootstrapV2, artifact: DkgBoundaryArtifact) -> Vec<SystemTx> {
    vec![
        SystemTx::Consensus {
            step_index: CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
            payload: outbe_primitives::reshare_artifact::encode_boundary_artifact(&artifact)
                .unwrap(),
        },
        SystemTx::Consensus {
            step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
            payload: codec::encode_canonical(&payload).unwrap(),
        },
        SystemTx::Consensus {
            step_index: CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
            payload: Bytes::new(),
        },
        SystemTx::BeginBlock { step_index: 0 },
        SystemTx::BeginBlock { step_index: 1 },
        SystemTx::BeginBlock { step_index: 2 },
        SystemTx::BeginBlock { step_index: 3 },
        SystemTx::BeginBlock { step_index: 4 },
        SystemTx::BeginBlock { step_index: 5 },
        SystemTx::BeginBlock { step_index: 6 },
        SystemTx::BeginBlock { step_index: 7 },
        SystemTx::BeginBlock { step_index: 8 },
    ]
}

fn sign_block1(inputs: Vec<SystemTx>) -> Vec<TransactionSigned> {
    inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| {
            build_unsigned_system_tx(
                u8::try_from(ordinal).unwrap(),
                1,
                1,
                input.encode().unwrap(),
            )
            .unwrap()
            .into_signed(Signature::test_signature())
            .into()
        })
        .collect()
}

fn assert_block1_zero_gas_layout(transactions: &[TransactionSigned]) {
    assert_eq!(transactions.len(), 12);
    for transaction in transactions {
        assert_eq!(transaction.gas_limit(), 0);
        assert_eq!(system_tx_visible_gas_limit(transaction.input()).unwrap(), 0);
    }
    let layout = split_system_layout(transactions).expect("block-1 layout splits");
    validate_system_tx_set_for_publication(
        &TEST_BLOCK_ORDER,
        &layout,
        1,
        BlockSystemFlags {
            has_boundary_outcome: true,
            has_publication: false,
        },
    )
    .expect("block-1 bootstrap layout validates");
    assert_eq!(
        layout.prefix_steps().unwrap(),
        BlockSystemPlan::for_block(
            1,
            BlockSystemFlags {
                has_boundary_outcome: true,
                has_publication: false
            },
            &TEST_BLOCK_ORDER
        )
        .expected_prefix_steps()
    );
    assert!(layout.end.is_empty());
    assert!(
        BlockSystemPlan::for_block(1, BlockSystemFlags::default(), &TEST_BLOCK_ORDER)
            .expected_end_block_steps()
            .is_empty()
    );
}

#[test]
fn block1_envelopes_carry_zero_gas() {
    let payload = fixture();
    let artifact = outbe_primitives::test_utils::sample_system_tx_boundary(2);
    let transactions = sign_block1(block1_system_inputs(payload, artifact));
    assert_block1_zero_gas_layout(&transactions);
}

#[test]
fn block_gas_limit_is_uniform_steady_32m_across_all_heights() {
    assert_eq!(STEADY_BLOCK_GAS_LIMIT, 32_000_000);
    assert_eq!(protocol_block_gas_limit(0), STEADY_BLOCK_GAS_LIMIT);
    assert_eq!(protocol_block_gas_limit(1), STEADY_BLOCK_GAS_LIMIT);
    assert_eq!(protocol_block_gas_limit(2), STEADY_BLOCK_GAS_LIMIT);
}

#[test]
fn canonical_bootstrap_rejects_noncanonical_pool_and_authority_shapes() {
    let mut reversed = fixture();
    reversed.collateral_pool.swap(0, 1);
    assert!(matches!(
        codec::encode_canonical(&reversed),
        Err(CodecError::NonCanonical(
            "TEE bootstrap collateral pool is not strictly sorted and unique"
        ))
    ));

    let mut unused = fixture();
    unused.collateral_pool.push(DcapCollateralComponentV1 {
        kind: DcapCollateralKind::QeIdentityIssuerChain,
        bytes: vec![8, 1],
    });
    assert!(matches!(
        codec::encode_canonical(&unused),
        Err(CodecError::NonCanonical(
            "TEE bootstrap collateral pool contains an unused component"
        ))
    ));

    let mut missing_signature = fixture();
    missing_signature.committee_signatures.pop();
    assert!(matches!(
        codec::encode_canonical(&missing_signature),
        Err(CodecError::NonCanonical(
            "TEE bootstrap signatures do not cover every participant"
        ))
    ));

    let mut mismatched_signature = fixture();
    mismatched_signature.committee_signatures[0].validator = Address::repeat_byte(0x30);
    assert!(matches!(
        codec::encode_canonical(&mismatched_signature),
        Err(CodecError::NonCanonical(
            "TEE bootstrap committee signatures do not match participants"
        ))
    ));
}

#[test]
fn decoder_rejects_full_calldata_cap_plus_one_before_parsing() {
    let oversized_body = vec![0_u8; MAX_TEE_BOOTSTRAP_BYTES - SYSTEM_CALLDATA_FRAMING_BYTES + 1];
    assert!(matches!(
        codec::decode_canonical(&oversized_body),
        Err(CodecError::LimitExceeded {
            field: "TeeBootstrapV2 full calldata",
            limit: MAX_TEE_BOOTSTRAP_BYTES,
            actual,
        }) if actual == MAX_TEE_BOOTSTRAP_BYTES + 1
    ));
}

#[test]
fn seven_validator_bootstrap_fits_block1_layout() {
    let payload = production_shaped_payload(7);
    let logical_evidence_len = payload
        .logical_evidence(0)
        .unwrap()
        .encode_canonical()
        .unwrap()
        .len();
    assert!(logical_evidence_len <= MAX_ATTESTATION_EVIDENCE_BYTES);
    assert!(logical_evidence_len > 32 * 1024);

    let artifact = DkgBoundaryArtifact {
        epoch: 0,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 1,
        target_set_hash: B256::repeat_byte(0x31),
        vrf_material_version: 0,
        vrf_group_public_key: B256::repeat_byte(0x32),
        vrf_group_public_key_bytes: Bytes::from(vec![0x33; 96]),
        committee_set_hash: payload.authority.committee_snapshot_hash,
        is_validator_set_change: true,
        outcome: Bytes::new(),
        is_full_dkg: true,
        reshare: ReshareResult {
            active_set_hash: B256::repeat_byte(0x34),
            new_active_set: payload
                .participants
                .iter()
                .map(|participant| Address::from(participant.validator_binding.validator))
                .collect(),
        },
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
    };
    let inputs = block1_system_inputs(payload, artifact);
    let bootstrap_len = inputs[1].encode().unwrap().len();
    assert!(bootstrap_len <= MAX_TEE_BOOTSTRAP_BYTES);
    let transactions = sign_block1(inputs);
    assert_block1_zero_gas_layout(&transactions);

    let exact_transactions_rlp_len = transactions
        .iter()
        .map(|transaction| {
            let mut encoded = Vec::new();
            transaction.encode_2718(&mut encoded);
            encoded.len()
        })
        .sum::<usize>();

    assert!(
        exact_transactions_rlp_len < outbe_primitives::consensus::OUTBE_MAX_BLOCK_SIZE,
        "the block-1 transaction encodings must fit the P2P block cap"
    );
}

#[test]
fn assembly_reuses_existing_authority_and_deduplicates_complete_submissions() {
    let mut expected = fixture();
    expected.authority.key_epoch = 0;
    expected.authority.tribute_offer_epoch = 0;
    expected.authority.dkg_transcript_hash = B256::ZERO;
    let submissions = expected
        .participants
        .iter()
        .enumerate()
        .rev()
        .map(|(index, participant)| TeeBootstrapParticipantSubmissionV2 {
            evidence: expected.logical_evidence(index).unwrap(),
            validator_binding: participant.validator_binding.clone(),
            validator_signature: participant.validator_signature,
            node_binding_signature: participant.node_binding_signature,
            node_signature: participant.node_signature,
            enclave_signature: participant.enclave_signature,
        })
        .collect();
    let assembled = assembly::assemble_unsigned(authority(&expected), submissions).unwrap();

    assert_eq!(assembled.collateral_pool, expected.collateral_pool);
    assert_eq!(assembled.participants, expected.participants);
    assert!(assembled
        .committee_signatures
        .iter()
        .all(|signature| signature.signature == [0; 65]));
    assert_eq!(
        assembled.signing_hash().unwrap(),
        expected.signing_hash().unwrap()
    );
}

#[test]
fn assembly_rejects_aggregate_calldata_over_cap_before_signing() {
    let template = fixture_with_participant_count(2);
    let policy_hash = template.authority.policy.policy_hash().unwrap();
    let submissions = (1_u8..=2)
        .map(|address| {
            let participant_intent = intent(address, policy_hash);
            let (validator_binding, validator_signature, node_binding_signature) =
                validator_binding_authorization(address, &participant_intent);
            let components = (1_u8..=8)
                .map(|kind| DcapCollateralComponentV1 {
                    kind: DcapCollateralKind::try_from(kind).unwrap(),
                    bytes: vec![address.wrapping_mul(16).wrapping_add(kind); 90 * 1024],
                })
                .collect();
            TeeBootstrapParticipantSubmissionV2 {
                evidence: AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                    intent: participant_intent.clone(),
                    quote: vec![address; 64],
                    components,
                    transition_key_ready_proof: None,
                }),
                validator_binding,
                validator_signature,
                node_binding_signature,
                node_signature: [address; 65],
                enclave_signature: [address; 64],
            }
        })
        .collect();

    let error = assembly::assemble_unsigned(authority(&template), submissions)
        .expect_err("aggregate calldata over the OST3 cap must fail before signing");
    assert!(matches!(
        error,
        CodecError::LimitExceeded {
            field: "TeeBootstrapV2 full calldata",
            limit: MAX_TEE_BOOTSTRAP_BYTES,
            actual,
        } if actual > MAX_TEE_BOOTSTRAP_BYTES
    ));
}

#[test]
fn assembly_rejects_logical_work_over_bootstrap_gas_cap() {
    let payload = production_shaped_payload(9);
    let error = assembly::assemble_unsigned(authority(&payload), submissions(&payload))
        .expect_err("logical QVL work over 32M must fail before signing");
    assert!(matches!(
        error,
        CodecError::LimitExceeded {
            field: "TeeBootstrapV2 worst-case visible gas",
            limit,
            actual,
        } if limit == usize::try_from(STEADY_BLOCK_GAS_LIMIT).unwrap() && actual > limit
    ));
}

#[test]
fn canonical_bootstrap_protocol_fingerprint_is_stable() {
    let payload = fixture();
    let encoded = codec::encode_canonical(&payload).unwrap();
    let calldata = SystemTx::Consensus {
        step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
        payload: codec::encode_canonical(&payload).unwrap(),
    }
    .encode()
    .unwrap();
    let gas = payload
        .bootstrap_work_gas(
            &SystemGasScheduleV1::normative(),
            &TeeRegistryGasScheduleV1::normative(),
        )
        .unwrap();
    assert_eq!(encoded.len(), 2443);
    assert_eq!(
        alloy_primitives::keccak256(&encoded),
        alloy_primitives::b256!(
            "0x5a2280be6ef8b004c524fcd76ce5498b32c2591c74c416476bfbdbad1929fab2"
        ),
    );
    assert_eq!(
        payload.signing_hash().unwrap(),
        alloy_primitives::b256!(
            "0x9aed9070b21466562e37910bf1d82c7e0d624c83e4137bc35216925d1086461b"
        ),
    );
    assert_eq!(calldata.len(), 2449);
    assert_eq!(
        alloy_primitives::keccak256(&calldata),
        alloy_primitives::b256!(
            "0x4873c28c49740cc7ddaedb0db62f844eeec27509758edabc0000107796297615"
        ),
    );
    assert_eq!(gas, 7308437);
    assert_eq!(&encoded[..4], b"TTB2");
}
