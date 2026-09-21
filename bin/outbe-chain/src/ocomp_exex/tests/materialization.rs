use super::*;

fn materialization_head(
    last_progress_height: u64,
) -> outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1 {
    outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: B256::repeat_byte(0x11),
        program_semantics_hash: B256::repeat_byte(0x22),
        worldwide_day: 20_260_812,
        generation: 1,
        nod_root: B256::repeat_byte(0x33),
        nod_count: 10,
        next_nod_ordinal: 8,
        last_progress_height,
    }
}

#[test]
fn finalized_materialization_wake_uses_the_authenticated_system_tx_signer() {
    use outbe_primitives::{
        signer::OutbeEvmSigner,
        system_tx::{build_unsigned_system_tx, SystemTxInputV2},
    };

    let signer = OutbeEvmSigner::from_secret_bytes([0x41; 32]).expect("test signer");
    let input = SystemTxInputV2::CycleTick;
    let unsigned = build_unsigned_system_tx(
        input.kind(),
        0,
        2,
        outbe_primitives::chain::CHAIN_ID,
        input.encode().expect("canonical system input"),
    )
    .expect("unsigned system transaction");
    let signed = signer
        .sign_unsigned(unsigned)
        .expect("signed system transaction");

    let transactions = [signed];
    let proposer = authenticated_finalized_proposer(transactions.iter())
        .expect("recover authenticated finalized proposer");
    assert_eq!(proposer, signer.address());
    assert_ne!(
        proposer,
        outbe_primitives::addresses::REWARDS_ADDRESS,
        "the protocol reward beneficiary is not proposer identity"
    );
    assert!(should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(signer.address()),
        proposer,
        100,
        Some(&materialization_head(90)),
        true,
        30,
    ));
}

#[test]
fn genesis_without_system_transactions_has_no_materialization_proposer() {
    let transactions: Vec<outbe_primitives::OutbeTxEnvelope> = Vec::new();
    assert_eq!(
        finalized_materialization_proposer(0, transactions.iter())
            .expect("genesis has no materialization proposer"),
        None
    );
}

#[test]
fn only_the_validator_representing_the_finalized_proposer_wakes_materialization() {
    let represented = alloy_primitives::Address::repeat_byte(0x11);
    let other = alloy_primitives::Address::repeat_byte(0x22);
    let head = materialization_head(90);

    assert!(should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        100,
        Some(&head),
        true,
        30,
    ));
    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        other,
        100,
        Some(&head),
        true,
        30,
    ));
    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::FullNode,
        None,
        represented,
        100,
        Some(&head),
        true,
        30,
    ));
}

#[test]
fn retry_wake_uses_the_resolved_interval_and_requires_an_incomplete_head() {
    let represented = alloy_primitives::Address::repeat_byte(0x11);
    let head = materialization_head(90);

    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        119,
        Some(&head),
        false,
        30,
    ));
    assert!(should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        120,
        Some(&head),
        false,
        30,
    ));
    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        120,
        None,
        true,
        30,
    ));
}

#[test]
fn materialization_retry_memory_tracks_only_the_current_head() {
    let old = MaterializationAttemptKeyV1 {
        queue_sequence: 1,
        first_nod_ordinal: 0,
    };
    let current = MaterializationAttemptKeyV1 {
        queue_sequence: 2,
        first_nod_ordinal: 8,
    };
    let mut attempts = BTreeMap::from([(old, 10), (current, 20)]);
    bound_materialization_attempts(&mut attempts, Some(current));
    assert_eq!(attempts, BTreeMap::from([(current, 20)]));
    bound_materialization_attempts(&mut attempts, None);
    assert!(attempts.is_empty());
}

#[test]
fn pre_finalization_request_blocks_checkpoint_until_job_materializes() {
    let intent_id = B256::repeat_byte(0x41);
    let mut materialized = std::collections::BTreeSet::new();
    assert!(!all_requests_materialized(
        [intent_id].into_iter(),
        &materialized
    ));
    materialized.insert(intent_id);
    assert!(all_requests_materialized(
        [intent_id].into_iter(),
        &materialized
    ));
}

#[cfg(test)]
fn all_requests_materialized(
    request_ids: impl IntoIterator<Item = B256>,
    materialized_requests: &BTreeSet<B256>,
) -> bool {
    request_ids
        .into_iter()
        .all(|intent_id| materialized_requests.contains(&intent_id))
}

// Reuses the existing native proof fixture shape from snapshot tests; not worker execution.
mod copied_public_work {
    use super::super::recovery::copied_native;
    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
    use outbe_lysis::program_v1::{
        planner::{
            LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedUnitPositionV1,
        },
        result::{
            encode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1,
            RootReduceSummaryV1,
        },
    };
    use outbe_ocomp::nod_materialization::build_nod_materialization_batch_with_references;
    use outbe_ocomp::{
        admission_catalog::{
            AdmissionCatalogReader, AdmissionPositionV1, VerifiedAdmissionCatalog,
        },
        bundle::PinnedProtocolBundle,
        cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
        control::poc_schema_limits,
        input_artifacts::{
            poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
            InputArtifactIdentity,
        },
        input_ref_catalog::VerifiedInputChunkRefCatalog,
        lysis_plan_audit::LocalLysisPlanAuditV1,
    };
    use outbe_ocomp_protocol::{
        common::{BoundedBytes, ProofBytes},
        input::{
            materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind,
            InputManifestV1,
        },
        opening::{
            partition_lysis_opening_subjects, LysisOpeningsProofV1, RawContractOpeningProofV1,
            RawStorageSlotV1,
        },
        registry::{
            ObjectKind, FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID,
        },
        result::{ContributorActionV1, NodActionV1, OutputManifestEntryV1, ResultChunkV1},
        unit::{UnitArtifactV1, UnitPhase, WorkOutputHeaderV1},
        ListKind,
    };
    use outbe_ocomp_protocol::{
        nod_materialization::NodMaterializationHeadV1, profile::ProtocolBundleV1, CasObjectRefV1,
        StreamingOrderedListRoot,
    };
    use outbe_primitives::time::WorldwideDay;
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    const CAS_LIMITS: CasLimits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: 64 * 1_048_576,
    };
    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }
    struct Fixture {
        job_id: B256,
        day: WorldwideDay,
        bundle: PinnedProtocolBundle,
        nod_root: B256,
        bucket_root: B256,
        output_manifest_root: B256,
        nod_count: u32,
        result_chunk_refs: Vec<CasObjectRefV1>,
    }
    // Protocol-shaped native CAS/planner fixture. Minimal non-root phase payloads
    // support structural/proof tests; this is not real worker-pipeline E2E evidence.
    fn protocol_bundle() -> ProtocolBundleV1 {
        ProtocolBundleV1 {
            protocol_version: 1,
            fork_id: B256::repeat_byte(1),
            intent_codec_id: B256::repeat_byte(2),
            finalized_intent_proof_codec_id: B256::repeat_byte(3),
            tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
            fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
            oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
            result_codec_id: B256::repeat_byte(4),
            action_codec_id: B256::repeat_byte(5),
            activation_codec_id: B256::repeat_byte(6),
            evidence_codec_id: B256::repeat_byte(7),
            request_semantics_version: 1,
            lysis_program_semantics_hash: B256::repeat_byte(8),
            planner_spec_version: 1,
            reducer_spec_version: 1,
            activation_apply_semantics_hash: B256::repeat_byte(9),
            effect_contract_registry_hash: B256::repeat_byte(10),
            object_codec_registry_hash: B256::repeat_byte(11),
            correctness_profile_id: B256::repeat_byte(12),
            capacity_profile_id: B256::repeat_byte(13),
            result_signature_profile_id: B256::repeat_byte(14),
            finality_verifier_and_vote_domain_id: B256::repeat_byte(15),
            consensus_committee_history_schema_version: 1,
            ocomp_committee_schema_version: 1,
            proof_system_and_verifier_key_id: None,
            da_codec_and_binding_verifier_id: None,
            anti_equivocation_journal_schema_hash: B256::repeat_byte(16),
            mode_pause_revocation_semantics_hash: B256::repeat_byte(17),
            upgrade_fsm_semantics_hash: B256::repeat_byte(18),
            release_requirement_catalog_sequence: 1,
            release_requirement_catalog_hash: B256::repeat_byte(19),
            release_requirement_catalog_parent_hash: B256::repeat_byte(20),
            release_gate_authority_envelope_hash: B256::repeat_byte(21),
            release_approval_policy_hash: B256::repeat_byte(22),
            release_validator_command_artifact_hash: B256::repeat_byte(23),
            consensus_state_schema_version: 1,
            migration_manifest_hash: B256::repeat_byte(24),
            required_upgrade_handler_set_hash: B256::repeat_byte(25),
        }
    }
    fn fixture(root: &Path, job_seed: u8, day: WorldwideDay, tribute_count: u32) -> Fixture {
        let limits = poc_schema_limits();
        let list_limits = poc_input_list_limits();
        let bundle = protocol_bundle();
        let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let pinned_bundle = PinnedProtocolBundle::decode(
            &bundle.encode_canonical(&limits).unwrap(),
            bundle_hash,
            &limits,
        )
        .unwrap();
        let job_id = hash(job_seed);

        let mut tributes = (0..tribute_count)
            .map(|index| {
                let mut owner_bytes = [0_u8; 20];
                owner_bytes[16..].copy_from_slice(&(index + 1).to_be_bytes());
                let owner = Address::from(owner_bytes);
                TributeBodyV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_amount_minor: U256::from(1),
                    issuance_currency: if index % 2 == 0 { 840 } else { 826 },
                    nominal_amount_minor: U256::from((index % 7) + 1),
                    reference_currency: if index % 3 == 0 { 978 } else { 392 },
                    tribute_price_minor: U256::from(1),
                    exclude_from_intex_issuance: false,
                }
            })
            .collect::<Vec<_>>();
        tributes.sort_by_key(|tribute| tribute.tribute_id);
        let mut contributors_by_owner = tributes
            .iter()
            .map(|tribute| ContributorActionV1 {
                owner: tribute.owner,
                source_tribute_id: *tribute.tribute_id,
                nominal_amount_minor: tribute.nominal_amount_minor,
            })
            .collect::<Vec<_>>();
        contributors_by_owner
            .sort_by_key(|contributor| (contributor.owner, contributor.source_tribute_id));
        let nod_action_tributes = tributes.clone();
        let owners = tributes
            .iter()
            .map(|tribute| tribute.owner)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut reference_isos = tributes
            .iter()
            .map(|tribute| tribute.reference_currency)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        reference_isos.push(840);
        reference_isos.sort_unstable();
        reference_isos.dedup();
        let finalized_state_root = hash(0x32);
        let raw_opening = |address, slot_byte| RawContractOpeningProofV1 {
            contract_address: address,
            state_root: finalized_state_root,
            ordered_slots: vec![RawStorageSlotV1 {
                slot: hash(slot_byte),
                value: U256::from(1),
            }],
            account_proof: ProofBytes(vec![0xa1]),
            storage_proof: ProofBytes(vec![0xb1]),
        };
        let mut fidelity_openings = Vec::new();
        let mut oracle_opening = None;
        for subjects in partition_lysis_opening_subjects(&owners, &reference_isos, &limits).unwrap()
        {
            let openings = materialize_authenticated_openings(
                &LysisOpeningsProofV1 {
                    protocol_bundle_hash: bundle_hash,
                    job_id,
                    finalized_block_hash: hash(0x31),
                    finalized_state_root,
                    wwd: day.value(),
                    subjects,
                    fidelity: raw_opening(Address::repeat_byte(0x63), 0x64),
                    oracle: raw_opening(Address::repeat_byte(0x65), 0x66),
                },
                &bundle,
                &limits,
            )
            .unwrap();
            fidelity_openings.push(openings.fidelity);
            match &oracle_opening {
                None => oracle_opening = Some(openings.oracle),
                Some(existing) => assert_eq!(existing, &openings.oracle),
            }
        }

        let job = hex::encode(job_id);
        let cas_root = root.join("cas-v1");
        let input_ref_root = root.join("exporter-v1/input-refs").join(&job);
        let admission_root = root
            .join("supervisor-v1/jobs")
            .join(&job)
            .join("admissions");
        let bundle_dir = root.join("protocol-bundles-v1");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(
            bundle_dir.join(format!("{}.ocb1", hex::encode(bundle_hash))),
            bundle.encode_canonical(&limits).unwrap(),
        )
        .unwrap();
        let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
        let published = publish_input_artifact_set(
            &cas,
            &input_ref_root,
            &bundle,
            InputArtifactContents {
                identity: InputArtifactIdentity {
                    job_id,
                    attempt: 0,
                    checkpoint: CheckpointIdentityV1 {
                        finalized_block_number: 90,
                        finalized_block_hash: hash(0x31),
                        finalized_state_root,
                        finalized_ce_root: hash(0x33),
                        ce_schema_version: 1,
                    },
                    wwd: day.value(),
                    sealed_tribute_collection_key: hash(0x34),
                    sealed_tribute_collection_root: hash(0x35),
                },
                canonical_tributes: tributes
                    .iter()
                    .map(|tribute| encode_tribute_v1(tribute).unwrap())
                    .collect(),
                fidelity_openings,
                oracle_opening: oracle_opening.unwrap(),
            },
            &limits,
            list_limits,
        )
        .unwrap();
        let manifest = InputManifestV1::decode_canonical(
            cas.read_verified(&published.manifest_ref).unwrap().bytes(),
            &limits,
        )
        .unwrap();
        let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
            &input_ref_root,
            &cas,
            &published.manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        let all_input_refs = input_refs_for_plan
            .exact_cursor()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let tribute_refs = all_input_refs
            .iter()
            .filter(|reference| reference.kind == InputChunkKind::Tribute)
            .cloned()
            .collect::<Vec<_>>();
        drop(input_refs_for_plan);
        let manifest_ref = published.manifest_ref;

        let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            attempt: 0,
            input_manifest_hash: manifest.manifest_hash(&limits).unwrap(),
            input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
            fidelity_opening_root: manifest.fidelity_opening_root,
            oracle_opening_root: manifest.oracle_opening_root,
            wwd: manifest.wwd,
            lysis_limit_minor: U256::from(200),
            logical_evaluation_time: 1_784_765_900,
            tribute_count: manifest.tribute_count,
            lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
            planner_spec_version: bundle.planner_spec_version,
            reducer_spec_version: bundle.reducer_spec_version,
        })
        .unwrap();
        let plan = planner
            .commit_primary_catalog(tribute_refs.clone(), &limits)
            .unwrap();
        let plan_ref = cas
            .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
            .unwrap();

        let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
        let input_refs =
            VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
                .unwrap();
        let mut admissions =
            VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
                .unwrap();
        let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
        let plan_hash = plan.plan_hash(&limits).unwrap();
        let mut nod_root =
            StreamingOrderedListRoot::new(ListKind::NodActions, tribute_count).unwrap();
        let mut result_chunk_refs = Vec::new();
        let mut output_manifest_root = StreamingOrderedListRoot::new(
            ListKind::CompleteOutputManifest,
            plan.primary_work_unit_count,
        )
        .unwrap();
        let mut bucket_root =
            StreamingOrderedListRoot::new(ListKind::BucketRecords, tribute_count).unwrap();

        for plan_ordinal in 0..topology.total_unit_count() {
            let spec = {
                let audit = LocalLysisPlanAuditV1::open(
                    &admissions,
                    &input_refs,
                    &reader,
                    &pinned_bundle,
                    &limits,
                )
                .unwrap();
                audit.candidate_spec_at(plan_ordinal).unwrap()
            };
            let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
                PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    level: 0,
                    index,
                } => {
                    let start = usize::try_from(index * 256).unwrap();
                    let end = (start + 256).min(tributes.len());
                    let actions = nod_action_tributes[start..end]
                        .iter()
                        .enumerate()
                        .map(|(local, tribute)| {
                            let tribute_id = *tribute.tribute_id;
                            NodActionV1 {
                                raw_ordinal: u32::try_from(start + local).unwrap(),
                                tribute_id,
                                nod_id: tribute_id,
                                owner: tribute.owner,
                                wwd: day.value(),
                                league_id: 1,
                                floor_price_minor: U256::ZERO,
                                gratis_load_minor: U256::from(1),
                                entry_price_minor: U256::ZERO,
                                settlement_cost_minor: U256::from(2),
                                issuance_currency: tribute.issuance_currency,
                                reference_currency: tribute.reference_currency,
                                issued_at: 1_784_765_900,
                                bucket_key: hash(u8::try_from(local % 251).unwrap()),
                            }
                        })
                        .collect::<Vec<_>>();
                    for action in &actions {
                        nod_root
                            .push(
                                &action.encode_canonical_record(&limits).unwrap(),
                                limits.max_bounded_bytes,
                            )
                            .unwrap();
                    }
                    let contributors = contributors_by_owner[start..end].to_vec();
                    let chunk = ResultChunkV1 {
                        protocol_bundle_hash: bundle_hash,
                        job_id,
                        attempt: 0,
                        chunk_ordinal: index,
                        first_nod_ordinal: u32::try_from(start).unwrap(),
                        ordered_nod_actions: actions.clone(),
                        ordered_eligible_contributors: contributors.clone(),
                    };
                    let chunk_hash = chunk.result_chunk_hash(&limits).unwrap();
                    let mut chunk_ref = cas
                        .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
                        .unwrap();
                    chunk_ref.expected_ocb1_kind = Some(ObjectKind::ResultChunkV1.tag());
                    result_chunk_refs.push(chunk_ref.clone());
                    let entry = OutputManifestEntryV1 {
                        chunk_ordinal: index,
                        result_chunk_hash: chunk_hash,
                        result_chunk_ref: chunk_ref,
                    };
                    output_manifest_root
                        .push(
                            &entry.encode_canonical_record(&limits).unwrap(),
                            limits.max_bounded_bytes,
                        )
                        .unwrap();
                    let nod_records = actions
                        .iter()
                        .map(|action| action.encode_canonical_record(&limits).unwrap())
                        .collect::<Vec<_>>();
                    let bucket_records = (start..end)
                        .map(|ordinal| ordinal.to_be_bytes().to_vec())
                        .collect::<Vec<_>>();
                    for record in &bucket_records {
                        bucket_root.push(record, limits.max_bounded_bytes).unwrap();
                    }
                    let contributor_records = contributors
                        .iter()
                        .map(|contributor| contributor.encode_canonical_record(&limits).unwrap())
                        .collect::<Vec<_>>();
                    let manifest_records = vec![entry.encode_canonical_record(&limits).unwrap()];
                    let chunk_hash_records = vec![chunk_hash.as_slice().to_vec()];
                    let count = u32::try_from(end - start).unwrap();
                    let raw_nominal_total = tributes[start..end]
                        .iter()
                        .fold(U256::ZERO, |total, tribute| {
                            total.checked_add(tribute.nominal_amount_minor).unwrap()
                        });
                    let nod_cost_total = actions.iter().fold(U256::ZERO, |total, action| {
                        total.checked_add(action.settlement_cost_minor).unwrap()
                    });
                    let summary = RootReduceSummaryV1 {
                        protocol_bundle_hash: bundle_hash,
                        job_id,
                        attempt: 0,
                        plan_hash,
                        covered_primary_start: index,
                        covered_primary_count: 1,
                        nod_actions: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::NodActions,
                            index,
                            &nod_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        bucket_records: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::BucketRecords,
                            index,
                            &bucket_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        contributor_actions: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::ContributorActions,
                            index,
                            &contributor_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        output_manifest_entries: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::CompleteOutputManifest,
                            index,
                            &manifest_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::ResultChunkHashes,
                            index,
                            &chunk_hash_records,
                            B256::len_bytes(),
                        )
                        .unwrap(),
                        tribute_count: count,
                        nod_count: count,
                        bucket_count: count,
                        contributor_count: u32::try_from(contributors.len()).unwrap(),
                        tribute_nominal_total: raw_nominal_total,
                        eligible_nominal_total: raw_nominal_total,
                        lysis_allocation_minor: U256::from(count),
                        nod_cost_total,
                        first_error_ordinal: None,
                    };
                    let coverage_root = summary.result_chunk_hashes.tree_root;
                    let output_coverage_root = coverage_root;
                    (
                        UnitArtifactV1::from_canonical_output(
                            &spec,
                            WorkOutputHeaderV1 {
                                source_coverage_root: coverage_root,
                                output_coverage_root,
                                source_coverage_count: 1,
                                output_coverage_count: 1,
                            },
                            BoundedBytes(
                                encode_root_reduce_output(
                                    &RootReduceOutputV1::Leaf {
                                        summary,
                                        output_manifest_entry: entry.clone(),
                                    },
                                    &limits,
                                )
                                .unwrap(),
                            ),
                            &limits,
                        )
                        .unwrap(),
                        Some(entry),
                    )
                }
                _ => (
                    UnitArtifactV1::from_canonical_output(
                        &spec,
                        WorkOutputHeaderV1 {
                            source_coverage_root: hash(0xa1),
                            output_coverage_root: hash(0xa2),
                            source_coverage_count: 1,
                            output_coverage_count: 1,
                        },
                        BoundedBytes(vec![0x42]),
                        &limits,
                    )
                    .unwrap(),
                    None,
                ),
            };
            let mut artifact_ref = cas
                .publish_bytes(&artifact.encode_canonical(&limits).unwrap())
                .unwrap();
            artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
            admissions
                .admit_verified_unit(
                    AdmissionPositionV1 { plan_ordinal },
                    &spec,
                    artifact_ref,
                    result_entry,
                )
                .unwrap();
        }
        drop(admissions);
        drop(input_refs);
        drop(reader);
        drop(cas);

        Fixture {
            job_id,
            day,
            bundle: pinned_bundle,
            nod_root: nod_root.finish().unwrap(),
            bucket_root: bucket_root.finish().unwrap(),
            output_manifest_root: output_manifest_root.finish().unwrap(),
            nod_count: tribute_count,
            result_chunk_refs,
        }
    }

    fn pending_head(f: &Fixture) -> NodMaterializationHeadV1 {
        NodMaterializationHeadV1 {
            queue_sequence: 1,
            job_id: f.job_id,
            program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
            worldwide_day: f.day.value(),
            generation: 1,
            nod_root: f.nod_root,
            nod_count: f.nod_count,
            next_nod_ordinal: 256,
            last_progress_height: 100,
        }
    }

    fn write_native_pending_head(root: &Path, f: &Fixture) {
        use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
        use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
        use reth_ethereum::provider::db::{
            database::Database,
            init_db,
            mdbx::DatabaseArguments,
            tables,
            transaction::{DbTx, DbTxMut},
        };
        use reth_primitives_traits::StorageEntry;
        let mut owner = HashMapStorageProvider::new_with_chain_identity(
            copied_native::chain().chain().id(),
            copied_native::chain().genesis_hash(),
        );
        StorageHandle::enter(&mut owner, |storage| {
            let nod = NodContract::new(storage);
            let p = NodCertifiedGenerationProjection {
                worldwide_day: f.day,
                generation: 1,
                job_id: f.job_id,
                program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
                protocol_bundle_hash: f.bundle.hash(),
                nod_root: f.nod_root,
                bucket_root: f.bucket_root,
                output_manifest_root: f.output_manifest_root,
                tribute_count: f.nod_count,
                nod_count: f.nod_count,
                bucket_count: f.nod_count,
                nod_amount_total: U256::from(f.nod_count) * U256::from(2),
                lysis_allocation_minor: U256::from(f.nod_count),
                issued_at: 1_000,
                next_nod_ordinal: 256,
                last_progress_height: 100,
            };
            nod.ocomp_materialization_head_sequence.write(1).unwrap();
            nod.ocomp_materialization_tail_sequence.write(2).unwrap();
            nod.ocomp_materialization_queue_wwd
                .write(&1, f.day)
                .unwrap();
            nod.ocomp_target_generation.write(&f.day, 1).unwrap();
            nod.ocomp_namespace_root.write(&f.day, p.nod_root).unwrap();
            nod.ocomp_bucket_root.write(&f.day, p.bucket_root).unwrap();
            nod.ocomp_output_manifest_root
                .write(&f.day, p.output_manifest_root)
                .unwrap();
            nod.ocomp_generation_metadata
                .write(&f.day, p.metadata_word())
                .unwrap();
            nod.ocomp_nod_amount_total
                .write(&f.day, p.nod_amount_total)
                .unwrap();
            nod.ocomp_lysis_allocation_minor
                .write(&f.day, p.lysis_allocation_minor)
                .unwrap();
            nod.ocomp_materialization_job_id
                .write(&f.day, p.job_id)
                .unwrap();
            nod.ocomp_materialization_protocol_bundle_hash
                .write(&f.day, p.protocol_bundle_hash)
                .unwrap();
            nod.ocomp_materialization_program_semantics_hash
                .write(&f.day, p.program_semantics_hash)
                .unwrap();
            nod.ocomp_materialization_next_nod_ordinal
                .write(&f.day, p.next_nod_ordinal)
                .unwrap();
            nod.ocomp_materialization_last_progress_height
                .write(&f.day, p.last_progress_height)
                .unwrap();
        });
        let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        for ((address, slot), value) in owner.storage {
            if !value.is_zero() {
                tx.put::<tables::PlainStorageState>(
                    address,
                    StorageEntry {
                        key: alloy_primitives::B256::from(slot.to_be_bytes::<32>()),
                        value,
                    },
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    fn read_native_pending_head<P: reth_provider::StateProviderFactory>(
        provider: &P,
    ) -> NodMaterializationHeadV1 {
        use outbe_primitives::storage::{readonly::ReadOnlyStorageProvider, StorageHandle};
        let state = provider.latest().unwrap();
        let reader = OcompExExStateReaderV1 {
            state: state.as_ref(),
        };
        let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
            reader,
            copied_native::chain().chain().id(),
            copied_native::chain().genesis_hash(),
        );
        outbe_nod::NodContract::new(StorageHandle::new(&mut readonly))
            .ocomp_materialization_head()
            .unwrap()
            .unwrap()
    }

    fn build_remaining(
        root: &Path,
        f: &Fixture,
        head: &NodMaterializationHeadV1,
    ) -> eyre::Result<outbe_ocomp::nod_materialization::BuiltNodMaterializationBatchV1> {
        let limits = poc_schema_limits();
        let cas = FilesystemCasReader::open(root.join("cas-v1"), CAS_LIMITS)?;
        let job = hex::encode(f.job_id);
        let inputs = VerifiedInputChunkRefCatalog::reopen(
            root.join("exporter-v1/input-refs").join(&job),
            &cas,
            limits,
            poc_input_list_limits(),
        )?;
        let admissions = AdmissionCatalogReader::open_existing(
            root.join("supervisor-v1/jobs")
                .join(&job)
                .join("admissions"),
            &cas,
            limits,
        )?;
        let audit =
            LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &limits)?;
        Ok(build_nod_materialization_batch_with_references(
            &audit, head, 3,
        )?)
    }

    fn reference_root(root: &Path, job: B256, first_nod_ordinal: u32) -> PathBuf {
        root.join("supervisor-v1/materialization-references")
            .join(hex::encode(job))
            .join(first_nod_ordinal.to_string())
    }

    fn chunk_path(root: &Path, f: &Fixture, ordinal: usize) -> PathBuf {
        let digest = hex::encode(f.result_chunk_refs[ordinal].transport_digest);
        root.join("cas-v1/objects")
            .join(&digest[..2])
            .join(&digest[2..])
    }

    #[test]
    fn copied_pending_nod_remains_buildable_after_terminal_pruning_and_consumed_chunk_removal() {
        use outbe_ocomp::embedded::EmbeddedJobEventV1;
        use outbe_ocomp::nod_materialization::MaterializationReferenceStoreV1;
        for remove_required in [false, true] {
            let donor = tempfile::tempdir().unwrap();
            let receiver = tempfile::tempdir().unwrap();
            let points = copied_native::write_frames(&donor.path().join("chain"), 0, 100);
            let public = donor.path().join("ocomp");
            let f = fixture(&public, 0x41, WorldwideDay::new(20_260_725), 257);
            write_native_pending_head(&donor.path().join("chain"), &f);
            let mut runtime = copied_native::runtime(
                copied_native::provider(&donor.path().join("chain")),
                &public,
                f.bundle.clone(),
            );
            let generation = runtime.state.observe_job(f.job_id, 90).unwrap();
            let digest = B256::repeat_byte(0x91);
            runtime
                .state
                .reduce(
                    f.job_id,
                    EmbeddedJobEventV1::LocalCompleted {
                        generation,
                        result_digest: digest,
                    },
                )
                .unwrap();
            runtime
                .state
                .reduce(
                    f.job_id,
                    EmbeddedJobEventV1::CanonicalCompleted {
                        result_digest: digest,
                    },
                )
                .unwrap();
            copied_native::catch_up(&mut runtime, points[100]);
            assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[100]);
            runtime.state.prune_terminal_job(f.job_id).unwrap();
            assert!(runtime.state.state(f.job_id).is_none());
            assert!(runtime.jobs.is_empty());
            let head = read_native_pending_head(&runtime.provider);
            assert_eq!(head, pending_head(&f));
            let built = build_remaining(&public, &f, &head).unwrap();
            assert_eq!(built.batch.actions.len(), 1);
            assert_eq!(built.batch.actions[0].raw_ordinal, 256);
            let references = MaterializationReferenceStoreV1::open(reference_root(
                &public,
                f.job_id,
                head.next_nod_ordinal,
            ))
            .unwrap();
            references.pin_exact(f.job_id, &built.dependencies).unwrap();
            let reference_member = reference_root(Path::new(""), f.job_id, head.next_nod_ordinal)
                .join(format!(
                    "{}.materialization-refs-v1.json",
                    hex::encode(f.job_id)
                ));
            let reference_bytes = fs::read(public.join(&reference_member)).unwrap();
            assert!(!reference_bytes.is_empty());
            drop(references);
            drop(runtime);
            copied_native::copy_tree(donor.path(), receiver.path());
            donor.close().unwrap();
            let public = receiver.path().join("ocomp");
            assert_eq!(
                fs::read(public.join(&reference_member)).unwrap(),
                reference_bytes
            );
            // This is a dependency-absence control, not an invocation of the GC scheduler.
            fs::remove_file(chunk_path(&public, &f, 0)).unwrap();
            if remove_required {
                fs::remove_file(chunk_path(&public, &f, 1)).unwrap();
            }
            let runtime = copied_native::runtime(
                copied_native::provider(&receiver.path().join("chain")),
                &public,
                f.bundle.clone(),
            );
            assert!(runtime.jobs.is_empty());
            let head = read_native_pending_head(&runtime.provider);
            let actual = build_remaining(&public, &f, &head);
            if remove_required {
                assert!(
                    actual.is_err(),
                    "pending NOD must not silently discard a missing required chunk"
                );
                assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[100]);
                continue;
            }
            assert_eq!(actual.unwrap(), built);
            drop(runtime);
            let later = copied_native::write_frames(&receiver.path().join("chain"), 101, 102);
            let mut runtime = copied_native::runtime(
                copied_native::provider(&receiver.path().join("chain")),
                &public,
                f.bundle.clone(),
            );
            copied_native::catch_up(&mut runtime, later[1]);
            drop(runtime);
            let restarted = copied_native::runtime(
                copied_native::provider(&receiver.path().join("chain")),
                &public,
                f.bundle.clone(),
            );
            assert_eq!(restarted.closure_checkpoint.current().unwrap(), later[1]);
            assert_eq!(
                fs::read(public.join(&reference_member)).unwrap(),
                reference_bytes
            );
            assert_eq!(
                build_remaining(&public, &f, &read_native_pending_head(&restarted.provider))
                    .unwrap(),
                built
            );
            assert_eq!(
                MaterializationReferenceStoreV1::open(reference_root(
                    &public,
                    f.job_id,
                    head.next_nod_ordinal
                ))
                .unwrap()
                .load_exact(f.job_id)
                .unwrap(),
                Some(built.dependencies)
            );
        }
    }
    struct PrepareOnlyRpc {
        allow_prepare: bool,
    }
    impl outbe_ocomp::vote_submitter::VoteSubmissionRpcV1 for PrepareOnlyRpc {
        type Error = std::io::Error;
        fn chain_id(&self) -> Result<u64, Self::Error> {
            assert!(self.allow_prepare);
            Ok(copied_native::chain().chain().id())
        }
        fn canonical_nonce(&self, _: Address) -> Result<u64, Self::Error> {
            assert!(self.allow_prepare);
            Ok(0)
        }
        fn gas_price(&self) -> Result<u128, Self::Error> {
            assert!(self.allow_prepare);
            Ok(1)
        }
        fn send_raw_transaction(&self, _: &[u8], _: B256) -> Result<B256, Self::Error> {
            panic!("this fixture must never submit a transaction")
        }
        fn transaction_receipt(
            &self,
            _: B256,
        ) -> Result<Option<outbe_ocomp::vote_submitter::VoteReceiptV1>, Self::Error> {
            panic!("foreign journal must fail before RPC")
        }
        fn canonical_block(
            &self,
            _: u64,
        ) -> Result<Option<outbe_ocomp::vote_submitter::VoteBlockV1>, Self::Error> {
            panic!("foreign journal must fail before RPC")
        }
        fn finalized_block(&self) -> Result<outbe_ocomp::vote_submitter::VoteBlockV1, Self::Error> {
            panic!("foreign journal must fail before RPC")
        }
    }

    #[test]
    fn copied_foreign_signed_materialization_journal_is_rejected_without_rewriting_recipient_key() {
        use outbe_ocomp::nod_materialization_submitter::{
            NodMaterializationSubmissionConfigV1, NodMaterializationSubmissionErrorV1,
            NodMaterializationSubmitterV1,
        };
        use outbe_primitives::signer::OutbeEvmSigner;
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let public = donor.path().join("public");
        let f = fixture(&public, 0x71, WorldwideDay::new(20_260_725), 257);
        let built = build_remaining(&public, &f, &pending_head(&f)).unwrap();
        let signer = OutbeEvmSigner::from_secret_bytes([0x11; 32]).unwrap();
        let journal = donor.path().join("signed-journal");
        let mut submitter = NodMaterializationSubmitterV1::open(
            NodMaterializationSubmissionConfigV1 {
                journal_root: journal.clone(),
                expected_chain_id: copied_native::chain().chain().id(),
                sender_address: signer.address(),
                limits: poc_schema_limits(),
            },
            PrepareOnlyRpc {
                allow_prepare: true,
            },
            signer,
        )
        .unwrap();
        submitter.reconcile(f.job_id, &built.batch).unwrap();
        drop(submitter);
        let original = fs::read(journal.join("submission-v1.json")).unwrap();
        copied_native::copy_tree(&journal, &receiver.path().join("signed-journal"));
        donor.close().unwrap();
        // Deliberately copied sender-owned data is a negative control, never a portable authority.
        let key_path = receiver.path().join("recipient-key.hex");
        fs::write(&key_path, format!("{}\n", hex::encode([0x22; 32]))).unwrap();
        let key_before = fs::read(&key_path).unwrap();
        let recipient = OutbeEvmSigner::from_secret_bytes([0x22; 32]).unwrap();
        let mut reopened = NodMaterializationSubmitterV1::open(
            NodMaterializationSubmissionConfigV1 {
                journal_root: receiver.path().join("signed-journal"),
                expected_chain_id: copied_native::chain().chain().id(),
                sender_address: recipient.address(),
                limits: poc_schema_limits(),
            },
            PrepareOnlyRpc {
                allow_prepare: false,
            },
            recipient,
        )
        .unwrap();
        assert!(matches!(
            reopened.reconcile(f.job_id, &built.batch),
            Err(NodMaterializationSubmissionErrorV1::ConflictingReplay)
        ));
        assert_eq!(fs::read(&key_path).unwrap(), key_before);
        assert_eq!(
            fs::read(receiver.path().join("signed-journal/submission-v1.json")).unwrap(),
            original
        );
    }

    mod copied_resident_authority {
        use super::*;
        use alloy_consensus::{transaction::SignerRecoverable as _, EthereumTxEnvelope, TxEip4844};
        use alloy_eips::eip2718::Decodable2718 as _;
        use alloy_primitives::keccak256;
        use outbe_ocomp::{
            nod_materialization::MaterializationReferenceStoreV1,
            nod_materialization_submitter::{
                reconcile_finalized_materialization_references,
                NodMaterializationSubmissionConfigV1, NodMaterializationSubmissionOutcomeV1,
                NodMaterializationSubmitterV1,
            },
            result_signer::OcompSigner,
            sign_once::{SignOnceError, SignOnceStore, SignOnceSubjectV1},
            vote_submitter::{VoteBlockV1, VoteReceiptV1, VoteSubmissionRpcV1},
        };
        use outbe_ocomp_protocol::committee::verify_low_s_prehash;
        use outbe_primitives::{projection::ProjectionCheckpoint, signer::OutbeEvmSigner};
        use std::{
            collections::BTreeMap,
            io::Write as _,
            os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
            sync::{Arc, Mutex},
        };

        struct PublicCopy {
            donor: tempfile::TempDir,
            fixture: Fixture,
            closed: ProjectionCheckpoint,
        }

        impl PublicCopy {
            fn new() -> Self {
                let donor = tempfile::tempdir().unwrap();
                let chain_root = donor.path().join("chain");
                let points = copied_native::write_frames(&chain_root, 0, 100);
                let public = donor.path().join("ocomp");
                let fixture = fixture(&public, 0x51, WorldwideDay::new(20_260_725), 257);
                write_native_pending_head(&chain_root, &fixture);
                let mut runtime = copied_native::runtime(
                    copied_native::provider(&chain_root),
                    &public,
                    fixture.bundle.clone(),
                );
                copied_native::catch_up(&mut runtime, points[100]);
                assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[100]);
                drop(runtime);
                Self {
                    donor,
                    fixture,
                    closed: points[100],
                }
            }

            fn place_public_files(&self, recipient: &Path) {
                copied_native::copy_tree(
                    &self.donor.path().join("chain"),
                    &recipient.join("chain"),
                );
                let source = self.donor.path().join("ocomp");
                let target = recipient.join("ocomp");
                // Public roots only. Resident keys, sign-once and sender journals
                // are deliberately absent from this placement list.
                for relative in [
                    "cas-v1",
                    "protocol-bundles-v1",
                    "exporter-v1/input-refs",
                    "exporter-v1/discovery",
                    "supervisor-v1/jobs",
                    "supervisor-v1/materialization-references",
                ] {
                    let from = source.join(relative);
                    if from.exists() {
                        copied_native::copy_tree(&from, &target.join(relative));
                    }
                }
            }
        }

        fn write_key(path: &Path, byte: u8) {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            writeln!(file, "{}", hex::encode([byte; 32])).unwrap();
            file.sync_all().unwrap();
        }

        fn resident_keys(public: &Path) -> (OutbeEvmSigner, OcompSigner, u32) {
            fs::create_dir_all(public).unwrap();
            let evm = public.join("ocomp-evm-key.hex");
            let result = public.join("ocomp-key-v1.hex");
            write_key(&evm, 0x21);
            write_key(&result, 0x31);
            let uid = fs::metadata(&evm).unwrap().uid();
            (
                OutbeEvmSigner::from_strict_file(evm, uid).unwrap(),
                OcompSigner::from_file(result, uid).unwrap(),
                uid,
            )
        }

        fn submission_root(public: &Path) -> PathBuf {
            public.join("supervisor-v1/materialization-submissions")
        }

        fn journal_root(public: &Path, f: &Fixture) -> PathBuf {
            submission_root(public)
                .join(hex::encode(f.job_id))
                .join("256")
        }

        // Scripted RPC completion exercises the real durable submitter. It is
        // not evidence of transaction execution/inclusion in the native fixture.
        #[derive(Clone)]
        struct CompletionRpc {
            enabled: bool,
            sender: Address,
            block: VoteBlockV1,
            sent: Arc<Mutex<Vec<B256>>>,
        }

        impl VoteSubmissionRpcV1 for CompletionRpc {
            type Error = std::io::Error;
            fn chain_id(&self) -> Result<u64, Self::Error> {
                assert!(self.enabled, "resident finalized journal must not call RPC");
                Ok(copied_native::chain().chain().id())
            }
            fn canonical_nonce(&self, sender: Address) -> Result<u64, Self::Error> {
                assert!(self.enabled);
                assert_eq!(sender, self.sender);
                Ok(7)
            }
            fn gas_price(&self) -> Result<u128, Self::Error> {
                assert!(self.enabled);
                Ok(1)
            }
            fn send_raw_transaction(
                &self,
                raw: &[u8],
                expected: B256,
            ) -> Result<B256, Self::Error> {
                assert!(self.enabled);
                assert!(!raw.is_empty());
                assert_eq!(keccak256(raw), expected);
                let mut encoded = raw;
                let transaction =
                    EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut encoded).unwrap();
                assert!(encoded.is_empty());
                assert_eq!(transaction.recover_signer().unwrap(), self.sender);
                self.sent.lock().unwrap().push(expected);
                Ok(expected)
            }
            fn transaction_receipt(&self, tx: B256) -> Result<Option<VoteReceiptV1>, Self::Error> {
                assert!(self.enabled);
                assert_eq!(*self.sent.lock().unwrap().last().unwrap(), tx);
                Ok(Some(VoteReceiptV1 {
                    transaction_hash: tx,
                    block_number: self.block.number,
                    block_hash: self.block.hash,
                    success: false,
                }))
            }
            fn canonical_block(&self, number: u64) -> Result<Option<VoteBlockV1>, Self::Error> {
                assert!(self.enabled);
                assert_eq!(number, self.block.number);
                Ok(Some(self.block))
            }
            fn finalized_block(&self) -> Result<VoteBlockV1, Self::Error> {
                assert!(self.enabled);
                Ok(self.block)
            }
        }

        fn rpc(
            signer: &OutbeEvmSigner,
            point: ProjectionCheckpoint,
            enabled: bool,
        ) -> CompletionRpc {
            CompletionRpc {
                enabled,
                sender: signer.address(),
                block: VoteBlockV1 {
                    number: point.block_number,
                    hash: point.block_hash,
                },
                sent: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn submitter(
            public: &Path,
            f: &Fixture,
            signer: OutbeEvmSigner,
            rpc: CompletionRpc,
        ) -> NodMaterializationSubmitterV1<CompletionRpc> {
            NodMaterializationSubmitterV1::open(
                NodMaterializationSubmissionConfigV1 {
                    journal_root: journal_root(public, f),
                    expected_chain_id: copied_native::chain().chain().id(),
                    sender_address: signer.address(),
                    limits: poc_schema_limits(),
                },
                rpc,
                signer,
            )
            .unwrap()
        }

        fn prepare_resident_journal(
            public: &Path,
            f: &Fixture,
            batch: &outbe_ocomp_protocol::nod_materialization::NodMaterializationBatchV1,
            signer: OutbeEvmSigner,
            point: ProjectionCheckpoint,
            finalize: bool,
        ) {
            let rpc = rpc(&signer, point, true);
            let sent = rpc.sent.clone();
            let mut submitter = submitter(public, f, signer, rpc);
            assert_eq!(
                submitter.reconcile(f.job_id, batch).unwrap(),
                NodMaterializationSubmissionOutcomeV1::Pending
            );
            if finalize {
                // Prepared -> Submitted -> Included -> Finalized, entirely via
                // the public owner. A failed tx leaves the pending NOD unchanged.
                for _ in 0..2 {
                    assert_eq!(
                        submitter.reconcile(f.job_id, batch).unwrap(),
                        NodMaterializationSubmissionOutcomeV1::Pending
                    );
                }
                assert_eq!(
                    submitter.reconcile(f.job_id, batch).unwrap(),
                    NodMaterializationSubmissionOutcomeV1::Finalized { success: false }
                );
                assert_eq!(sent.lock().unwrap().len(), 1);
            } else {
                assert!(sent.lock().unwrap().is_empty());
            }
        }

        fn assert_public_reopen(
            recipient: &Path,
            f: &Fixture,
            point: ProjectionCheckpoint,
            expected: &outbe_ocomp::nod_materialization::BuiltNodMaterializationBatchV1,
        ) {
            let public = recipient.join("ocomp");
            let runtime = copied_native::runtime(
                copied_native::provider(&recipient.join("chain")),
                &public,
                f.bundle.clone(),
            );
            assert_eq!(runtime.closure_checkpoint.current().unwrap(), point);
            assert_eq!(
                build_remaining(&public, f, &read_native_pending_head(&runtime.provider)).unwrap(),
                *expected
            );
        }

        fn advance_to_k(recipient: &Path, f: &Fixture) -> ProjectionCheckpoint {
            let points = copied_native::write_frames(&recipient.join("chain"), 101, 102);
            let mut runtime = copied_native::runtime(
                copied_native::provider(&recipient.join("chain")),
                &recipient.join("ocomp"),
                f.bundle.clone(),
            );
            copied_native::catch_up(&mut runtime, points[1]);
            assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[1]);
            points[1]
        }

        #[derive(Clone, Copy, Debug)]
        enum ReferencesAtCopy {
            LivePrepared,
            StaleFinalized,
            Released,
        }

        #[test]
        fn copied_nested_nod_refs_release_only_resident_finalized_journals_and_stay_released_at_k()
        {
            for state in [
                ReferencesAtCopy::LivePrepared,
                ReferencesAtCopy::StaleFinalized,
                ReferencesAtCopy::Released,
            ] {
                let image = PublicCopy::new();
                let recipient = tempfile::tempdir().unwrap();
                let public = recipient.path().join("ocomp");
                let (signer, _, _) = resident_keys(&public);
                let donor_public = image.donor.path().join("ocomp");
                let built =
                    build_remaining(&donor_public, &image.fixture, &pending_head(&image.fixture))
                        .unwrap();
                assert!(!built.dependencies.is_empty());
                let refs = MaterializationReferenceStoreV1::open(reference_root(
                    &donor_public,
                    image.fixture.job_id,
                    256,
                ))
                .unwrap();
                refs.pin_exact(image.fixture.job_id, &built.dependencies)
                    .unwrap();
                if matches!(state, ReferencesAtCopy::Released) {
                    refs.release(image.fixture.job_id).unwrap();
                }
                drop(refs);
                // This journal belongs to the receiver before file placement.
                prepare_resident_journal(
                    &public,
                    &image.fixture,
                    &built.batch,
                    signer,
                    image.closed,
                    !matches!(state, ReferencesAtCopy::LivePrepared),
                );
                let resident_journal =
                    journal_root(&public, &image.fixture).join("submission-v1.json");
                let journal_before = fs::read(&resident_journal).unwrap();
                image.place_public_files(recipient.path());
                let PublicCopy {
                    donor,
                    fixture,
                    closed,
                } = image;
                donor.close().unwrap();
                assert_eq!(fs::read(&resident_journal).unwrap(), journal_before);
                let ref_path = reference_root(&public, fixture.job_id, 256);
                let references = MaterializationReferenceStoreV1::open(&ref_path).unwrap();
                assert_eq!(
                    references.load_exact(fixture.job_id).unwrap().is_some(),
                    !matches!(state, ReferencesAtCopy::Released)
                );
                let cas = FilesystemCasReader::open(public.join("cas-v1"), CAS_LIMITS).unwrap();
                let before: Vec<_> = built
                    .dependencies
                    .iter()
                    .map(|reference| cas.read_verified(reference).unwrap().bytes().to_vec())
                    .collect();
                let expected_releases =
                    usize::from(matches!(state, ReferencesAtCopy::StaleFinalized));
                assert_eq!(
                    reconcile_finalized_materialization_references(
                        &submission_root(&public),
                        &public.join("supervisor-v1/materialization-references")
                    )
                    .unwrap(),
                    expected_releases
                );
                drop(references);
                for restart in 0..2 {
                    let point = if restart == 0 {
                        closed
                    } else {
                        advance_to_k(recipient.path(), &fixture)
                    };
                    assert_public_reopen(recipient.path(), &fixture, point, &built);
                    assert_eq!(
                        reconcile_finalized_materialization_references(
                            &submission_root(&public),
                            &public.join("supervisor-v1/materialization-references")
                        )
                        .unwrap(),
                        0
                    );
                    let references = MaterializationReferenceStoreV1::open(&ref_path).unwrap();
                    assert_eq!(
                        references.load_exact(fixture.job_id).unwrap(),
                        matches!(state, ReferencesAtCopy::LivePrepared)
                            .then(|| built.dependencies.clone())
                    );
                    assert_eq!(fs::read(&resident_journal).unwrap(), journal_before);
                }
                let after: Vec<_> = built
                    .dependencies
                    .iter()
                    .map(|reference| cas.read_verified(reference).unwrap().bytes().to_vec())
                    .collect();
                assert_eq!(
                    after, before,
                    "reference retirement must not remove public CAS data"
                );
            }
        }

        fn protected_files(public: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
            fn collect(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (Vec<u8>, u32)>) {
                let metadata = fs::symlink_metadata(path).unwrap();
                assert!(!metadata.file_type().is_symlink());
                if metadata.is_dir() {
                    for entry in fs::read_dir(path).unwrap() {
                        collect(root, &entry.unwrap().path(), result);
                    }
                } else {
                    assert!(metadata.is_file());
                    result.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        (
                            fs::read(path).unwrap(),
                            metadata.permissions().mode() & 0o777,
                        ),
                    );
                }
            }
            let mut result = BTreeMap::new();
            for relative in [
                "ocomp-evm-key.hex",
                "ocomp-key-v1.hex",
                "supervisor-v1/sign-once",
                "supervisor-v1/materialization-submissions",
            ] {
                collect(public, &public.join(relative), &mut result);
            }
            result
        }

        #[test]
        fn copied_public_files_preserve_own_real_signatures_and_equivocation_guard_through_k() {
            let image = PublicCopy::new();
            let recipient = tempfile::tempdir().unwrap();
            let public = recipient.path().join("ocomp");
            let (evm, signer, uid) = resident_keys(&public);
            let built = build_remaining(
                &image.donor.path().join("ocomp"),
                &image.fixture,
                &pending_head(&image.fixture),
            )
            .unwrap();
            prepare_resident_journal(
                &public,
                &image.fixture,
                &built.batch,
                evm.clone(),
                image.closed,
                true,
            );
            // A typed resident signing subject, not a claim of an authenticated
            // canonical Completed job or an on-chain result vote.
            let subject = SignOnceSubjectV1 {
                chain_id: copied_native::chain().chain().id(),
                genesis_hash: copied_native::chain().genesis_hash(),
                fork_id: image.fixture.bundle.bundle().fork_id,
                job_id: image.fixture.job_id,
                attempt: 0,
                protocol_bundle_hash: image.fixture.bundle.hash(),
                result_validator_set_epoch: 1,
                result_committee_set_hash: hash(0x81),
                result_ocomp_binding_hash: hash(0x82),
                ocomp_key_hash: keccak256(signer.public_key_sec1()),
                key_epoch: signer.key_epoch(),
                result_digest: keccak256(
                    built.batch.encode_canonical(&poc_schema_limits()).unwrap(),
                ),
            };
            let sign_root = public.join("supervisor-v1/sign-once");
            let sign_once =
                SignOnceStore::open(sign_root.clone(), uid, poc_schema_limits()).unwrap();
            let mut signing_digest = None;
            let signed = sign_once
                .record_or_replay(subject, |digest| {
                    signing_digest = Some(digest);
                    signer
                        .sign_result_digest(digest)
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            let digest = signing_digest.expect("the initial record must really sign");
            verify_low_s_prehash(&signer.public_key_sec1(), digest, &signed.signature_rs).unwrap();
            drop(sign_once);
            drop(signer);
            let before = protected_files(&public);
            assert_eq!(
                before.len(),
                4,
                "two keys, one sign-once record and one signed journal"
            );
            image.place_public_files(recipient.path());
            let PublicCopy {
                donor,
                fixture,
                closed,
            } = image;
            donor.close().unwrap();
            assert_eq!(protected_files(&public), before);
            for restart in 0..2 {
                let point = if restart == 0 {
                    closed
                } else {
                    advance_to_k(recipient.path(), &fixture)
                };
                assert_public_reopen(recipient.path(), &fixture, point, &built);
                let own_evm =
                    OutbeEvmSigner::from_strict_file(public.join("ocomp-evm-key.hex"), uid)
                        .unwrap();
                assert_eq!(own_evm.address(), evm.address());
                let own_signer =
                    OcompSigner::from_file(public.join("ocomp-key-v1.hex"), uid).unwrap();
                let store =
                    SignOnceStore::open(sign_root.clone(), uid, poc_schema_limits()).unwrap();
                let replay = store
                    .record_or_replay(subject, |_| {
                        panic!("copied public placement must not trigger another signature")
                    })
                    .unwrap();
                assert_eq!(replay, signed);
                verify_low_s_prehash(&own_signer.public_key_sec1(), digest, &replay.signature_rs)
                    .unwrap();
                assert!(matches!(
                    store.record_or_replay(
                        SignOnceSubjectV1 {
                            result_digest: hash(0xfe),
                            ..subject
                        },
                        |_| panic!("equivocation must fail before signing")
                    ),
                    Err(SignOnceError::Equivocation { .. })
                ));
                drop(store);
                let no_rpc = rpc(&own_evm, point, false);
                let mut journal = submitter(&public, &fixture, own_evm, no_rpc);
                assert_eq!(
                    journal.reconcile(fixture.job_id, &built.batch).unwrap(),
                    NodMaterializationSubmissionOutcomeV1::Finalized { success: false }
                );
                drop(journal);
                assert_eq!(protected_files(&public), before);
            }
        }
    }
}
