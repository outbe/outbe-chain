use super::*;

pub(super) fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

pub(super) fn tribute_collection_key() -> B256 {
    let (_, key) = partition_collection_key(PartitionRef::TributeWwd(TEST_WWD)).unwrap();
    B256::from(*key.as_bytes())
}

pub(super) fn capacity_profile() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: hash(13),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        ready_backoff_blocks: 1,
        max_reference_currencies: 256,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks: outbe_chain_constants::DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: hash(23),
    }
}

pub(super) fn bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(21),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: outbe_ocomp_protocol::registry::TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: outbe_ocomp_protocol::registry::FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: outbe_ocomp_protocol::registry::ORACLE_OPENING_CODEC_ID,
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
        release_gate_authority_envelope_hash: hash(22),
        release_approval_policy_hash: hash(24),
        release_validator_command_artifact_hash: hash(25),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(26),
        required_upgrade_handler_set_hash: hash(27),
    }
}

/// Builds a fully valid immutable fork-install artifact for behavioral tests.
///
/// This creates only canonical manifest bytes. It does not seed chain state or
/// bypass the production lifecycle.
pub fn fork_install_fixture(
    classification: OcompForkInstallClassification,
    activation_height: u64,
    chain_id: u64,
    genesis_hash: B256,
) -> OcompForkInstallV1 {
    let limits = poc_schema_limits();
    let protocol_bundle = bundle();
    let bundle_hash = protocol_bundle.protocol_bundle_hash(&limits).unwrap();
    let founder_key = signing_key(0);
    let mut founder_registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id,
            genesis_hash,
            validator_identity_hash: validator_identity_hash_v1(
                Address::repeat_byte(0xB0),
                &[0x30; 48],
            )
            .unwrap(),
            ocomp_public_key_sec1: founder_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
            key_epoch: 1,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    founder_registration.proof_of_possession = sign(
        &founder_key,
        founder_registration
            .proof_of_possession_digest(&limits)
            .unwrap(),
    );
    OcompForkInstallV1 {
        classification,
        activation_height,
        request_profile: OcompRequestProfile {
            chain_id,
            genesis_hash,
            fork_id: hash(21),
            protocol_bundle_hash: bundle_hash,
            correctness_profile_id: hash(12),
            capacity_profile: capacity_profile(),
            source_availability_policy_id: hash(44),
        },
        protocol_bundle,
        founder_registrations: vec![founder_registration],
    }
}

/// A request profile and protocol bundle that agree with each other on `chain_id`.
#[cfg(test)]
pub fn fixture_authority(chain_id: u64) -> outbe_ocompregistry::OcompProtocolAuthorityV1 {
    let install = fork_install_fixture(
        OcompForkInstallClassification::Measurement,
        1,
        chain_id,
        hash(17),
    );
    outbe_ocompregistry::OcompProtocolAuthorityV1 {
        request_profile: install.request_profile,
        protocol_bundle: install.protocol_bundle,
    }
}

/// Leaves `OcompRegistry` holding `authority` the way the genesis install does,
/// without that install's activation-height and chain-identity gates.
pub fn seed_registry_authority(
    storage: &StorageHandle<'_>,
    authority: &outbe_ocompregistry::OcompProtocolAuthorityV1,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> PrecompileResult<()> {
    let registry = outbe_ocompregistry::OcompRegistry::new(storage.clone());
    registry
        .active_request_profile
        .write(&authority.request_profile.encode_canonical(limits)?)?;
    registry.active_protocol_bundle.write(
        &authority
            .protocol_bundle
            .encode_canonical(limits)
            .map_err(|error| PrecompileError::Fatal(error.to_string()))?,
    )?;
    registry
        .active_protocol_bundle_hash
        .write(authority.request_profile.protocol_bundle_hash)?;
    if registry.active_authority(limits)?.as_ref() != Some(authority) {
        return Err(PrecompileError::Fatal(
            "seeded OCOMP Registry authority does not read back".into(),
        ));
    }
    Ok(())
}

pub(super) fn request_receipt(bundle_hash: B256, logical_time: u64) -> RequestLimitSplitReceiptV1 {
    RequestLimitSplitReceiptV1 {
        protocol_bundle_hash: bundle_hash,
        wwd: TEST_WWD.value(),
        pending_nonce: 0,
        day_type: DayType::Green,
        day_limit: U256::from(100),
        lysis_limit_minor: U256::from(60),
        desis_limit_minor: U256::from(40),
        destination: LimitSplitDestination::DesisAuction,
        desis_brief_hash: Some(
            desis_request_brief_hash(bundle_hash, TEST_WWD.value(), U256::from(40), logical_time)
                .unwrap(),
        ),
        carry_over_credit: U256::ZERO,
        logical_anchor: logical_time,
    }
}

pub(super) fn intent(
    bundle_hash: B256,
    snapshot: &OcompSnapshotExtensionV1,
    request_receipt_hash: B256,
    logical_time: u64,
) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: 1,
        genesis_hash: hash(17),
        fork_id: hash(21),
        wwd: TEST_WWD.value(),
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: bundle_hash,
        ce_sealed_root: hash(42),
        sealed_tribute_collection_key: tribute_collection_key(),
        sealed_tribute_collection_root: hash(31),
        authenticated_day_count: 2,
        authenticated_day_nominal: U256::from(1_000),
        pre_admission_envelope_hash: hash(43),
        source_availability_policy_id: hash(44),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(100),
            previous_vwap: U256::from(8),
            current_vwap: U256::from(10),
            gratis_demand: U256::from(60),
            day_gratis_limit_minor: U256::from(60),
            lysis_limit_minor: U256::from(60),
            desis_limit_minor: U256::from(40),
            request_limit_split_receipt_hash: request_receipt_hash,
        },
        logical_evaluation_height: TEST_REQUEST_HEIGHT,
        logical_evaluation_time: logical_time,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: TEST_WWD.value(),
                source_generation: 0,
                collection_key: tribute_collection_key(),
                sealed_collection_root: hash(31),
                exact_count: 2,
                exact_nominal_total: U256::from(1_000),
            },
            nod: NodTargetPreconditionV1 {
                wwd: TEST_WWD.value(),
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 2,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: TEST_WWD.value(),
                expected_series_version: 0,
                max_contributor_count: 2,
                max_eligible_nominal_total: U256::from(1_000),
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: TEST_WWD.value(),
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_validator_set_epoch: snapshot.epoch,
        result_committee_set_hash: snapshot.committee_set_hash,
        result_ocomp_binding_hash: snapshot.ocomp_binding_hash,
        result_member_count: snapshot.member_count,
        result_quorum_threshold: u16::try_from(outbe_consensus::proof::simplex_n3f1_quorum(
            usize::from(snapshot.member_count),
        ))
        .unwrap(),
        custody_committee_epoch_hash: None,
    }
}

pub(super) fn result(
    bundle_hash: B256,
    job_id: B256,
    limits: &SchemaLimits,
    logical_time: u64,
) -> LysisResultV1 {
    let roots = ResultRootsV1 {
        nod_root: hash(50),
        bucket_root: hash(51),
        contributor_root: hash(52),
        output_manifest_root: hash(53),
    };
    let counts = ExactCountsV1 {
        tribute_count: 2,
        nod_count: 2,
        bucket_count: 1,
        contributor_count: 1,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::from(1_000),
        eligible_nominal_total: U256::from(600),
        day_limit: U256::from(100),
        gratis_demand: U256::from(60),
        day_gratis_limit_minor: U256::from(60),
        lysis_limit_minor: U256::from(60),
        desis_limit_minor: U256::from(40),
        lysis_allocation_minor: U256::from(45),
        unused_lysis_limit_minor: U256::from(15),
        carry_over_credit: U256::from(15),
        nod_cost_total: U256::from(300),
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
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: 0,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 2,
        result_chunk_list_root: hash(61),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: TEST_WWD.value(),
            reason: CarryOverReason::UnusedLysis,
            amount: U256::from(15),
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: TEST_WWD.value(),
            pending_nonce: 0,
            day_type: DayType::Green,
            tribute_nominal_total: U256::from(1_000),
            day_limit: U256::from(100),
            gratis_demand: U256::from(60),
            day_gratis_limit_minor: U256::from(60),
            lysis_limit_minor: U256::from(60),
            desis_limit_minor: U256::from(40),
            lysis_allocation_minor: U256::from(45),
            unused_lysis_limit_minor: U256::from(15),
            carry_over_credit: U256::from(15),
            status: CompletionStatus::Completed,
            logical_evaluation_height: TEST_REQUEST_HEIGHT,
            logical_evaluation_time: logical_time,
        },
        tribute_count: 2,
        tribute_nominal_total: U256::from(1_000),
        unused_lysis_limit_minor: U256::from(15),
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(limits).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    }
}

/// Builds one production-valid, bounded result for an already persisted test
/// intent. The result keeps every scalar bound anchored to the intent while
/// using deterministic non-zero commitments for the off-chain artifacts.
#[must_use]
pub fn lysis_result_for_intent(
    intent: &JobIntentV1,
    job_id: B256,
    limits: &SchemaLimits,
) -> LysisResultV1 {
    let frozen = &intent.frozen_metadosis_values;
    let roots = ResultRootsV1 {
        nod_root: hash(150),
        bucket_root: hash(151),
        contributor_root: hash(152),
        output_manifest_root: hash(153),
    };
    let counts = ExactCountsV1 {
        tribute_count: intent.authenticated_day_count,
        nod_count: intent.authenticated_day_count,
        bucket_count: u32::from(intent.authenticated_day_count > 0),
        contributor_count: 0,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: intent.authenticated_day_nominal,
        eligible_nominal_total: U256::ZERO,
        day_limit: frozen.day_limit,
        gratis_demand: frozen.gratis_demand,
        day_gratis_limit_minor: frozen.day_gratis_limit_minor,
        lysis_limit_minor: frozen.lysis_limit_minor,
        desis_limit_minor: frozen.desis_limit_minor,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: frozen.lysis_limit_minor,
        carry_over_credit: frozen.lysis_limit_minor,
        nod_cost_total: U256::ZERO,
    };
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: hash(154),
        plan_hash: hash(155),
        unit_artifact_root: hash(156),
        fidelity_fraction_root: hash(157),
        gratis_prefix_root: hash(158),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    let result = LysisResultV1 {
        protocol_bundle_hash: intent.protocol_bundle_hash,
        job_id,
        attempt: intent.attempt,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: hash(159),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: intent.wwd,
            reason: CarryOverReason::UnusedLysis,
            amount: frozen.lysis_limit_minor,
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: intent.wwd,
            pending_nonce: intent.pending_nonce,
            day_type: frozen.day_type,
            tribute_nominal_total: intent.authenticated_day_nominal,
            day_limit: frozen.day_limit,
            gratis_demand: frozen.gratis_demand,
            day_gratis_limit_minor: frozen.day_gratis_limit_minor,
            lysis_limit_minor: frozen.lysis_limit_minor,
            desis_limit_minor: frozen.desis_limit_minor,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: frozen.lysis_limit_minor,
            carry_over_credit: frozen.lysis_limit_minor,
            status: CompletionStatus::Completed,
            logical_evaluation_height: intent.logical_evaluation_height,
            logical_evaluation_time: intent.logical_evaluation_time,
        },
        tribute_count: intent.authenticated_day_count,
        tribute_nominal_total: intent.authenticated_day_nominal,
        unused_lysis_limit_minor: frozen.lysis_limit_minor,
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(limits).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    };
    result.validate_finalized_intent(intent).unwrap();
    result.validate_semantics(limits).unwrap();
    result
}

pub(super) fn finality_proof(
    intent: &JobIntentV1,
    limits: &SchemaLimits,
) -> FinalizedIntentProofV1 {
    FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(vec![1, 2]),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: 9,
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
        canonical_job_intent: BoundedBytes(intent.encode_canonical(limits).unwrap()),
        intent_account_proof: ProofBytes(vec![4]),
        intent_storage_proof: ProofBytes(vec![5]),
    }
}

#[derive(Clone)]
pub struct FixedFinality {
    pub(super) expected: ExpectedFinalizedIntentBindingV1,
    pub(super) verified: VerifiedFinalizedIntentV1,
    pub(super) calls: Arc<AtomicUsize>,
}

impl OcompFinalizedIntentAuthority for FixedFinality {
    fn verify(
        &self,
        _proof: &FinalizedIntentProofV1,
        expected: ExpectedFinalizedIntentBindingV1,
        _limits: &SchemaLimits,
    ) -> Result<VerifiedFinalizedIntentV1, OcompFinalityAuthorityError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if expected != self.expected {
            return Err(FinalizedIntentVerificationError::WrongProtocolBundle.into());
        }
        Ok(self.verified.clone())
    }
}
