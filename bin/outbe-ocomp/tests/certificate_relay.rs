use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use alloy_consensus::Transaction as _;
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{Address, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use outbe_e2e_harness::ocomp_finality_fixture::finalized_intent_proof_fixture;
use outbe_node::ocomp::finality::{
    PublicAccountProofV1, PublicBlockViewV1, PublicExactBlockProofSourceV1,
    PublicFinalizationBytesV1, PublicFinalizedIntentProofBuilderV1,
};
use outbe_ocomp::relay::{
    ActivationPublisherV1, CandidateRelayV1, NormalActivationSubmitterV1,
    PublicExactBlockRpcClientV1, RelayAcceptOutcomeV1, RelayHeightSourceV1, RelayHttpServerV1,
    VerifiedRelayJobV1,
};
use outbe_ocomp_protocol::{
    abi::ACTIVATE_LYSIS_SELECTOR,
    activation::{encode_activate_lysis_calldata, CandidateAnnouncementV1, PoCActivationV1},
    certificate::{build_execution_certificate, OrderedSignatureV1},
    codec::CodecLimits,
    committee::{
        OcompCommitteeSnapshotV1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        OcompMemberV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::{BoundedBytes, ProofBytes},
    hash::hash_framed,
    intent::{
        ActivationPreconditionsV1, CertifiedParentAccountingMetadataV2,
        ContributorTargetPreconditionV1, DayType, ExpectedFinalizedIntentBindingV1,
        FinalizedIntentProofV1, FrozenMetadosisValuesV1, JobIntentV1,
        MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
        ParentProofKind, TributeInputBindingV1,
    },
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    SchemaLimits,
};
use outbe_primitives::{addresses::METADOSIS_ADDRESS, signer::OutbeEvmSigner};
use reth_ethereum::{primitives::SignedTransaction as _, TransactionSigned};

const LIMITS: SchemaLimits = SchemaLimits {
    codec: CodecLimits::new(1_048_576, 4_096, 2_097_152),
    max_bounded_bytes: 262_144,
    max_proof_bytes: 262_144,
    max_opening_bytes: 262_144,
    max_collection_items: 4_096,
    max_action_items: 4_096,
    max_chunk_items: 4_096,
    max_unit_inputs: 64,
    max_result_chunk_bytes: 524_288,
    max_control_body_bytes: 262_144,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes((&[index + 1; 32]).into()).unwrap()
}

fn compressed_key(key: &SigningKey) -> [u8; 33] {
    key.verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap()
}

fn sign(key: &SigningKey, digest: B256) -> [u8; 64] {
    let signature: Signature = key.sign_prehash(digest.as_slice()).unwrap();
    signature.to_bytes().into()
}

fn registration(index: u8) -> OcompKeyRegistrationV1 {
    let key = signing_key(index);
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id: 42,
            genesis_hash: hash(40),
            fork_id: hash(1),
            protocol_bundle_hash: hash(41),
            validator_index: index,
            validator_identity_hash: hash(70 + index),
            ocomp_public_key_sec1: compressed_key(&key),
            key_epoch: 1,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
            valid_from_height: 1,
            valid_until_height_exclusive: 1_000,
        },
        proof_of_possession: [0; 64],
    };
    let digest = registration.proof_of_possession_digest(&LIMITS).unwrap();
    registration.proof_of_possession = sign(&key, digest);
    registration
}

fn committee() -> OcompCommitteeSnapshotV1 {
    OcompCommitteeSnapshotV1 {
        chain_id: 42,
        genesis_hash: hash(40),
        fork_id: hash(1),
        protocol_bundle_hash: hash(41),
        snapshot_epoch: 1,
        threshold: 3,
        ordered_members: (0..4)
            .map(|index| {
                let registration = registration(index);
                OcompMemberV1 {
                    validator_index: index,
                    validator_identity_hash: registration.core.validator_identity_hash,
                    ocomp_public_key_sec1: registration.core.ocomp_public_key_sec1,
                    key_epoch: registration.core.key_epoch,
                    allowed_purpose_bitmap: registration.core.allowed_purpose_bitmap,
                    valid_from_height: registration.core.valid_from_height,
                    valid_until_height_exclusive: registration.core.valid_until_height_exclusive,
                    proof_of_possession: registration.proof_of_possession,
                }
            })
            .collect(),
    }
}

fn preconditions() -> ActivationPreconditionsV1 {
    ActivationPreconditionsV1 {
        tribute: TributeInputBindingV1 {
            wwd: 7,
            source_generation: 3,
            collection_key: hash(30),
            sealed_collection_root: hash(31),
            exact_count: 1,
            exact_nominal_total: U256::ZERO,
        },
        nod: NodTargetPreconditionV1 {
            wwd: 7,
            target_generation: 5,
            namespace_root_before: hash(32),
            max_nod_count: 1,
        },
        contributors: ContributorTargetPreconditionV1 {
            series_id: 7,
            expected_series_version: 8,
            max_contributor_count: 1,
            max_eligible_nominal_total: U256::ZERO,
        },
        metadosis: MetadosisAttemptPreconditionV1 {
            wwd: 7,
            pending_nonce: 1,
            expected_status: MetadosisExpectedStatus::OffchainPending,
            state_version: 12,
        },
    }
}

fn intent(committee_hash: B256) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: 42,
        genesis_hash: hash(40),
        fork_id: hash(1),
        wwd: 7,
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash: hash(41),
        ce_sealed_root: hash(42),
        sealed_tribute_collection_key: hash(30),
        sealed_tribute_collection_root: hash(31),
        authenticated_day_count: 1,
        authenticated_day_nominal: U256::ZERO,
        pre_admission_envelope_hash: hash(43),
        source_availability_policy_id: hash(44),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::ZERO,
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_budget: U256::ZERO,
            auction_base: U256::ZERO,
            auction_entry_price: U256::ZERO,
            request_budget_split_receipt_hash: hash(113),
        },
        logical_evaluation_height: 100,
        logical_evaluation_time: 1_000,
        activation_preconditions: preconditions(),
        result_committee_snapshot_hash: committee_hash,
        custody_committee_epoch_hash: None,
        deadline_height: 110,
    }
}

fn finality_proof(intent: &JobIntentV1) -> FinalizedIntentProofV1 {
    FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(vec![1, 2]),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: 90,
            finalized_block_hash: hash(46),
            finalized_epoch: 2,
            finalized_view: 3,
            parent_view: 2,
            ordered_committee: vec![BoundedBytes(vec![1])],
            signer_bitmap: BoundedBytes(vec![1]),
            canonical_commonware_finalization_proof: ProofBytes(vec![2]),
            committee_set_hash: hash(47),
            vrf_material_version: 1,
            vrf_group_public_key_hash: hash(48),
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        },
        historical_committee_membership_proof: ProofBytes(vec![3]),
        canonical_job_intent: BoundedBytes(intent.encode_canonical(&LIMITS).unwrap()),
        intent_account_proof: ProofBytes(vec![4]),
        intent_storage_proof: ProofBytes(vec![5]),
    }
}

fn result() -> LysisResultV1 {
    let roots = ResultRootsV1 {
        nod_root: hash(50),
        bucket_root: hash(51),
        contributor_root: hash(52),
        output_manifest_root: hash(53),
    };
    let counts = ExactCountsV1 {
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 0,
        contributor_count: 0,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        gratis_supply: U256::ZERO,
        lysis_budget: U256::ZERO,
        auction_base: U256::ZERO,
        nod_gratis_consumed: U256::ZERO,
        unused_lysis: U256::ZERO,
        carry_over_credit: U256::ZERO,
        nod_cost_total: U256::ZERO,
    };
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: hash(54),
        plan_hash: hash(55),
        unit_artifact_root: hash(56),
        fidelity_fraction_root: hash(57),
        gratis_prefix_root: hash(58),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    LysisResultV1 {
        protocol_bundle_hash: hash(41),
        job_id: hash(59),
        attempt: 1,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: hash(61),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: 7,
            reason: CarryOverReason::UnusedLysis,
            amount: U256::ZERO,
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: 7,
            pending_nonce: 1,
            day_type: DayType::Green,
            tribute_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_budget: U256::ZERO,
            auction_base: U256::ZERO,
            nod_gratis_consumed: U256::ZERO,
            unused_lysis: U256::ZERO,
            carry_over_credit: U256::ZERO,
            status: CompletionStatus::Completed,
            logical_evaluation_height: 100,
            logical_evaluation_time: 1_000,
        },
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(&LIMITS).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    }
}

fn candidate(index: u8, result: &LysisResultV1) -> CandidateAnnouncementV1 {
    let result_digest = result
        .activation_payload(&LIMITS)
        .unwrap()
        .result_digest(&LIMITS)
        .unwrap();
    CandidateAnnouncementV1 {
        protocol_bundle_hash: result.protocol_bundle_hash,
        job_id: result.job_id,
        attempt: result.attempt,
        result: result.clone(),
        result_digest,
        validator_index: index,
        key_epoch: 1,
        signature_rs: sign(&signing_key(index), result_digest),
    }
}

fn recommit_result(result: &mut LysisResultV1) {
    result.arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &result
            .arithmetic_summary()
            .encode_canonical(&LIMITS)
            .unwrap(),
    )
    .unwrap();
}

// OCOMP-TEST-ID: OCM-CRT-001
#[test]
fn three_matching_candidates_build_one_canonical_verified_certificate() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let result = result();
    let candidates = [
        candidate(2, &result),
        candidate(0, &result),
        candidate(1, &result),
    ];

    let certificate = build_execution_certificate(
        &candidates,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS,
    )
    .unwrap();

    assert_eq!(certificate.signer_bitmap, 0b0111);
    assert_eq!(
        certificate
            .ordered_signatures
            .iter()
            .map(|signature| signature.validator_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    certificate.verify(&committee, 100, &LIMITS).unwrap();
}

#[test]
fn every_three_of_four_subset_is_valid_and_four_choose_the_lowest_three() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let result = result();
    let all = (0..4)
        .map(|index| candidate(index, &result))
        .collect::<Vec<_>>();

    for omitted in 0..4 {
        let subset = all
            .iter()
            .filter(|candidate| candidate.validator_index != omitted)
            .cloned()
            .collect::<Vec<_>>();
        let certificate =
            build_execution_certificate(&subset, &intent, result.job_id, &committee, 100, &LIMITS)
                .unwrap();
        assert_eq!(
            certificate
                .ordered_signatures
                .iter()
                .map(|signature| signature.validator_index)
                .collect::<Vec<_>>(),
            (0..4).filter(|index| *index != omitted).collect::<Vec<_>>()
        );
    }

    let certificate =
        build_execution_certificate(&all, &intent, result.job_id, &committee, 100, &LIMITS)
            .unwrap();
    assert_eq!(certificate.signer_bitmap, 0b0111);
    assert_eq!(
        certificate
            .ordered_signatures
            .iter()
            .map(|signature| signature.validator_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn certificate_builder_rejects_non_quorum_and_non_matching_candidates() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let result = result();
    let valid = (0..3)
        .map(|index| candidate(index, &result))
        .collect::<Vec<_>>();

    assert!(build_execution_certificate(
        &valid[..2],
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut duplicate = valid.clone();
    duplicate[2] = duplicate[1].clone();
    assert!(build_execution_certificate(
        &duplicate,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut wrong_epoch = valid.clone();
    wrong_epoch[2].key_epoch = 2;
    assert!(build_execution_certificate(
        &wrong_epoch,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut bad_signature = valid.clone();
    bad_signature[2].signature_rs[0] ^= 1;
    assert!(build_execution_certificate(
        &bad_signature,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut high_s = valid.clone();
    high_s[2].signature_rs[32..].copy_from_slice(&[
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x40,
    ]);
    assert!(
        build_execution_certificate(&high_s, &intent, result.job_id, &committee, 100, &LIMITS)
            .is_err()
    );

    let mut different_result = result.clone();
    different_result.roots.nod_root = hash(62);
    recommit_result(&mut different_result);
    let mixed = [
        valid[0].clone(),
        valid[1].clone(),
        candidate(2, &different_result),
    ];
    assert!(
        build_execution_certificate(&mixed, &intent, result.job_id, &committee, 100, &LIMITS)
            .is_err()
    );
}

#[test]
fn certificate_verifier_closes_the_complete_signer_and_result_mutation_matrix() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let result = result();
    let valid = (0..4)
        .map(|index| candidate(index, &result))
        .collect::<Vec<_>>();
    let certificate = build_execution_certificate(
        &valid[..3],
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS,
    )
    .unwrap();

    let mut four_signatures = certificate.clone();
    four_signatures.signer_bitmap = 0b1111;
    four_signatures.ordered_signatures.push(OrderedSignatureV1 {
        validator_index: 3,
        signature_rs: valid[3].signature_rs,
    });
    assert!(four_signatures.verify(&committee, 100, &LIMITS).is_err());

    let mut wrong_population = certificate.clone();
    wrong_population.signer_bitmap = 0b0011;
    assert!(wrong_population.verify(&committee, 100, &LIMITS).is_err());

    let mut high_bitmap_bit = certificate.clone();
    high_bitmap_bit.signer_bitmap |= 1 << 7;
    assert!(high_bitmap_bit.verify(&committee, 100, &LIMITS).is_err());

    let mut wrong_bitmap_binding = certificate.clone();
    wrong_bitmap_binding.signer_bitmap = 0b1011;
    assert!(wrong_bitmap_binding
        .verify(&committee, 100, &LIMITS)
        .is_err());

    let mut reordered = certificate.clone();
    reordered.ordered_signatures.swap(0, 1);
    assert!(reordered.verify(&committee, 100, &LIMITS).is_err());

    let mut wrong_snapshot = certificate.clone();
    wrong_snapshot.result_committee_snapshot_hash = hash(240);
    assert!(wrong_snapshot.verify(&committee, 100, &LIMITS).is_err());

    let mut unknown_index = valid[..3].to_vec();
    unknown_index[2].validator_index = 4;
    assert!(build_execution_certificate(
        &unknown_index,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut wrong_purpose_committee = committee.clone();
    wrong_purpose_committee.ordered_members[2].allowed_purpose_bitmap = 2;
    assert!(build_execution_certificate(
        &valid[..3],
        &intent,
        result.job_id,
        &wrong_purpose_committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut invalid_key_committee = committee.clone();
    invalid_key_committee.ordered_members[2].ocomp_public_key_sec1 = [0; 33];
    assert!(build_execution_certificate(
        &valid[..3],
        &intent,
        result.job_id,
        &invalid_key_committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut signature_mutations = Vec::new();
    signature_mutations.push([0; 64]);
    let mut overflow_r = valid[2].signature_rs;
    overflow_r[..32].fill(0xff);
    signature_mutations.push(overflow_r);
    let mut zero_s = valid[2].signature_rs;
    zero_s[32..].fill(0);
    signature_mutations.push(zero_s);
    let mut overflow_s = valid[2].signature_rs;
    overflow_s[32..].fill(0xff);
    signature_mutations.push(overflow_s);
    for signature_rs in signature_mutations {
        let mut candidates = valid[..3].to_vec();
        candidates[2].signature_rs = signature_rs;
        assert!(build_execution_certificate(
            &candidates,
            &intent,
            result.job_id,
            &committee,
            100,
            &LIMITS
        )
        .is_err());
    }

    let mut different_digest_signature = valid[..3].to_vec();
    different_digest_signature[2].signature_rs = sign(&signing_key(2), hash(241));
    assert!(build_execution_certificate(
        &different_digest_signature,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut claimed_digest = valid[..3].to_vec();
    claimed_digest[2].result_digest = hash(242);
    claimed_digest[2].signature_rs = sign(&signing_key(2), claimed_digest[2].result_digest);
    assert!(build_execution_certificate(
        &claimed_digest,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());

    let mut wrong_job = valid[..3].to_vec();
    wrong_job[2].job_id = hash(243);
    assert!(build_execution_certificate(
        &wrong_job,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS
    )
    .is_err());
}

#[test]
fn activate_lysis_calldata_is_one_standard_canonical_bytes_argument() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let result = result();
    let candidates = (0..3)
        .map(|index| candidate(index, &result))
        .collect::<Vec<_>>();
    let certificate = build_execution_certificate(
        &candidates,
        &intent,
        result.job_id,
        &committee,
        100,
        &LIMITS,
    )
    .unwrap();
    let activation = PoCActivationV1 {
        intent_id: intent.intent_id(&LIMITS).unwrap(),
        finalized_intent_proof: finality_proof(&intent),
        activation_payload: result.activation_payload(&LIMITS).unwrap(),
        result,
        certificate,
    };
    let canonical_activation = activation.encode_canonical(&LIMITS).unwrap();

    let calldata = encode_activate_lysis_calldata(&activation, &LIMITS).unwrap();

    assert_eq!(&calldata[..4], &ACTIVATE_LYSIS_SELECTOR);
    assert_eq!(U256::from_be_slice(&calldata[4..36]), U256::from(32));
    assert_eq!(
        U256::from_be_slice(&calldata[36..68]),
        U256::from(canonical_activation.len())
    );
    assert_eq!(
        &calldata[68..68 + canonical_activation.len()],
        canonical_activation
    );
    assert!(calldata[68 + canonical_activation.len()..]
        .iter()
        .all(|byte| *byte == 0));
    assert_eq!((calldata.len() - 4) % 32, 0);
}

#[test]
fn relay_groups_exact_result_bytes_and_only_emits_activation_at_three_matches() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job = VerifiedRelayJobV1::verify(
        finalized.proof.clone(),
        expected,
        committee.clone(),
        100,
        LIMITS,
    )
    .unwrap();
    let mut result = result();
    result.job_id = finalized.job_id;
    let first_group = [candidate(0, &result), candidate(1, &result)];
    let mut different_result = result.clone();
    different_result.roots.nod_root = hash(62);
    recommit_result(&mut different_result);
    let second_group = [
        candidate(2, &different_result),
        candidate(3, &different_result),
    ];
    let mut relay = CandidateRelayV1::new(verified_job);

    assert!(matches!(
        relay
            .accept_candidate(&first_group[0].encode_canonical(&LIMITS).unwrap(), 100)
            .unwrap(),
        RelayAcceptOutcomeV1::Accepted
    ));
    assert!(matches!(
        relay
            .accept_candidate(&first_group[0].encode_canonical(&LIMITS).unwrap(), 100)
            .unwrap(),
        RelayAcceptOutcomeV1::ExactDuplicate
    ));
    for candidate in first_group.iter().skip(1).chain(&second_group) {
        assert!(matches!(
            relay
                .accept_candidate(&candidate.encode_canonical(&LIMITS).unwrap(), 100)
                .unwrap(),
            RelayAcceptOutcomeV1::Accepted
        ));
    }

    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee.clone(), 100, LIMITS)
            .unwrap();
    let mut relay = CandidateRelayV1::new(verified_job);
    let canonical = [
        candidate(2, &result),
        candidate(0, &result),
        candidate(1, &result),
    ];
    for candidate in &canonical[..2] {
        assert!(matches!(
            relay
                .accept_candidate(&candidate.encode_canonical(&LIMITS).unwrap(), 100)
                .unwrap(),
            RelayAcceptOutcomeV1::Accepted
        ));
    }
    let RelayAcceptOutcomeV1::ActivationReady {
        activation,
        calldata,
    } = relay
        .accept_candidate(&canonical[2].encode_canonical(&LIMITS).unwrap(), 100)
        .unwrap()
    else {
        panic!("third exact match must produce activation");
    };

    activation
        .verify(finalized.state_root, &committee, 100, &LIMITS)
        .unwrap();
    assert_eq!(&calldata[..4], &ACTIVATE_LYSIS_SELECTOR);
}

#[test]
fn relay_rejects_mutated_public_authority_and_recovers_by_resubmission_after_loss() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };

    let mut bad_header = finalized.proof.clone();
    bad_header.canonical_request_header_rlp.0[0] ^= 1;
    assert!(
        VerifiedRelayJobV1::verify(bad_header, expected, committee.clone(), 100, LIMITS).is_err()
    );

    let mut bad_state_proof = finalized.proof.clone();
    let last = bad_state_proof.intent_storage_proof.0.len() - 1;
    bad_state_proof.intent_storage_proof.0[last] ^= 1;
    assert!(
        VerifiedRelayJobV1::verify(bad_state_proof, expected, committee.clone(), 100, LIMITS)
            .is_err()
    );

    let mut wrong_committee = committee.clone();
    wrong_committee.snapshot_epoch += 1;
    assert!(VerifiedRelayJobV1::verify(
        finalized.proof.clone(),
        expected,
        wrong_committee,
        100,
        LIMITS
    )
    .is_err());

    let mut result = result();
    result.job_id = finalized.job_id;
    let announcements = (0..3)
        .map(|index| candidate(index, &result).encode_canonical(&LIMITS).unwrap())
        .collect::<Vec<_>>();
    let verified_job = VerifiedRelayJobV1::verify(
        finalized.proof.clone(),
        expected,
        committee.clone(),
        100,
        LIMITS,
    )
    .unwrap();
    let mut lost_relay = CandidateRelayV1::new(verified_job);
    for announcement in &announcements[..2] {
        assert!(matches!(
            lost_relay.accept_candidate(announcement, 100).unwrap(),
            RelayAcceptOutcomeV1::Accepted
        ));
    }
    drop(lost_relay);

    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let mut restarted_relay = CandidateRelayV1::new(verified_job);
    for announcement in &announcements[..2] {
        assert!(matches!(
            restarted_relay.accept_candidate(announcement, 100).unwrap(),
            RelayAcceptOutcomeV1::Accepted
        ));
    }
    assert!(matches!(
        restarted_relay
            .accept_candidate(&announcements[2], 100)
            .unwrap(),
        RelayAcceptOutcomeV1::ActivationReady { .. }
    ));
}

#[test]
fn relay_bounds_and_canonical_decoding_fail_before_candidate_storage() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let mut relay = CandidateRelayV1::new(verified_job);
    let mut result = result();
    result.job_id = finalized.job_id;
    let first = candidate(0, &result);
    let mut first_bytes = first.encode_canonical(&LIMITS).unwrap();

    let oversized = vec![0; LIMITS.max_control_body_bytes + 1];
    assert!(relay.accept_candidate(&oversized, 100).is_err());

    first_bytes.push(0);
    assert!(relay.accept_candidate(&first_bytes, 100).is_err());

    let first_bytes = first.encode_canonical(&LIMITS).unwrap();
    assert!(matches!(
        relay.accept_candidate(&first_bytes, 100).unwrap(),
        RelayAcceptOutcomeV1::Accepted
    ));
    let mut conflicting_result = result;
    conflicting_result.roots.nod_root = hash(62);
    recommit_result(&mut conflicting_result);
    let conflicting = candidate(0, &conflicting_result)
        .encode_canonical(&LIMITS)
        .unwrap();
    assert!(relay.accept_candidate(&conflicting, 100).is_err());
    assert!(matches!(
        relay.accept_candidate(&first_bytes, 100).unwrap(),
        RelayAcceptOutcomeV1::ExactDuplicate
    ));
}

fn http_round_trip(server: &Arc<RelayHttpServerV1>, request: &[u8]) -> Vec<u8> {
    let serving = Arc::clone(server);
    let handle = std::thread::spawn(move || serving.serve_one().unwrap());
    let mut stream = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    stream.write_all(request).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    handle.join().unwrap();
    response
}

fn candidate_http_request(body: &[u8]) -> Vec<u8> {
    let mut request = format!(
        "POST /v1/candidates HTTP/1.1\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

#[derive(Default)]
struct RecordingActivationPublisherV1 {
    activations: Mutex<Vec<PoCActivationV1>>,
}

impl ActivationPublisherV1 for RecordingActivationPublisherV1 {
    fn publish(
        &self,
        activation: &PoCActivationV1,
        _limits: &SchemaLimits,
    ) -> Result<B256, String> {
        self.activations.lock().unwrap().push(activation.clone());
        Ok(hash(99))
    }
}

#[test]
fn relay_http_exposes_only_health_and_bounded_canonical_candidate_ingress() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let publisher = Arc::new(RecordingActivationPublisherV1::default());
    let server = Arc::new(
        RelayHttpServerV1::bind(
            "127.0.0.1:0",
            CandidateRelayV1::new(verified_job),
            publisher,
            Arc::new(MutableRelayHeightSourceV1::new(100)),
        )
        .unwrap(),
    );
    let mut result = result();
    result.job_id = finalized.job_id;
    let announcement = candidate(0, &result).encode_canonical(&LIMITS).unwrap();

    let health = http_round_trip(
        &server,
        b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(health.starts_with(b"HTTP/1.1 200 OK\r\n"));

    let accepted = http_round_trip(&server, &candidate_http_request(&announcement));
    assert!(accepted.starts_with(b"HTTP/1.1 202 Accepted\r\n"));
    let duplicate = http_round_trip(&server, &candidate_http_request(&announcement));
    assert!(duplicate.starts_with(b"HTTP/1.1 200 OK\r\n"));

    let oversized = format!(
        "POST /v1/candidates HTTP/1.1\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        LIMITS.max_control_body_bytes + 1
    );
    let rejected = http_round_trip(&server, oversized.as_bytes());
    assert!(rejected.starts_with(b"HTTP/1.1 413 Payload Too Large\r\n"));

    let unknown = http_round_trip(
        &server,
        b"GET /v1/candidates HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(unknown.starts_with(b"HTTP/1.1 404 Not Found\r\n"));
}

#[test]
fn malformed_http_connection_does_not_terminate_the_relay_loop() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let server = Arc::new(
        RelayHttpServerV1::bind(
            "127.0.0.1:0",
            CandidateRelayV1::new(verified_job),
            Arc::new(RecordingActivationPublisherV1::default()),
            Arc::new(MutableRelayHeightSourceV1::new(100)),
        )
        .unwrap(),
    );
    let serving = server.clone();
    std::thread::spawn(move || serving.serve().unwrap());

    let mut malformed = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    malformed
        .write_all(b"POST /v1/candidates HTTP/1.1\r\n")
        .unwrap();
    malformed.shutdown(Shutdown::Both).unwrap();

    let mut health = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    health
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    health
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    health.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    health.read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
}

#[test]
fn relay_http_publishes_q3_activation_once_and_keeps_exact_duplicates_idempotent() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let publisher = Arc::new(RecordingActivationPublisherV1::default());
    let server = Arc::new(
        RelayHttpServerV1::bind(
            "127.0.0.1:0",
            CandidateRelayV1::new(verified_job),
            publisher.clone(),
            Arc::new(MutableRelayHeightSourceV1::new(100)),
        )
        .unwrap(),
    );
    let mut result = result();
    result.job_id = finalized.job_id;
    let announcements = (0..3)
        .map(|index| candidate(index, &result).encode_canonical(&LIMITS).unwrap())
        .collect::<Vec<_>>();

    for announcement in &announcements {
        let response = http_round_trip(&server, &candidate_http_request(announcement));
        assert!(response.starts_with(b"HTTP/1.1 202 Accepted\r\n"));
    }
    assert_eq!(publisher.activations.lock().unwrap().len(), 1);

    let duplicate = http_round_trip(&server, &candidate_http_request(&announcements[2]));
    assert!(duplicate.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert_eq!(publisher.activations.lock().unwrap().len(), 1);
}

#[derive(Default)]
struct RetryOnceActivationPublisherV1 {
    attempts: AtomicUsize,
}

impl ActivationPublisherV1 for RetryOnceActivationPublisherV1 {
    fn publish(
        &self,
        _activation: &PoCActivationV1,
        _limits: &SchemaLimits,
    ) -> Result<B256, String> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("simulated uncertain RPC failure".to_owned())
        } else {
            Ok(hash(98))
        }
    }
}

struct MutableRelayHeightSourceV1 {
    height: AtomicU64,
}

impl MutableRelayHeightSourceV1 {
    const fn new(height: u64) -> Self {
        Self {
            height: AtomicU64::new(height),
        }
    }

    fn advance_to(&self, height: u64) {
        self.height.store(height, Ordering::SeqCst);
    }
}

impl RelayHeightSourceV1 for MutableRelayHeightSourceV1 {
    fn current_height(&self) -> Result<u64, String> {
        Ok(self.height.load(Ordering::SeqCst))
    }
}

#[test]
fn relay_http_retries_unconfirmed_publication_without_reforming_authority() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let publisher = Arc::new(RetryOnceActivationPublisherV1::default());
    let server = Arc::new(
        RelayHttpServerV1::bind(
            "127.0.0.1:0",
            CandidateRelayV1::new(verified_job),
            publisher.clone(),
            Arc::new(MutableRelayHeightSourceV1::new(100)),
        )
        .unwrap(),
    );
    let mut result = result();
    result.job_id = finalized.job_id;
    let announcements = (0..3)
        .map(|index| candidate(index, &result).encode_canonical(&LIMITS).unwrap())
        .collect::<Vec<_>>();
    for announcement in &announcements[..2] {
        let response = http_round_trip(&server, &candidate_http_request(announcement));
        assert!(response.starts_with(b"HTTP/1.1 202 Accepted\r\n"));
    }

    let unavailable = http_round_trip(&server, &candidate_http_request(&announcements[2]));
    assert!(unavailable.starts_with(b"HTTP/1.1 503 Service Unavailable\r\n"));
    let retried = http_round_trip(&server, &candidate_http_request(&announcements[2]));
    assert!(retried.starts_with(b"HTTP/1.1 202 Accepted\r\n"));
    let confirmed_duplicate = http_round_trip(&server, &candidate_http_request(&announcements[2]));
    assert!(confirmed_duplicate.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert_eq!(publisher.attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn relay_rechecks_the_public_height_for_every_candidate() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let deadline_height = intent.deadline_height;
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let publisher = Arc::new(RecordingActivationPublisherV1::default());
    let height = Arc::new(MutableRelayHeightSourceV1::new(100));
    let server = Arc::new(
        RelayHttpServerV1::bind(
            "127.0.0.1:0",
            CandidateRelayV1::new(verified_job),
            publisher.clone(),
            height.clone(),
        )
        .unwrap(),
    );
    let mut result = result();
    result.job_id = finalized.job_id;
    let announcements = (0..3)
        .map(|index| candidate(index, &result).encode_canonical(&LIMITS).unwrap())
        .collect::<Vec<_>>();

    for announcement in &announcements[..2] {
        let response = http_round_trip(&server, &candidate_http_request(announcement));
        assert!(response.starts_with(b"HTTP/1.1 202 Accepted\r\n"));
    }
    height.advance_to(deadline_height);
    let expired = http_round_trip(&server, &candidate_http_request(&announcements[2]));
    assert!(expired.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
    assert!(publisher.activations.lock().unwrap().is_empty());
}

fn read_test_http_request(stream: &mut TcpStream) -> serde_json::Value {
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).unwrap();
    let content_length = header
        .split("\r\n")
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
        })
        .unwrap();
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn public_rpc_client_pins_finality_header_and_state_proof_to_one_exact_block() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        for response_result in [
            serde_json::json!({
                "finalizationHex": "0x0102",
                "blockHex": "0x0304"
            }),
            serde_json::json!({
                "hash": format!("{:#x}", hash(90)),
                "stateRoot": format!("{:#x}", hash(91)),
                "number": "0x40"
            }),
            serde_json::json!({
                "address": format!("{:#x}", Address::repeat_byte(0x10)),
                "balance": "0x0",
                "codeHash": format!("{:#x}", hash(94)),
                "nonce": "0x0",
                "storageHash": format!("{:#x}", hash(95)),
                "accountProof": ["0x01"],
                "storageProof": []
            }),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut stream);
            let id = request["id"].clone();
            observed.lock().unwrap().push(request);
            let body = serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": response_result
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    let client =
        PublicExactBlockRpcClientV1::new(format!("http://{address}"), LIMITS.max_proof_bytes)
            .unwrap();
    let block_hash = hash(90);
    let proof_address = Address::repeat_byte(0x10);
    let slots = [hash(92), hash(93)];

    let finalization = client.finalization(64).unwrap();
    assert_eq!(finalization.finalization_bytes, vec![1, 2]);
    assert_eq!(finalization.block_bytes, vec![3, 4]);
    let block = client.block_by_hash(block_hash).unwrap();
    assert_eq!(block.hash, block_hash);
    assert_eq!(block.state_root, hash(91));
    assert_eq!(block.number, 64);
    client
        .account_proof(proof_address, &slots, block_hash)
        .unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests[0]["method"], "outbe_getFinalization");
    assert_eq!(requests[0]["params"], serde_json::json!([64]));
    assert_eq!(requests[1]["method"], "eth_getBlockByHash");
    assert_eq!(
        requests[1]["params"],
        serde_json::json!([format!("{block_hash:#x}"), false])
    );
    assert_eq!(requests[2]["method"], "eth_getProof");
    assert_eq!(
        requests[2]["params"],
        serde_json::json!([
            format!("{proof_address:#x}"),
            slots
                .iter()
                .map(|slot| format!("{slot:#x}"))
                .collect::<Vec<_>>(),
            {
                "blockHash": format!("{block_hash:#x}"),
                "requireCanonical": true
            }
        ])
    );
}

#[test]
fn normal_activation_payer_submits_one_standard_ethereum_transaction() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let mut relay = CandidateRelayV1::new(verified_job);
    let mut result = result();
    result.job_id = finalized.job_id;
    let mut activation_and_calldata = None;
    for validator_index in 0..3 {
        let announcement = candidate(validator_index, &result)
            .encode_canonical(&LIMITS)
            .unwrap();
        if let RelayAcceptOutcomeV1::ActivationReady {
            activation,
            calldata,
        } = relay.accept_candidate(&announcement, 100).unwrap()
        {
            activation_and_calldata = Some((activation, calldata));
        }
    }
    let (activation, activation_calldata) =
        activation_and_calldata.expect("q=3 produces activation");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        for expected_method in [
            "eth_chainId",
            "eth_getTransactionCount",
            "eth_gasPrice",
            "eth_estimateGas",
            "eth_sendRawTransaction",
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut stream);
            assert_eq!(request["method"], expected_method);
            let id = request["id"].clone();
            let result = match expected_method {
                "eth_chainId" => serde_json::json!("0x2a"),
                "eth_getTransactionCount" => serde_json::json!("0x7"),
                "eth_gasPrice" => serde_json::json!("0x64"),
                "eth_estimateGas" => serde_json::json!("0x5208"),
                "eth_sendRawTransaction" => {
                    let raw = request["params"][0].as_str().unwrap();
                    let raw = hex::decode(raw.strip_prefix("0x").unwrap()).unwrap();
                    serde_json::json!(format!("{:#x}", alloy_primitives::keccak256(raw)))
                }
                _ => unreachable!(),
            };
            observed.lock().unwrap().push(request);
            let body = serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });

    let signer = OutbeEvmSigner::from_secret_bytes([99; 32]).unwrap();
    let payer = signer.address();
    let rpc = PublicExactBlockRpcClientV1::new(format!("http://{address}"), LIMITS.max_proof_bytes)
        .unwrap();
    let submitter = NormalActivationSubmitterV1::new(rpc, signer);
    let submitted_hash = submitter.submit(&activation, &LIMITS).unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[1]["params"],
        serde_json::json!([format!("{payer:#x}"), "pending"])
    );
    assert_eq!(
        requests[3]["params"],
        serde_json::json!([{
            "from": format!("{payer:#x}"),
            "to": format!("{METADOSIS_ADDRESS:#x}"),
            "data": format!("0x{}", hex::encode(&activation_calldata)),
            "value": "0x0"
        }])
    );

    let raw = requests[4]["params"][0].as_str().unwrap();
    let raw = hex::decode(raw.strip_prefix("0x").unwrap()).unwrap();
    let mut raw_slice = raw.as_slice();
    let transaction = TransactionSigned::decode_2718(&mut raw_slice).unwrap();
    assert!(raw_slice.is_empty());
    assert_eq!(transaction.chain_id(), Some(42));
    assert_eq!(transaction.nonce(), 7);
    assert_eq!(transaction.gas_price(), Some(100));
    assert_eq!(transaction.gas_limit(), 25_200);
    assert_eq!(transaction.to(), Some(METADOSIS_ADDRESS));
    assert_eq!(transaction.value(), U256::ZERO);
    assert_eq!(transaction.input().as_ref(), activation_calldata);
    assert_eq!(transaction.try_recover().unwrap(), payer);
    assert_eq!(submitted_hash, *transaction.hash());
}

#[test]
fn normal_activation_retry_rebroadcasts_the_exact_signed_transaction() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job =
        VerifiedRelayJobV1::verify(finalized.proof, expected, committee, 100, LIMITS).unwrap();
    let mut relay = CandidateRelayV1::new(verified_job);
    let mut result = result();
    result.job_id = finalized.job_id;
    let mut activation = None;
    for validator_index in 0..3 {
        let announcement = candidate(validator_index, &result)
            .encode_canonical(&LIMITS)
            .unwrap();
        if let RelayAcceptOutcomeV1::ActivationReady {
            activation: ready, ..
        } = relay.accept_candidate(&announcement, 100).unwrap()
        {
            activation = Some(ready);
        }
    }
    let activation = activation.expect("q=3 produces activation");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        for request_index in 0..6 {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut stream);
            let method = request["method"].as_str().unwrap();
            let expected_method = match request_index {
                0 => "eth_chainId",
                1 => "eth_getTransactionCount",
                2 => "eth_gasPrice",
                3 => "eth_estimateGas",
                4 | 5 => "eth_sendRawTransaction",
                _ => unreachable!(),
            };
            assert_eq!(method, expected_method);
            observed.lock().unwrap().push(request.clone());

            // The first transaction was accepted, but its HTTP response was
            // lost. A safe retry must broadcast the same signed bytes.
            if request_index == 4 {
                continue;
            }

            let id = request["id"].clone();
            let result = match method {
                "eth_chainId" => serde_json::json!("0x2a"),
                "eth_getTransactionCount" => serde_json::json!("0x7"),
                "eth_gasPrice" => serde_json::json!("0x64"),
                "eth_estimateGas" => serde_json::json!("0x5208"),
                "eth_sendRawTransaction" => {
                    let raw = request["params"][0].as_str().unwrap();
                    let raw = hex::decode(raw.strip_prefix("0x").unwrap()).unwrap();
                    serde_json::json!(format!("{:#x}", alloy_primitives::keccak256(raw)))
                }
                _ => unreachable!(),
            };
            let body = serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });

    let signer = OutbeEvmSigner::from_secret_bytes([99; 32]).unwrap();
    let rpc = PublicExactBlockRpcClientV1::new(format!("http://{address}"), LIMITS.max_proof_bytes)
        .unwrap();
    let submitter = NormalActivationSubmitterV1::new(rpc, signer);
    assert!(submitter.submit(&activation, &LIMITS).is_err());
    let retried_hash = submitter.submit(&activation, &LIMITS).unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[4]["params"][0], requests[5]["params"][0]);
    let raw = requests[5]["params"][0].as_str().unwrap();
    let raw = hex::decode(raw.strip_prefix("0x").unwrap()).unwrap();
    assert_eq!(retried_hash, alloy_primitives::keccak256(raw));
}

#[derive(Clone)]
struct FixtureExactBlockSourceV1 {
    fixture: outbe_e2e_harness::ocomp_finality_fixture::PublicExactBlockFixtureV1,
}

impl PublicExactBlockProofSourceV1 for FixtureExactBlockSourceV1 {
    type Error = std::io::Error;

    fn finalization(&self, height: u64) -> Result<PublicFinalizationBytesV1, Self::Error> {
        if height != self.fixture.block_view.number {
            return Err(std::io::Error::other("unknown finalized height"));
        }
        Ok(PublicFinalizationBytesV1 {
            finalization_bytes: self.fixture.finalization_bytes.clone(),
            block_bytes: self.fixture.block_bytes.clone(),
        })
    }

    fn block_by_hash(&self, block_hash: B256) -> Result<PublicBlockViewV1, Self::Error> {
        if block_hash != self.fixture.block_view.hash {
            return Err(std::io::Error::other("unknown block hash"));
        }
        Ok(self.fixture.block_view)
    }

    fn job_record(&self, intent_id: B256, block_hash: B256) -> Result<Vec<u8>, Self::Error> {
        if intent_id != self.fixture.intent_id || block_hash != self.fixture.block_view.hash {
            return Err(std::io::Error::other("unknown exact-block job"));
        }
        Ok(self.fixture.canonical_job_record.clone())
    }

    fn account_proof(
        &self,
        address: Address,
        storage_slots: &[B256],
        block_hash: B256,
    ) -> Result<PublicAccountProofV1, Self::Error> {
        if block_hash != self.fixture.block_view.hash {
            return Err(std::io::Error::other("unknown exact block"));
        }
        let mut proof = self
            .fixture
            .account_proofs
            .get(&address)
            .cloned()
            .ok_or_else(|| std::io::Error::other("unknown proof address"))?;
        proof
            .storage_proofs
            .retain(|entry| storage_slots.contains(&entry.key));
        if proof.storage_proofs.len() != storage_slots.len() {
            return Err(std::io::Error::other("unknown storage slot"));
        }
        Ok(proof)
    }
}

#[test]
fn public_exact_block_material_rebuilds_and_verifies_the_canonical_intent_proof() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let source = FixtureExactBlockSourceV1 {
        fixture: finalized.public_exact_block.clone(),
    };
    let builder = PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS);

    let (rebuilt, verified) = builder
        .build_and_verify(
            intent.logical_evaluation_height,
            finalized.intent_id,
            expected,
        )
        .unwrap();

    assert_eq!(rebuilt, finalized.proof);
    assert_eq!(verified.intent_id, finalized.intent_id);
    assert_eq!(verified.job_id, finalized.job_id);
    assert_eq!(verified.request.state_root, finalized.state_root);
}

#[test]
fn public_finalization_bytes_are_authenticated_before_relay_use() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };

    let mut mutated = finalized.public_exact_block.clone();
    let finalization_last = mutated.finalization_bytes.len() - 1;
    mutated.finalization_bytes[finalization_last] ^= 1;
    let source = FixtureExactBlockSourceV1 { fixture: mutated };
    assert!(PublicFinalizedIntentProofBuilderV1::new(&source, LIMITS)
        .build_and_verify(
            intent.logical_evaluation_height,
            finalized.intent_id,
            expected,
        )
        .is_err());
}

fn rpc_account_proof_json(
    proof: &PublicAccountProofV1,
    requested_slots: &[B256],
) -> serde_json::Value {
    let storage_proof = requested_slots
        .iter()
        .map(|slot| {
            let entry = proof
                .storage_proofs
                .iter()
                .find(|entry| entry.key == *slot)
                .expect("fixture contains requested slot");
            serde_json::json!({
                "key": format!("{:#x}", entry.key),
                "value": format!("{:#x}", entry.value),
                "proof": entry
                    .nodes
                    .iter()
                    .map(|node| format!("0x{}", hex::encode(node)))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "address": format!("{:#x}", proof.address),
        "balance": format!("{:#x}", proof.balance),
        "codeHash": format!("{:#x}", proof.code_hash),
        "nonce": format!("0x{:x}", proof.nonce),
        "storageHash": format!("{:#x}", proof.storage_root),
        "accountProof": proof
            .account_nodes
            .iter()
            .map(|node| format!("0x{}", hex::encode(node)))
            .collect::<Vec<_>>(),
        "storageProof": storage_proof
    })
}

fn abi_bytes_return(bytes: &[u8]) -> String {
    let mut encoded = vec![0_u8; 64 + bytes.len().div_ceil(32) * 32];
    encoded[31] = 32;
    U256::from(bytes.len())
        .to_be_bytes::<32>()
        .iter()
        .enumerate()
        .for_each(|(index, byte)| encoded[32 + index] = *byte);
    encoded[64..64 + bytes.len()].copy_from_slice(bytes);
    format!("0x{}", hex::encode(encoded))
}

#[test]
fn public_rpc_composition_rebuilds_verified_job_without_private_proof_injection() {
    let committee = committee();
    let intent = intent(committee.snapshot_hash(&LIMITS).unwrap());
    let finalized = finalized_intent_proof_fixture(intent.clone(), &LIMITS);
    let public_fixture = finalized.public_exact_block.clone();
    let server_fixture = public_fixture.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let server = std::thread::spawn(move || {
        for _ in 0..7 {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut stream);
            let method = request["method"].as_str().unwrap();
            let result = match method {
                "outbe_getFinalization" => serde_json::json!({
                    "finalizationHex": format!(
                        "0x{}",
                        hex::encode(&server_fixture.finalization_bytes)
                    ),
                    "blockHex": format!("0x{}", hex::encode(&server_fixture.block_bytes))
                }),
                "eth_getBlockByHash" => serde_json::json!({
                    "hash": format!("{:#x}", server_fixture.block_view.hash),
                    "stateRoot": format!("{:#x}", server_fixture.block_view.state_root),
                    "number": format!("0x{:x}", server_fixture.block_view.number)
                }),
                "eth_call" => {
                    serde_json::json!(abi_bytes_return(&server_fixture.canonical_job_record))
                }
                "eth_getProof" => {
                    let proof_address = request["params"][0]
                        .as_str()
                        .unwrap()
                        .parse::<Address>()
                        .unwrap();
                    let slots = request["params"][1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|value| value.as_str().unwrap().parse::<B256>().unwrap())
                        .collect::<Vec<_>>();
                    rpc_account_proof_json(&server_fixture.account_proofs[&proof_address], &slots)
                }
                other => panic!("unexpected RPC method {other}"),
            };
            observed.lock().unwrap().push(request.clone());
            let body = serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": result
            }))
            .unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });

    let client =
        PublicExactBlockRpcClientV1::new(format!("http://{address}"), LIMITS.max_proof_bytes)
            .unwrap();
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let verified_job = client
        .verified_relay_job(
            intent.logical_evaluation_height,
            finalized.intent_id,
            expected,
            committee,
            100,
            LIMITS,
        )
        .unwrap();
    server.join().unwrap();

    let mut relay = CandidateRelayV1::new(verified_job);
    let mut result = result();
    result.job_id = finalized.job_id;
    for validator_index in 0..2 {
        let announcement = candidate(validator_index, &result)
            .encode_canonical(&LIMITS)
            .unwrap();
        assert!(matches!(
            relay.accept_candidate(&announcement, 100).unwrap(),
            RelayAcceptOutcomeV1::Accepted
        ));
    }
    let third = candidate(2, &result).encode_canonical(&LIMITS).unwrap();
    assert!(matches!(
        relay.accept_candidate(&third, 100).unwrap(),
        RelayAcceptOutcomeV1::ActivationReady { .. }
    ));

    let methods = requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["method"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        [
            "outbe_getFinalization",
            "eth_getBlockByHash",
            "eth_getProof",
            "eth_getProof",
            "eth_getProof",
            "eth_call",
            "eth_getProof",
        ]
    );
}
