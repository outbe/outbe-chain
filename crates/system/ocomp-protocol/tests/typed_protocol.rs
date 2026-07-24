use alloy_primitives::{keccak256, Address, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use outbe_ocomp_protocol::{
    abi::{
        ACTIVATE_LYSIS_SELECTOR, GET_ACTIVE_LYSIS_GENERATION_SELECTOR,
        GET_LYSIS_TERMINAL_RECEIPT_SELECTOR, GET_OFFCHAIN_JOB_SELECTOR, LYSIS_ACTIVATED_TOPIC0,
        OCOMP_ACTIVATION_REJECTED_SELECTOR, OCOMP_CONFLICTED_TOPIC0, OCOMP_EXPIRED_TOPIC0,
        OCOMP_REQUESTED_TOPIC0,
    },
    activation::{
        ActivationCallCoreV1, CandidateAnnouncementV1, PoCActivationV1, SignOncePurpose,
        SignOnceRecordV1,
    },
    certificate::{ExecutionCertificateV1, OrderedSignatureV1},
    codec::CodecLimits,
    committee::{
        verify_low_s_prehash, OcompCommitteeSnapshotV1, OcompKeyRegistrationCoreV1,
        OcompKeyRegistrationV1, OcompMemberV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::{BoundedBytes, EntityId36, ProofBytes},
    control::{
        ControlFrameV1, ControlMagic, FinalizedJobSpecV1, FinalizedJobSummaryV1, GetJobSpecV1,
        ListFinalizedJobsResponseV1, ListFinalizedJobsV1, RunUnitV1, CONTROL_FRAME_HEADER_LEN,
        MAX_FINALIZED_JOBS_PER_RESPONSE, WORKER_CONTROL_MAGIC,
    },
    hash::hash_framed,
    input::{CheckpointIdentityV1, Compression, InputManifestV1},
    intent::{
        ActivationPreconditionsV1, CertifiedParentAccountingMetadataV2,
        ContributorTargetPreconditionV1, DayType, FinalizedIntentProofV1, FrozenMetadosisValuesV1,
        JobIntentV1, MetadosisAttemptPreconditionV1, MetadosisExpectedStatus,
        NodTargetPreconditionV1, ParentProofKind, PreAdmissionEnvelopeV1, TributeInputBindingV1,
    },
    profile::{CapacityProfileV1, CorrectnessProfileV1, ProgramId, ProtocolBundleV1},
    receipts::{
        desis_request_brief_hash, ActivationOutcome, AggregateActivationReceiptV1,
        BudgetSplitDestination, CarryOverReceiptV1, ContributorReceiptV1, EffectBindingV1,
        NodBatchReceiptV1, RequestBudgetSplitReceiptV1, TributeReceiptV1,
    },
    registry::{HashDomain, ObjectKind},
    result::{
        ActivationPayloadV1, CarryOverCreditActionV1, CarryOverReason, CompletionStatus,
        ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1, LysisResultV1,
        MetadosisCompletionSummaryV1, ResultChunkV1, ResultRootsV1,
    },
    state::{
        ActiveGenerationV1, OcompJobRecordV1, OcompJobStatus, OcompJobTerminalV1,
        OcompTerminalOutcome,
    },
    unit::{
        BinaryReducerNode, EntityIdHalfOpenRange, PlanCommitmentV1, UnitArtifactV1, UnitInterval,
        UnitPhase, UnitSpecV1,
    },
    ProtocolError, SchemaLimits,
};

const LIMITS: SchemaLimits = SchemaLimits {
    codec: CodecLimits::new(1_048_576, 4_096, 2_097_152),
    max_bounded_bytes: 262_144,
    max_proof_bytes: 262_144,
    max_opening_bytes: 262_144,
    max_collection_items: 4_096,
    max_action_items: 4_096,
    max_chunk_items: 4_096,
    max_unit_inputs: 64,
    max_control_body_bytes: 262_144,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(1),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        result_codec_id: hash(4),
        action_codec_id: hash(5),
        activation_codec_id: hash(6),
        evidence_codec_id: hash(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: hash(9),
        effect_contract_registry_hash: hash(10),
        object_codec_registry_hash: hash(11),
        correctness_profile_id: hash(12),
        capacity_profile_id: hash(13),
        result_signature_profile_id: hash(14),
        finality_verifier_and_vote_domain_id: hash(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: hash(16),
        mode_pause_revocation_semantics_hash: hash(17),
        upgrade_fsm_semantics_hash: hash(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: hash(19),
        release_requirement_catalog_parent_hash: hash(20),
        release_gate_authority_envelope_hash: hash(21),
        release_approval_policy_hash: hash(22),
        release_validator_command_artifact_hash: hash(23),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(24),
        required_upgrade_handler_set_hash: hash(25),
    }
}

fn preconditions() -> ActivationPreconditionsV1 {
    ActivationPreconditionsV1 {
        tribute: TributeInputBindingV1 {
            wwd: 7,
            source_generation: 3,
            collection_key: hash(30),
            sealed_collection_root: hash(31),
            exact_count: 0,
            exact_nominal_total: U256::ZERO,
        },
        nod: NodTargetPreconditionV1 {
            wwd: 7,
            target_generation: 5,
            namespace_root_before: hash(32),
            max_nod_count: 0,
        },
        contributors: ContributorTargetPreconditionV1 {
            series_id: 7,
            expected_series_version: 8,
            max_contributor_count: 0,
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

fn intent() -> JobIntentV1 {
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
        authenticated_day_count: 0,
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
        result_committee_snapshot_hash: hash(45),
        custody_committee_epoch_hash: None,
        deadline_height: 110,
    }
}

fn finality_proof() -> FinalizedIntentProofV1 {
    let intent = intent();
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
    let carry_over_credit = CarryOverCreditActionV1 {
        source_wwd: 7,
        reason: CarryOverReason::UnusedLysis,
        amount: U256::ZERO,
    };
    let metadosis_completion_summary = MetadosisCompletionSummaryV1 {
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
    };
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
    let arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &summary.encode_canonical(&LIMITS).unwrap(),
    )
    .unwrap();
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
        carry_over_credit,
        metadosis_completion_summary,
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment,
        event_summary_hash: hash(60),
    }
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

fn certificate(result_digest: B256) -> ExecutionCertificateV1 {
    let snapshot = committee();
    ExecutionCertificateV1 {
        result_committee_snapshot_hash: snapshot.snapshot_hash(&LIMITS).unwrap(),
        signer_bitmap: 0b0111,
        ordered_signatures: (0..3)
            .map(|index| OrderedSignatureV1 {
                validator_index: index,
                signature_rs: sign(&signing_key(index), result_digest),
            })
            .collect(),
        result_digest,
    }
}

fn binding() -> EffectBindingV1 {
    EffectBindingV1 {
        intent_id: hash(80),
        job_id: hash(59),
        attempt: 1,
        protocol_bundle_hash: hash(41),
        result_digest: hash(81),
        activation_preconditions_hash: hash(82),
        activation_call_id: hash(83),
    }
}

fn conflict_receipt() -> AggregateActivationReceiptV1 {
    AggregateActivationReceiptV1 {
        binding: binding(),
        outcome: ActivationOutcome::ConflictResolved,
        nod_receipt_hash: None,
        contributor_receipt_hash: None,
        tribute_receipt_hash: None,
        carry_over_receipt_hash: None,
        request_budget_split_receipt_hash: hash(113),
        active_generation_hash: None,
        effect_commitment: hash_framed(HashDomain::Effects, &[]).unwrap(),
        event_summary_hash: hash(84),
        activated_at_height: 101,
        activated_at_time: 1_001,
    }
}

macro_rules! assert_round_trip {
    ($value:expr, $type:ty, $kind:ident) => {{
        let value: $type = $value;
        let encoded = value.encode_canonical(&LIMITS).unwrap();
        assert_eq!(
            outbe_ocomp_protocol::decode_envelope(&encoded, LIMITS.codec)
                .unwrap()
                .kind,
            ObjectKind::$kind
        );
        assert_eq!(<$type>::decode_canonical(&encoded, &LIMITS).unwrap(), value);
        let mut trailing = encoded;
        trailing.push(0);
        assert!(<$type>::decode_canonical(&trailing, &LIMITS).is_err());
    }};
}

#[test]
fn every_registered_object_round_trips_and_rejects_trailing_bytes() {
    let correctness = CorrectnessProfileV1 {
        profile_id: hash(90),
        program: ProgramId::LysisV1,
        arithmetic_profile_id: hash(91),
        object_codec_registry_hash: hash(92),
        list_root_scheme_id: hash(93),
        result_signature_profile_id: hash(94),
        finality_verifier_profile_id: hash(95),
    };
    let capacity = CapacityProfileV1 {
        profile_id: hash(96),
        max_tributes_per_work_shard: 32,
        max_workers_per_domain: 4,
        max_pending_jobs: 8,
        max_intents_per_block: 2,
        max_activations_per_block: 2,
        max_ready_inspections_per_block: 4,
        max_expirations_per_block: 4,
        retry_backoff_blocks: 2,
        max_terminal_job_records: 365,
        max_reference_currencies: 16,
        max_fidelity_cohorts_per_owner: 16,
        max_oracle_wwd_pair_entries: 128,
        max_active_scurve_entries: 128,
        result_deadline_blocks: 10,
        source_retention_after_terminal_blocks: 100,
        generated_limits_manifest_hash: hash(97),
    };
    let pre_admission = PreAdmissionEnvelopeV1 {
        chain_id: 42,
        genesis_hash: hash(40),
        fork_id: hash(1),
        wwd: 7,
        sealed_tribute_collection_root: hash(31),
        sealed_tribute_count: 0,
        sealed_tribute_canonical_body_bytes: 0,
        distinct_owner_count: 0,
        distinct_reference_currency_count: 0,
        max_fidelity_cohorts_observed: 0,
        oracle_wwd_pair_entries_observed: 0,
        active_scurve_entries_observed: 0,
        auction_entry_price: U256::ZERO,
        auction_entry_price_source:
            outbe_ocomp_protocol::intent::AuctionEntryPriceSource::LastClosedDayVwap,
        auction_entry_price_source_day: 6,
        oracle_state_version: 1,
        fidelity_opening_upper_bound: 0,
        oracle_opening_upper_bound: 0,
        input_encoded_bytes_upper_bound: 0,
        output_record_upper_bound: 0,
        result_chunk_bytes_upper_bound: 1_024,
        activation_bytes_upper_bound: 2_048,
        retained_bytes_upper_bound: 4_096,
        correctness_profile_id: hash(90),
        capacity_profile_id: hash(96),
    };
    let manifest = InputManifestV1 {
        protocol_bundle_hash: hash(41),
        job_id: hash(59),
        attempt: 1,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: 90,
            finalized_block_hash: hash(46),
            finalized_state_root: hash(98),
            finalized_ce_root: hash(99),
            ce_schema_version: 1,
        },
        wwd: 7,
        sealed_tribute_collection_key: hash(30),
        sealed_tribute_collection_root: hash(31),
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        input_chunk_count: 1,
        input_chunk_list_root: hash(100),
        fidelity_opening_root: hash(101),
        oracle_opening_root: hash(102),
        exact_encoded_bytes: 1,
        exact_record_count: 1,
        body_codec_id: hash(103),
        opening_codec_registry_hash: hash(104),
        compression: Compression::None,
    };
    let chunk = outbe_ocomp_protocol::input::AuthenticatedInputChunkV1 {
        protocol_bundle_hash: hash(41),
        job_id: hash(59),
        kind: outbe_ocomp_protocol::input::InputChunkKind::Tribute,
        ordinal: 0,
        canonical_records_or_openings: Vec::new(),
    };
    let unit_spec = UnitSpecV1 {
        protocol_bundle_hash: hash(41),
        job_id: hash(59),
        attempt: 1,
        phase: UnitPhase::Enumerate,
        interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange {
            start: EntityId36([0; 36]),
            end: Some(EntityId36([1; 36])),
        }),
        canonical_ordered_inputs: Vec::new(),
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    let unit_artifact = UnitArtifactV1 {
        protocol_bundle_hash: hash(41),
        job_id: hash(59),
        attempt: 1,
        unit_id: hash(105),
        phase: UnitPhase::Enumerate,
        interval_commitment: hash(106),
        input_root: hash(107),
        output_record_count: 0,
        canonical_output_bytes: BoundedBytes(Vec::new()),
        output_semantic_digest: hash(108),
        coverage_or_permutation_commitment: hash(109),
    };
    let result = result();
    let activation_payload = result.activation_payload(&LIMITS).unwrap();
    let result_digest = activation_payload.result_digest(&LIMITS).unwrap();
    let certificate = certificate(result_digest);
    let snapshot = committee();
    let proof = finality_proof();
    let poc_activation = PoCActivationV1 {
        intent_id: intent().intent_id(&LIMITS).unwrap(),
        finalized_intent_proof: proof.clone(),
        activation_payload: activation_payload.clone(),
        result: result.clone(),
        certificate: certificate.clone(),
    };
    let candidate = CandidateAnnouncementV1 {
        protocol_bundle_hash: result.protocol_bundle_hash,
        job_id: result.job_id,
        attempt: result.attempt,
        result: result.clone(),
        result_digest,
        validator_index: 0,
        key_epoch: 1,
        signature_rs: sign(&signing_key(0), result_digest),
    };
    let sign_once = SignOnceRecordV1 {
        chain_id: 42,
        purpose: SignOncePurpose::ResultSignature,
        job_id: result.job_id,
        attempt: result.attempt,
        protocol_bundle_hash: result.protocol_bundle_hash,
        committee_snapshot_hash: snapshot.snapshot_hash(&LIMITS).unwrap(),
        key_epoch: 1,
        result_digest,
        signature_rs: sign(&signing_key(0), result_digest),
    };
    let activation_call = ActivationCallCoreV1 {
        intent_id: hash(80),
        job_id: result.job_id,
        attempt: result.attempt,
        protocol_bundle_hash: result.protocol_bundle_hash,
        result_digest,
        activation_preconditions_hash: hash(82),
        terminal_pending_nonce: 1,
    };
    let nod_receipt = NodBatchReceiptV1 {
        binding: binding(),
        nod_target_precondition: preconditions().nod,
        nod_count: 0,
        nod_root: result.roots.nod_root,
        nod_amount_total: U256::ZERO,
        nod_gratis_consumed: U256::ZERO,
        issued_at: 1_000,
        state_event_digest: hash(110),
    };
    let contributor_receipt = ContributorReceiptV1 {
        binding: binding(),
        contributor_target_precondition: preconditions().contributors,
        contributor_count: 0,
        contributor_root: result.roots.contributor_root,
        eligible_nominal_total: U256::ZERO,
        state_event_digest: hash(111),
    };
    let tribute_receipt = TributeReceiptV1 {
        binding: binding(),
        tribute_input_binding: preconditions().tribute,
        sealed_collection_root: hash(31),
        consumed_count: 0,
        consumed_nominal_total: U256::ZERO,
        retired_generation: 3,
        state_event_digest: hash(112),
    };
    let request_budget_split_receipt = RequestBudgetSplitReceiptV1 {
        protocol_bundle_hash: hash(41),
        wwd: 7,
        pending_nonce: 1,
        day_type: DayType::Green,
        day_limit: U256::ZERO,
        lysis_budget: U256::ZERO,
        auction_base: U256::ZERO,
        destination: BudgetSplitDestination::DesisAuction,
        desis_brief_hash: Some(hash(113)),
        carry_over_credit: U256::ZERO,
        auction_entry_price: U256::ZERO,
        logical_anchor: 10,
    };
    let carry_over_receipt = CarryOverReceiptV1 {
        binding: binding(),
        source_wwd: 7,
        before_value: U256::ZERO,
        credited_unused_lysis: U256::ZERO,
        after_value: U256::ZERO,
        state_event_digest: hash(115),
    };
    let active_generation = ActiveGenerationV1 {
        job_id: result.job_id,
        program_semantics_hash: hash(8),
        nod_root: result.roots.nod_root,
        bucket_root: result.roots.bucket_root,
        contributor_root: result.roots.contributor_root,
        output_manifest_root: result.roots.output_manifest_root,
        exact_counts: result.counts.clone(),
        result_evidence_hash: hash(116),
        availability_certificate_hash: None,
    };
    let aggregate = conflict_receipt();
    let job_record = OcompJobRecordV1 {
        intent: intent(),
        status: OcompJobStatus::Expired,
        terminal: Some(OcompJobTerminalV1 {
            outcome: OcompTerminalOutcome::Expired,
            terminal_height: 111,
            terminal_time: 1_100,
            next_pending_nonce: Some(2),
            completed_binding: None,
        }),
    };

    assert_round_trip!(bundle(), ProtocolBundleV1, ProtocolBundleV1);
    assert_round_trip!(correctness, CorrectnessProfileV1, CorrectnessProfileV1);
    assert_round_trip!(capacity, CapacityProfileV1, CapacityProfileV1);
    assert_round_trip!(
        pre_admission,
        PreAdmissionEnvelopeV1,
        PreAdmissionEnvelopeV1
    );
    assert_round_trip!(
        preconditions(),
        ActivationPreconditionsV1,
        ActivationPreconditionsV1
    );
    assert_round_trip!(intent(), JobIntentV1, JobIntentV1);
    assert_round_trip!(proof, FinalizedIntentProofV1, FinalizedIntentProofV1);
    assert_round_trip!(manifest, InputManifestV1, InputManifestV1);
    assert_round_trip!(
        chunk,
        outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
        AuthenticatedInputChunkV1
    );
    assert_round_trip!(unit_spec, UnitSpecV1, UnitSpecV1);
    assert_round_trip!(unit_artifact, UnitArtifactV1, UnitArtifactV1);
    let result_chunk = ResultChunkV1 {
        protocol_bundle_hash: result.protocol_bundle_hash,
        job_id: result.job_id,
        attempt: result.attempt,
        chunk_ordinal: 0,
        first_nod_ordinal: 0,
        ordered_nod_actions: Vec::new(),
        ordered_eligible_contributors: Vec::new(),
    };
    assert_round_trip!(result_chunk, ResultChunkV1, ResultChunkV1);
    assert_round_trip!(result.clone(), LysisResultV1, LysisResultV1);
    assert_round_trip!(activation_payload, ActivationPayloadV1, ActivationPayloadV1);
    assert_round_trip!(snapshot, OcompCommitteeSnapshotV1, OcompCommitteeSnapshotV1);
    assert_round_trip!(
        registration(0),
        OcompKeyRegistrationV1,
        OcompKeyRegistrationV1
    );
    assert_round_trip!(certificate, ExecutionCertificateV1, ExecutionCertificateV1);
    assert_round_trip!(poc_activation, PoCActivationV1, PoCActivationV1);
    assert_round_trip!(active_generation, ActiveGenerationV1, ActiveGenerationV1);
    assert_round_trip!(
        aggregate,
        AggregateActivationReceiptV1,
        AggregateActivationReceiptV1
    );
    assert_round_trip!(nod_receipt, NodBatchReceiptV1, NodBatchReceiptV1);
    assert_round_trip!(
        contributor_receipt,
        ContributorReceiptV1,
        ContributorReceiptV1
    );
    assert_round_trip!(tribute_receipt, TributeReceiptV1, TributeReceiptV1);
    assert_round_trip!(
        request_budget_split_receipt,
        RequestBudgetSplitReceiptV1,
        RequestBudgetSplitReceiptV1
    );
    assert_round_trip!(carry_over_receipt, CarryOverReceiptV1, CarryOverReceiptV1);
    assert_round_trip!(candidate, CandidateAnnouncementV1, CandidateAnnouncementV1);
    assert_round_trip!(sign_once, SignOnceRecordV1, SignOnceRecordV1);
    assert_round_trip!(activation_call, ActivationCallCoreV1, ActivationCallCoreV1);
    assert_round_trip!(
        result.arithmetic_summary(),
        LysisArithmeticSummaryV1,
        LysisArithmeticSummaryV1
    );
    assert_round_trip!(job_record, OcompJobRecordV1, OcompJobRecordV1);
}

#[test]
fn committee_certificate_and_signatures_fail_closed() {
    let digest = hash(120);
    let snapshot = committee();
    let certificate = certificate(digest);
    certificate.verify(&snapshot, 100, &LIMITS).unwrap();

    let mut wrong_bitmap = certificate.clone();
    wrong_bitmap.signer_bitmap = 0b1111;
    assert!(matches!(
        wrong_bitmap.validate_shape(),
        Err(ProtocolError::InvalidInvariant(
            "certificate threshold bitmap"
        ))
    ));

    let mut duplicate = certificate.clone();
    duplicate.ordered_signatures[1].validator_index = 0;
    assert!(duplicate.validate_shape().is_err());

    let invalid_key = [0_u8; 33];
    assert_eq!(
        verify_low_s_prehash(&invalid_key, digest, &[0; 64]),
        Err(ProtocolError::InvalidPublicKey)
    );

    let mut high_s = [0_u8; 64];
    high_s[31] = 1;
    high_s[32..].copy_from_slice(&[
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x40,
    ]);
    assert_eq!(
        verify_low_s_prehash(&compressed_key(&signing_key(0)), digest, &high_s),
        Err(ProtocolError::HighSignatureS)
    );
}

#[test]
fn typed_caps_and_semantic_invariants_reject_before_acceptance() {
    let tiny = SchemaLimits {
        codec: CodecLimits::new(1_048_576, 4_096, 2_097_152),
        max_bounded_bytes: 1,
        max_proof_bytes: 1,
        max_opening_bytes: 1,
        max_collection_items: 4,
        max_action_items: 1,
        max_chunk_items: 1,
        max_unit_inputs: 1,
        max_control_body_bytes: 1,
    };
    let mut proof = finality_proof();
    proof.canonical_request_header_rlp = ProofBytes(vec![1, 2]);
    assert!(matches!(
        proof.encode_canonical(&tiny),
        Err(ProtocolError::InvalidInvariant("proof byte field cap"))
    ));

    let mut invalid_intent = intent();
    invalid_intent.attempt = 2;
    assert!(matches!(
        invalid_intent.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "attempt equals checked pending nonce"
        ))
    ));

    let mut invalid_result = result();
    invalid_result.counts.nod_count = 0;
    assert!(matches!(
        invalid_result.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("result exact counts"))
    ));
}

#[test]
fn plan_commitment_scales_the_population_into_bounded_work_units_without_a_total_cap() {
    for (tribute_count, expected_units) in [(10_000, 40), (1_000_000_000, 3_906_250)] {
        let plan = PlanCommitmentV1 {
            protocol_bundle_hash: hash(1),
            job_id: hash(2),
            attempt: 1,
            input_manifest_hash: hash(3),
            tribute_count,
            max_tributes_per_work_shard: 256,
            primary_work_unit_count: expected_units,
            primary_work_unit_root: hash(4),
            planner_spec_version: 1,
            reducer_spec_version: 1,
        };
        assert!(plan.plan_hash(&LIMITS).is_ok());

        let mut missing_unit = plan;
        missing_unit.primary_work_unit_count -= 1;
        assert!(missing_unit.plan_hash(&LIMITS).is_err());
    }

    let prefix_down = UnitSpecV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        phase: UnitPhase::GratisPrefixDown,
        interval: UnitInterval::BinaryReducerNode(BinaryReducerNode {
            level: 21,
            index: 7,
        }),
        canonical_ordered_inputs: Vec::new(),
        lysis_program_semantics_hash: hash(5),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    assert!(prefix_down.unit_id(&LIMITS).is_ok());
}

#[test]
fn split_budget_and_carry_over_invariants_fail_closed() {
    let mut invalid_intent = intent();
    invalid_intent.frozen_metadosis_values.day_limit = U256::from(1);
    assert!(matches!(
        invalid_intent.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("Metadosis budget split"))
    ));

    let green_split = RequestBudgetSplitReceiptV1 {
        protocol_bundle_hash: hash(41),
        wwd: 7,
        pending_nonce: 1,
        day_type: DayType::Green,
        day_limit: U256::from(10),
        lysis_budget: U256::from(4),
        auction_base: U256::from(6),
        destination: BudgetSplitDestination::DesisAuction,
        desis_brief_hash: Some(hash(113)),
        carry_over_credit: U256::ZERO,
        auction_entry_price: U256::from(2),
        logical_anchor: 10,
    };
    green_split.encode_canonical(&LIMITS).unwrap();

    let mut green_to_carry_over = green_split.clone();
    green_to_carry_over.destination = BudgetSplitDestination::CarryOver;
    green_to_carry_over.desis_brief_hash = None;
    green_to_carry_over.carry_over_credit = green_to_carry_over.auction_base;
    assert!(matches!(
        green_to_carry_over.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "request budget split destination"
        ))
    ));

    let mut red_split = green_split;
    red_split.day_type = DayType::Red;
    red_split.destination = BudgetSplitDestination::CarryOver;
    red_split.desis_brief_hash = None;
    red_split.carry_over_credit = red_split.auction_base;
    red_split.encode_canonical(&LIMITS).unwrap();

    let mut red_to_desis = red_split;
    red_to_desis.destination = BudgetSplitDestination::DesisAuction;
    red_to_desis.desis_brief_hash = Some(hash(113));
    red_to_desis.carry_over_credit = U256::ZERO;
    assert!(matches!(
        red_to_desis.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "request budget split destination"
        ))
    ));

    let mut invalid_lysis_conservation = result();
    invalid_lysis_conservation.conservation.day_limit = U256::from(1);
    invalid_lysis_conservation.conservation.lysis_budget = U256::from(1);
    assert!(matches!(
        invalid_lysis_conservation.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("Lysis budget conservation"))
    ));

    let mut invalid_carry_over = result();
    invalid_carry_over.conservation.carry_over_credit = U256::from(1);
    assert!(matches!(
        invalid_carry_over.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("carry-over conservation"))
    ));

    let invalid_receipt = CarryOverReceiptV1 {
        binding: binding(),
        source_wwd: 7,
        before_value: U256::from(3),
        credited_unused_lysis: U256::from(4),
        after_value: U256::from(8),
        state_event_digest: hash(115),
    };
    assert!(matches!(
        invalid_receipt.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant("carry-over receipt"))
    ));

    let mut partial_conflict = conflict_receipt();
    partial_conflict.nod_receipt_hash = Some(hash(110));
    assert!(matches!(
        partial_conflict.encode_canonical(&LIMITS),
        Err(ProtocolError::InvalidInvariant(
            "aggregate receipt outcome shape"
        ))
    ));
}

#[test]
fn desis_request_brief_hash_commits_every_frozen_request_field() {
    let protocol_bundle_hash = hash(41);
    let wwd = 7_u32;
    let auction_base = U256::from(6);
    let auction_entry_price = U256::from(2);
    let logical_anchor = 10_u64;

    let digest = desis_request_brief_hash(
        protocol_bundle_hash,
        wwd,
        auction_base,
        auction_entry_price,
        logical_anchor,
    )
    .unwrap();
    let mut preimage = Vec::with_capacity(108);
    preimage.extend_from_slice(protocol_bundle_hash.as_slice());
    preimage.extend_from_slice(&wwd.to_be_bytes());
    preimage.extend_from_slice(&auction_base.to_be_bytes::<32>());
    preimage.extend_from_slice(&auction_entry_price.to_be_bytes::<32>());
    preimage.extend_from_slice(&logical_anchor.to_be_bytes());
    assert_eq!(
        digest,
        hash_framed(HashDomain::DesisRequestBrief, &preimage).unwrap()
    );

    for changed in [
        desis_request_brief_hash(
            hash(42),
            wwd,
            auction_base,
            auction_entry_price,
            logical_anchor,
        )
        .unwrap(),
        desis_request_brief_hash(
            protocol_bundle_hash,
            wwd + 1,
            auction_base,
            auction_entry_price,
            logical_anchor,
        )
        .unwrap(),
        desis_request_brief_hash(
            protocol_bundle_hash,
            wwd,
            auction_base + U256::from(1),
            auction_entry_price,
            logical_anchor,
        )
        .unwrap(),
        desis_request_brief_hash(
            protocol_bundle_hash,
            wwd,
            auction_base,
            auction_entry_price + U256::from(1),
            logical_anchor,
        )
        .unwrap(),
        desis_request_brief_hash(
            protocol_bundle_hash,
            wwd,
            auction_base,
            auction_entry_price,
            logical_anchor + 1,
        )
        .unwrap(),
    ] {
        assert_ne!(changed, digest);
    }
}

#[test]
fn local_control_frame_checks_cap_magic_length_and_worker_shape() {
    let frame = ControlFrameV1 {
        magic: ControlMagic::Worker,
        message_kind: 0x0010,
        session_generation: 7,
        request_id: 9,
        body: vec![1, 2, 3],
    };
    let encoded = frame.encode(&LIMITS).unwrap();
    assert_eq!(encoded.len(), CONTROL_FRAME_HEADER_LEN + 3);
    assert_eq!(&encoded[4..8], &WORKER_CONTROL_MAGIC);
    assert_eq!(
        ControlFrameV1::decode(&encoded, ControlMagic::Worker, &LIMITS).unwrap(),
        frame
    );
    assert!(ControlFrameV1::decode(&encoded, ControlMagic::Node, &LIMITS).is_err());

    let mut malformed = encoded.clone();
    malformed[3] = malformed[3].saturating_add(1);
    assert!(ControlFrameV1::decode(&malformed, ControlMagic::Worker, &LIMITS).is_err());

    let tiny = SchemaLimits {
        codec: LIMITS.codec,
        max_bounded_bytes: 16,
        max_proof_bytes: 16,
        max_opening_bytes: 16,
        max_collection_items: 16,
        max_action_items: 16,
        max_chunk_items: 16,
        max_unit_inputs: 16,
        max_control_body_bytes: 2,
    };
    assert!(matches!(
        ControlFrameV1::decode(&encoded, ControlMagic::Worker, &tiny),
        Err(ProtocolError::CapacityExceeded {
            what: "control frame bytes",
            ..
        })
    ));

    let request = RunUnitV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 1,
        plan_hash: hash(3),
        unit_index: 0,
        canonical_unit_spec: BoundedBytes(vec![1]),
        plan_ref: outbe_ocomp_protocol::control::CasObjectRefV1 {
            transport_digest: hash(4),
            encoded_bytes: 10,
            expected_ocb1_kind: None,
        },
        input_manifest_ref: outbe_ocomp_protocol::control::CasObjectRefV1 {
            transport_digest: hash(5),
            encoded_bytes: 20,
            expected_ocb1_kind: Some(ObjectKind::InputManifestV1.tag()),
        },
        ordered_input_refs: Vec::new(),
    };
    let body = request.encode_body(&LIMITS).unwrap();
    assert_eq!(RunUnitV1::decode_body(&body, &LIMITS).unwrap(), request);
}

#[test]
fn finalized_discovery_control_is_one_job_bounded_and_canonical() {
    let request = ListFinalizedJobsV1 {
        after_cursor: 99,
        limit: 1,
    };
    assert_eq!(
        ListFinalizedJobsV1::decode_body(&request.encode_body(&LIMITS).unwrap(), &LIMITS).unwrap(),
        request
    );
    for invalid_limit in [0, MAX_FINALIZED_JOBS_PER_RESPONSE + 1] {
        assert!(ListFinalizedJobsV1 {
            after_cursor: 99,
            limit: invalid_limit,
        }
        .encode_body(&LIMITS)
        .is_err());
    }

    let intent = intent();
    let finalized_block_hash = hash(0x92);
    let finalized_state_root = hash(0x93);
    let summary = FinalizedJobSummaryV1 {
        cursor: 100,
        job_id: intent
            .job_id(finalized_block_hash, finalized_state_root, &LIMITS)
            .expect("JobId"),
        intent_id: intent.intent_id(&LIMITS).expect("IntentId"),
        finalized_block_hash,
        finalized_state_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
    };
    let response = ListFinalizedJobsResponseV1 {
        next_cursor: summary.cursor,
        jobs: vec![summary.clone()],
    };
    assert_eq!(
        ListFinalizedJobsResponseV1::decode_body(&response.encode_body(&LIMITS).unwrap(), &LIMITS)
            .unwrap(),
        response
    );
    assert!(ListFinalizedJobsResponseV1 {
        next_cursor: 101,
        jobs: vec![summary.clone(), summary.clone()],
    }
    .encode_body(&LIMITS)
    .is_err());

    let get = GetJobSpecV1 {
        job_id: summary.job_id,
    };
    assert_eq!(
        GetJobSpecV1::decode_body(&get.encode_body(&LIMITS).unwrap(), &LIMITS).unwrap(),
        get
    );
    let spec = FinalizedJobSpecV1 {
        summary,
        canonical_job_intent: BoundedBytes(
            intent.encode_canonical(&LIMITS).expect("canonical intent"),
        ),
    };
    assert_eq!(
        FinalizedJobSpecV1::decode_body(&spec.encode_body(&LIMITS).unwrap(), &LIMITS).unwrap(),
        spec
    );
}

fn selector(signature: &str) -> [u8; 4] {
    keccak256(signature.as_bytes()).0[..4].try_into().unwrap()
}

#[test]
fn public_abi_constants_match_independent_keccak_derivation() {
    assert_eq!(selector("activateLysis(bytes)"), ACTIVATE_LYSIS_SELECTOR);
    assert_eq!(
        selector("getOffchainJob(bytes32)"),
        GET_OFFCHAIN_JOB_SELECTOR
    );
    assert_eq!(
        selector("getActiveLysisGeneration(uint32)"),
        GET_ACTIVE_LYSIS_GENERATION_SELECTOR
    );
    assert_eq!(
        selector("getLysisTerminalReceipt(bytes32)"),
        GET_LYSIS_TERMINAL_RECEIPT_SELECTOR
    );
    assert_eq!(
        selector("OcompActivationRejected(uint16)"),
        OCOMP_ACTIVATION_REJECTED_SELECTOR
    );
    assert_eq!(
        keccak256(b"OffchainJobRequested(bytes32,uint32,uint64,uint32,uint64,bytes32)"),
        OCOMP_REQUESTED_TOPIC0
    );
    assert_eq!(
        keccak256(b"OffchainJobExpired(bytes32,uint32,uint64,uint64,uint64)"),
        OCOMP_EXPIRED_TOPIC0
    );
    assert_eq!(
        keccak256(b"OffchainJobConflicted(bytes32,bytes32,uint32,uint64,uint64,bytes32)"),
        OCOMP_CONFLICTED_TOPIC0
    );
    assert_eq!(
        keccak256(b"LysisActivated(bytes32,bytes32,bytes32,bytes32,bytes32,uint32)"),
        LYSIS_ACTIVATED_TOPIC0
    );
}

#[test]
fn address_is_exactly_twenty_bytes_in_typed_actions() {
    assert_eq!(core::mem::size_of::<Address>(), 20);
}
