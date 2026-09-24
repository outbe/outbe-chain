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
#[cfg(feature = "snapshot-integration")]
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

        // Paste inside copied_public_work::copied_resident_authority.
        // Component evidence only: native MDBX/static frames + NOD/Intex/ValidatorSet
        // owner state, actual EmbeddedOcompExExV1 dispatch, real signer/journal/HTTP client.
        // ActiveGeneration and receipt/finality RPC replies are scripted. No Metadosis
        // quorum, native EVM execution, real Lysis pipeline, or E2E09 claim is made.
        mod pending_spawned {
            use super::*;
            use alloy_consensus::{BlockHeader as _, Header, Sealable, Transaction as _};
            use alloy_primitives::{Bytes, TxKind};
            use alloy_sol_types::{sol, SolCall, SolValue};
            use outbe_intex::payout::{
                build_contributor_range_proof, contributor_list_root, decode_contributor_leaf,
                CONTRIBUTOR_LEAF_BYTES,
            };
            use outbe_node::finalized_frame::{
                read_bounded_finalized_frames, RethFinalizedFrameSource,
            };
            use outbe_ocomp::{
                embedded_runtime::{EmbeddedOcompBundleConfigV1, EmbeddedOcompDomainConfigV1},
                payout_artifact::{
                    write_contributor_payout_artifact, CONTRIBUTOR_PAYOUT_ARTIFACT_FILE,
                },
                payout_submitter::PayoutTickOutcomeV1,
            };
            use outbe_ocomp_protocol::{
                abi::{encode_materialize_certified_nods_calldata, NOD_FACTORY_ADDRESS},
                result::ExactCountsV1,
                state::ActiveGenerationV1,
            };
            use outbe_primitives::{
                addresses::{INTEX_ADDRESS, INTEX_FACTORY_ADDRESS, METADOSIS_ADDRESS},
                storage::{readonly::ReadOnlyStorageProvider, StorageHandle},
                OutbeHeader, OutbePrimitives, OutbeReceipt, OutbeTxEnvelope,
            };
            use reth_ethereum::provider::db::{
                database::Database,
                init_db,
                mdbx::DatabaseArguments,
                models::{ShardedKey, StoredBlockBodyIndices},
                table::Table,
                tables,
                transaction::{DbTx, DbTxMut},
            };
            use reth_provider::{
                providers::{RocksDBProvider, StaticFileProviderBuilder},
                StateProviderFactory, StaticFileSegment, StaticFileWriter,
            };
            use std::{
                io::{BufRead as _, BufReader, Read as _},
                net::{TcpListener, TcpStream},
                sync::atomic::{AtomicBool, Ordering},
                time::{Duration, Instant},
            };

            const H: u64 = 100;
            // Local ABI copies only: avoids adding an outbe-intexfactory dev dependency.
            // Signatures match its public Solidity interface exactly.
            sol! {
                struct Round { uint256 amount; uint32 contributorCount; uint256 paidSoFar; uint32 paidLeafCount; }
                struct Certified { uint64 seriesVersion; bytes32 contributorRoot; uint32 contributorCount; uint256 eligibleNominalTotal; }
                struct Leaf { address owner; uint256 sourceTributeId; uint256 nominal; }
                function contributorPayoutRound(uint32 worldwideDay) external view returns (Round);
                function certifiedContributorGeneration(uint32 worldwideDay) external view returns (Certified);
                function contributorPaidWord(uint32 worldwideDay, uint32 wordIndex) external view returns (uint256);
                function getActiveLysisGeneration(uint32 wwd) external view returns (bytes);
                function payContributorBatch(uint32 worldwideDay, uint32 startIndex, Leaf[] leaves, bytes32[] proof) external;
            }

            fn isolated(name: &str) -> bool {
                const CASE: &str = "OUTBE_TEST_COPIED_PENDING_CASE";
                const STARTED: &str = "OUTBE_TEST_COPIED_PENDING_STARTED";
                if std::env::var(CASE).ok().as_deref() == Some(name) {
                    outbe_consensus::proof::init_consensus_chain_id(
                        copied_native::chain().chain().id(),
                    )
                    .unwrap();
                    outbe_chain_constants::initialize(None).unwrap();
                    fs::write(std::env::var_os(STARTED).unwrap(), name).unwrap();
                    return true;
                }
                let witness = tempfile::tempdir().unwrap();
                let started = witness.path().join("started");
                let exact = format!(
                    "ocomp_exex::tests::materialization::copied_public_work::copied_resident_authority::pending_spawned::{name}"
                );
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
                    .env(CASE, name)
                    .env(STARTED, &started)
                    .env("RAYON_NUM_THREADS", "2")
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(120);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            assert!(status.success(), "pending component child failed: {status}");
                            assert_eq!(fs::read_to_string(&started).unwrap(), name);
                            return false;
                        }
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(20))
                        }
                        result => {
                            let _ = child.kill();
                            let _ = child.wait();
                            panic!("pending component timed out or wait failed: {result:?}");
                        }
                    }
                }
            }

            fn artifact(public: &Path, f: &Fixture) -> Vec<[u8; CONTRIBUTOR_LEAF_BYTES]> {
                let limits = poc_schema_limits();
                let cas = FilesystemCasReader::open(public.join("cas-v1"), CAS_LIMITS).unwrap();
                let job = hex::encode(f.job_id);
                let inputs = VerifiedInputChunkRefCatalog::reopen(
                    public.join("exporter-v1/input-refs").join(&job),
                    &cas,
                    limits,
                    poc_input_list_limits(),
                )
                .unwrap();
                let job_root = public.join("supervisor-v1/jobs").join(&job);
                let admissions = AdmissionCatalogReader::open_existing(
                    job_root.join("admissions"),
                    &cas,
                    limits,
                )
                .unwrap();
                let audit = LocalLysisPlanAuditV1::open_read_only(
                    &admissions,
                    &inputs,
                    &cas,
                    &f.bundle,
                    &limits,
                )
                .unwrap();
                let count = write_contributor_payout_artifact(&audit, &job_root).unwrap();
                assert_eq!(count, f.nod_count);
                let bytes = fs::read(job_root.join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE)).unwrap();
                assert_eq!(bytes.len(), count as usize * CONTRIBUTOR_LEAF_BYTES);
                bytes
                    .chunks_exact(CONTRIBUTOR_LEAF_BYTES)
                    .map(|chunk| chunk.try_into().unwrap())
                    .collect()
            }

            // Typed ActiveGeneration is a SCRIPTED response, bound to this job and audit.
            // It is not written into Metadosis and is not evidence of canonical activation.
            fn scripted_active(f: &Fixture, leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]]) -> Vec<u8> {
                ActiveGenerationV1 {
                    job_id: f.job_id,
                    program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
                    nod_root: f.nod_root,
                    bucket_root: f.bucket_root,
                    contributor_root: contributor_list_root(leaves.len() as u32, leaves.iter())
                        .unwrap(),
                    output_manifest_root: f.output_manifest_root,
                    exact_counts: ExactCountsV1 {
                        tribute_count: f.nod_count,
                        nod_count: f.nod_count,
                        bucket_count: f.nod_count,
                        contributor_count: leaves.len() as u32,
                        semantic_event_count: 0,
                    },
                    result_evidence_hash: f.output_manifest_root,
                    availability_certificate_hash: None,
                }
                .encode_canonical(&poc_schema_limits())
                .unwrap()
            }

            fn expected_payout(f: &Fixture, leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]]) -> Vec<u8> {
                payContributorBatchCall {
                    worldwideDay: f.day.into(),
                    startIndex: 0,
                    leaves: leaves[..256]
                        .iter()
                        .map(|bytes| {
                            let leaf = decode_contributor_leaf(bytes);
                            Leaf {
                                owner: leaf.owner,
                                sourceTributeId: leaf.source_tribute_id,
                                nominal: leaf.nominal,
                            }
                        })
                        .collect(),
                    proof: build_contributor_range_proof(leaves.len() as u32, 0, leaves.iter())
                        .unwrap(),
                }
                .abi_encode()
            }

            // Added below: native fixture owners and signed native-frame writer.
            fn seed_native(
                root: &Path,
                f: &Fixture,
                sender: Address,
                leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]],
            ) {
                use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
                use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
                use reth_ethereum::provider::db::{
                    database::Database,
                    init_db,
                    mdbx::DatabaseArguments,
                    tables,
                    transaction::{DbTx, DbTxMut},
                };
                let mut owner = HashMapStorageProvider::new_with_chain_identity(
                    copied_native::chain().chain().id(),
                    copied_native::chain().genesis_hash(),
                );
                owner.set_block_number(1);
                StorageHandle::enter(&mut owner, |storage| {
                    let nod = NodContract::new(storage.clone());
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
                    let intex = outbe_intex::schema::IntexContract::new(storage.clone());
                    let count = leaves.len() as u32;
                    let root = contributor_list_root(count, leaves.iter()).unwrap();
                    let total = leaves.iter().fold(U256::ZERO, |sum, leaf| {
                        sum.checked_add(decode_contributor_leaf(leaf).nominal)
                            .unwrap()
                    });
                    intex.ocomp_contributor_root.write(&f.day, root).unwrap();
                    intex
                        .ocomp_contributor_metadata
                        .write(&f.day, U256::ONE | (U256::from(count) << 64))
                        .unwrap();
                    intex
                        .ocomp_eligible_nominal_total
                        .write(&f.day, total)
                        .unwrap();
                    outbe_intex::api::open_certified_payout_round(
                        &storage,
                        f.day.into(),
                        U256::from(10_000),
                    )
                    .unwrap();
                    let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
                    validators.config_owner.write(sender).unwrap();
                    validators.set_config_max_validators(128).unwrap();
                    validators.config_epoch_length_blocks.write(10).unwrap();
                    // BLS12-381 G1 generator compressed; only fixture admission uses it.
                    // No consensus signer/quorum or SGX identity is constructed here.
                    let key: [u8; 48] = hex::decode("97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb").unwrap().try_into().unwrap();
                    validators.register_validator(sender, sender, &key).unwrap();
                    validators
                        .activate_validator_via_boundary_for_test(sender)
                        .unwrap();
                    assert_eq!(
                        validators
                            .resolve_validator_for_role(
                                sender,
                                outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp
                            )
                            .unwrap(),
                        Some(sender)
                    );
                });
                let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                type Word = <tables::PlainStorageState as Table>::Value;
                type HistoryKey = <tables::StoragesHistory as Table>::Key;
                type HistoryBlocks = <tables::StoragesHistory as Table>::Value;
                for ((address, slot), value) in owner.storage {
                    if value.is_zero() {
                        continue;
                    }
                    let key = B256::from(slot.to_be_bytes::<32>());
                    tx.put::<tables::PlainAccountState>(address, Default::default())
                        .unwrap();
                    tx.put::<tables::PlainStorageState>(address, Word { key, value })
                        .unwrap();
                    tx.put::<tables::StorageChangeSets>(
                        (0, address).into(),
                        Word {
                            key,
                            value: U256::ZERO,
                        },
                    )
                    .unwrap();
                    tx.put::<tables::StoragesHistory>(
                        HistoryKey {
                            address,
                            sharded_key: ShardedKey {
                                key,
                                highest_block_number: u64::MAX,
                            },
                        },
                        HistoryBlocks::new(vec![0]).unwrap(),
                    )
                    .unwrap();
                }
                tx.commit().unwrap();
            }

            fn signed_frames(
                root: &Path,
                first: u64,
                last: u64,
                signer: &OutbeEvmSigner,
            ) -> Vec<ProjectionCheckpoint> {
                let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                let settings = reth_provider::StorageSettings::v1();
                match tx
                    .get::<tables::Metadata>("storage_settings".into())
                    .unwrap()
                {
                    Some(bytes) => assert!(
                        !serde_json::from_slice::<reth_provider::StorageSettings>(&bytes)
                            .unwrap()
                            .is_v2()
                    ),
                    None => tx
                        .put::<tables::Metadata>(
                            "storage_settings".into(),
                            serde_json::to_vec(&settings).unwrap(),
                        )
                        .unwrap(),
                }
                let mut parent = if first == 0 {
                    B256::ZERO
                } else {
                    tx.get::<tables::CanonicalHeaders>(first - 1)
                        .unwrap()
                        .unwrap()
                };
                let files = StaticFileProviderBuilder::read_write(root.join("static_files"))
                    .with_blocks_per_file(1_000)
                    .build::<OutbePrimitives>()
                    .unwrap();
                let mut writer = files
                    .get_writer(first, StaticFileSegment::Transactions)
                    .unwrap();
                let mut headers = files.get_writer(first, StaticFileSegment::Headers).unwrap();
                let mut points = Vec::new();
                for height in first..=last {
                    let input = outbe_primitives::system_tx::SystemTxInputV2::CycleTick;
                    let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
                        input.kind(),
                        0,
                        height,
                        copied_native::chain().chain().id(),
                        input.encode().unwrap(),
                    )
                    .unwrap();
                    let transaction: OutbeTxEnvelope = signer.sign_unsigned(unsigned).unwrap();
                    let receipt = OutbeReceipt {
                        success: true,
                        cumulative_gas_used: 21_000,
                        ..Default::default()
                    };
                    let header = if height == 0 {
                        copied_native::chain().genesis_header().clone()
                    } else {
                        OutbeHeader::new(Header {
                            number: height,
                            parent_hash: parent,
                            timestamp: height,
                            gas_limit: 30_000_000,
                            gas_used: 21_000,
                            transactions_root: alloy_consensus::proofs::calculate_transaction_root(
                                std::slice::from_ref(&transaction),
                            ),
                            receipts_root: alloy_consensus::proofs::calculate_receipt_root(&[
                                alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
                            ]),
                            ..Default::default()
                        })
                    };
                    let hash = header.hash_slow();
                    headers.append_header(&header, &hash).unwrap();
                    tx.put::<tables::CanonicalHeaders>(height, hash).unwrap();
                    tx.put::<tables::HeaderNumbers>(hash, height).unwrap();
                    tx.put::<tables::Headers<OutbeHeader>>(height, header)
                        .unwrap();
                    tx.put::<tables::BlockBodyIndices>(
                        height,
                        StoredBlockBodyIndices {
                            first_tx_num: height.saturating_sub(1),
                            tx_count: u64::from(height != 0),
                        },
                    )
                    .unwrap();
                    writer.increment_block(height).unwrap();
                    if height != 0 {
                        writer.append_transaction(height - 1, &transaction).unwrap();
                        tx.put::<tables::Receipts<OutbeReceipt>>(height - 1, receipt)
                            .unwrap();
                    }
                    points.push(ProjectionCheckpoint {
                        block_number: height,
                        block_hash: hash,
                    });
                    parent = hash;
                }
                drop(writer);
                drop(headers);
                files.commit().unwrap();
                type Stage =
                    <tables::StageCheckpoints as reth_ethereum::provider::db::table::Table>::Value;
                for stage in ["Headers", "Bodies", "Execution", "Finish"] {
                    tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(last))
                        .unwrap();
                }
                tx.put::<tables::ChainState>(tables::ChainStateKey::LastFinalizedBlock, last)
                    .unwrap();
                tx.commit().unwrap();
                drop(files);
                drop(db);
                drop(
                    RocksDBProvider::builder(root.join("rocksdb"))
                        .with_default_tables()
                        .build()
                        .unwrap(),
                );
                points
            }

            fn native_payout_replies(
                chain_root: &Path,
                f: &Fixture,
                active: Vec<u8>,
            ) -> BTreeMap<(Address, Vec<u8>), Vec<u8>> {
                let provider = copied_native::provider(chain_root);
                let state = provider.latest().unwrap();
                let reader = OcompExExStateReaderV1 {
                    state: state.as_ref(),
                };
                let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
                    reader,
                    copied_native::chain().chain().id(),
                    copied_native::chain().genesis_hash(),
                );
                let storage = StorageHandle::new(&mut readonly);
                let mut replies = BTreeMap::new();
                let mut day: u32 = f.day.into();
                for _ in 0..=PAYOUT_LOOKBACK_DAYS {
                    let round = outbe_intex::api::certified_payout_round(&storage, day).unwrap();
                    let generation = outbe_intex::api::certified_contributor_generation(
                        &storage,
                        WorldwideDay::from(day),
                    )
                    .unwrap();
                    let value = match round {
                        Some(round) => Round {
                            amount: round.amount,
                            contributorCount: generation.as_ref().unwrap().contributor_count,
                            paidSoFar: round.paid_so_far,
                            paidLeafCount: round.paid_leaf_count,
                        },
                        None => Round {
                            amount: U256::ZERO,
                            contributorCount: 0,
                            paidSoFar: U256::ZERO,
                            paidLeafCount: 0,
                        },
                    };
                    replies.insert(
                        (
                            INTEX_FACTORY_ADDRESS,
                            contributorPayoutRoundCall { worldwideDay: day }.abi_encode(),
                        ),
                        value.abi_encode(),
                    );
                    day = outbe_primitives::time::previous_date_key(day);
                }
                let day: u32 = f.day.into();
                let g = outbe_intex::api::certified_contributor_generation(&storage, f.day)
                    .unwrap()
                    .unwrap();
                assert_eq!(g.contributor_count, 257);
                let round = outbe_intex::api::certified_payout_round(&storage, day)
                    .unwrap()
                    .unwrap();
                assert_eq!(round.paid_leaf_count, 0);
                assert_eq!(round.paid_so_far, U256::ZERO);
                replies.insert(
                    (
                        INTEX_ADDRESS,
                        certifiedContributorGenerationCall { worldwideDay: day }.abi_encode(),
                    ),
                    Certified {
                        seriesVersion: g.series_version,
                        contributorRoot: g.contributor_root,
                        contributorCount: g.contributor_count,
                        eligibleNominalTotal: g.eligible_nominal_total,
                    }
                    .abi_encode(),
                );
                let paid = outbe_intex::api::paid_leaves_word(&storage, day, 0).unwrap();
                assert_eq!(paid, U256::ZERO);
                replies.insert(
                    (
                        INTEX_FACTORY_ADDRESS,
                        contributorPaidWordCall {
                            worldwideDay: day,
                            wordIndex: 0,
                        }
                        .abi_encode(),
                    ),
                    paid.abi_encode(),
                );
                // Sole state-view exception: typed fixture response, not a Metadosis read.
                replies.insert(
                    (
                        METADOSIS_ADDRESS,
                        getActiveLysisGenerationCall { wwd: day }.abi_encode(),
                    ),
                    Bytes::from(active).abi_encode(),
                );
                replies
            }

            #[derive(Default)]
            struct RpcEvidence {
                calls: usize,
                sent: Vec<(Address, Vec<u8>, B256)>,
                round_days: Vec<u32>,
            }
            struct ScriptedRpc {
                url: String,
                address: std::net::SocketAddr,
                stopped: Arc<AtomicBool>,
                point: Arc<Mutex<ProjectionCheckpoint>>,
                evidence: Arc<Mutex<RpcEvidence>>,
                thread: Option<std::thread::JoinHandle<()>>,
            }
            impl ScriptedRpc {
                fn start(
                    point: ProjectionCheckpoint,
                    own_sender: Address,
                    replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
                    expected: BTreeMap<Address, Vec<u8>>,
                ) -> Self {
                    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                    let address = listener.local_addr().unwrap();
                    let stopped = Arc::new(AtomicBool::new(false));
                    let evidence = Arc::new(Mutex::new(RpcEvidence::default()));
                    let point = Arc::new(Mutex::new(point));
                    let stop = Arc::clone(&stopped);
                    let seen = Arc::clone(&evidence);
                    let tip = Arc::clone(&point);
                    let thread = std::thread::spawn(move || {
                        // Joining is unblocked explicitly with a loopback connection.
                        // Every accepted request also has a strict byte/time bound.
                        while !stop.load(Ordering::Acquire) {
                            let (mut stream, _) = listener.accept().unwrap();
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            stream
                                .set_read_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            let request = read_http_json(&mut stream);
                            let mut state = seen.lock().unwrap();
                            state.calls += 1;
                            assert!(state.calls <= 256, "unexpected RPC loop");
                            let point = *tip.lock().unwrap();
                            let params = &request["params"];
                            let method = request["method"].as_str().unwrap();
                            let result = match method {
                                "eth_chainId" => serde_json::json!(format!(
                                    "0x{:x}",
                                    copied_native::chain().chain().id()
                                )),
                                "eth_getTransactionCount" => {
                                    assert_eq!(
                                        params[0].as_str().unwrap().parse::<Address>().unwrap(),
                                        own_sender
                                    );
                                    assert_eq!(params[1], "latest");
                                    // Stable scripted nonce: this component does not model native
                                    // nonce advancement or order concurrent worker scheduling.
                                    serde_json::json!("0x7")
                                }
                                "eth_gasPrice" => serde_json::json!("0x1"),
                                "eth_call" => {
                                    assert_eq!(params[1], "finalized");
                                    let to = params[0]["to"]
                                        .as_str()
                                        .unwrap()
                                        .parse::<Address>()
                                        .unwrap();
                                    let data = hex::decode(
                                        params[0]["data"]
                                            .as_str()
                                            .unwrap()
                                            .strip_prefix("0x")
                                            .unwrap(),
                                    )
                                    .unwrap();
                                    if data.starts_with(&contributorPayoutRoundCall::SELECTOR) {
                                        let call =
                                            contributorPayoutRoundCall::abi_decode(&data).unwrap();
                                        state.round_days.push(call.worldwideDay);
                                    }
                                    let bytes = replies.get(&(to, data)).expect("RPC read outside native fixture/lookback or scripted ActiveGeneration");
                                    serde_json::json!(format!("0x{}", hex::encode(bytes)))
                                }
                                "eth_sendRawTransaction" => {
                                    let raw = hex::decode(
                                        params[0].as_str().unwrap().strip_prefix("0x").unwrap(),
                                    )
                                    .unwrap();
                                    let mut slice = raw.as_slice();
                                    let tx =
                                        EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut slice)
                                            .unwrap();
                                    assert!(slice.is_empty());
                                    assert!(matches!(&tx, EthereumTxEnvelope::Eip1559(_)));
                                    assert_eq!(tx.recover_signer().unwrap(), own_sender);
                                    assert_eq!(
                                        tx.chain_id(),
                                        Some(copied_native::chain().chain().id())
                                    );
                                    assert_eq!(
                                        tx.nonce(),
                                        7,
                                        "must sign the nonce returned by this RPC"
                                    );
                                    assert_eq!(tx.value(), U256::ZERO);
                                    let TxKind::Call(to) = tx.kind() else {
                                        panic!("unexpected contract creation")
                                    };
                                    assert_eq!(
                                        tx.input().as_ref(),
                                        expected
                                            .get(&to)
                                            .expect("unexpected submission destination")
                                            .as_slice()
                                    );
                                    assert!(
                                        !state.sent.iter().any(|(previous, _, _)| *previous == to),
                                        "duplicate submission"
                                    );
                                    let hash = keccak256(&raw);
                                    state.sent.push((to, raw, hash));
                                    serde_json::json!(format!("{hash:#x}"))
                                }
                                "eth_getTransactionReceipt" => {
                                    let hash = params[0].as_str().unwrap().parse::<B256>().unwrap();
                                    if state.sent.iter().any(|(_, _, sent)| *sent == hash) {
                                        // Deliberately reverted scripted receipts: proves actual delivery
                                        // and completion without pretending native state was advanced.
                                        serde_json::json!({"transactionHash": format!("{hash:#x}"), "blockNumber": format!("0x{:x}", point.block_number), "blockHash": format!("{:#x}", point.block_hash), "status":"0x0"})
                                    } else {
                                        serde_json::Value::Null
                                    }
                                }
                                "eth_getBlockByNumber" => {
                                    assert!(
                                        params[0] == "finalized"
                                            || params[0] == format!("0x{:x}", point.block_number)
                                    );
                                    serde_json::json!({"number":format!("0x{:x}", point.block_number),"hash":format!("{:#x}",point.block_hash)})
                                }
                                other => panic!("unexpected RPC method: {other}"),
                            };
                            let body = serde_json::to_vec(
                                &serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result}),
                            )
                            .unwrap();
                            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                            stream.write_all(&body).unwrap();
                        }
                    });
                    Self {
                        url: format!("http://{address}"),
                        address,
                        stopped,
                        point,
                        evidence,
                        thread: Some(thread),
                    }
                }
                fn assert_quiet(&self) {
                    let evidence = self.evidence.lock().unwrap();
                    assert_eq!(evidence.calls, 0);
                    assert!(evidence.sent.is_empty());
                }
                fn finish(mut self) {
                    self.stopped.store(true, Ordering::Release);
                    let _ = TcpStream::connect(self.address);
                    self.thread
                        .take()
                        .unwrap()
                        .join()
                        .expect("scripted RPC failed");
                }
            }
            impl Drop for ScriptedRpc {
                fn drop(&mut self) {
                    self.stopped.store(true, Ordering::Release);
                    let _ = TcpStream::connect(self.address);
                    if let Some(thread) = self.thread.take() {
                        let _ = thread.join();
                    }
                }
            }
            fn read_http_json(stream: &mut TcpStream) -> serde_json::Value {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST "));
                let mut length = None;
                let mut header_bytes = line.len();
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    header_bytes += line.len();
                    assert!(header_bytes < 16_384);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                }
                let length = length.expect("content length");
                assert!(length <= 256 * 1024);
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                serde_json::from_slice(&bytes).unwrap()
            }
            fn enable_validator<P>(
                runtime: &mut EmbeddedOcompExExV1<P>,
                public: &Path,
                bundle: &PinnedProtocolBundle,
                url: &str,
            ) {
                // Reuse the existing component fixture. Configure all policy owners
                // consistently; this does not construct another FullNode adapter.
                runtime.domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
                    domain_root: public.to_path_buf(),
                    registry_generation: 1,
                    bundles: vec![EmbeddedOcompBundleConfigV1 {
                        worker_address: "127.0.0.1:0".parse().unwrap(),
                        identity: EndpointIdentity {
                            chain_id: copied_native::chain().chain().id(),
                            genesis_hash: copied_native::chain().genesis_hash(),
                            boot_nonce: B256::repeat_byte(0x81),
                            protocol_bundle_hash: bundle.hash(),
                        },
                        protocol_bundle: bundle.clone(),
                    }],
                    policy: EmbeddedNodePolicyV1::Validator,
                    validator_rpc_url: Some(url.to_owned()),
                    limits: poc_schema_limits(),
                })
                .unwrap();
                runtime.policy = EmbeddedNodePolicyV1::Validator;
                runtime.state = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::Validator);
            }

            fn assert_no_submission_files(public: &Path) {
                fn walk(path: &Path) {
                    if !path.exists() {
                        return;
                    }
                    for entry in fs::read_dir(path).unwrap() {
                        let entry = entry.unwrap();
                        if entry.file_type().unwrap().is_dir() {
                            walk(&entry.path());
                        } else {
                            panic!("unexpected submission file {}", entry.path().display());
                        }
                    }
                }
                for relative in [
                    "supervisor-v1/materialization-submissions",
                    "supervisor-v1/payout-submissions",
                    "supervisor-v1/vote-submissions",
                ] {
                    walk(&public.join(relative));
                }
            }

            async fn exercise(validator: bool) {
                let receiver = tempfile::tempdir().unwrap();
                let public = receiver.path().join("ocomp");
                let chain_root = receiver.path().join("chain");
                let (signer, _, _) = resident_keys(&public);
                let own_evm_key = fs::read(public.join("ocomp-evm-key.hex")).unwrap();
                let own_result_key = fs::read(public.join("ocomp-key-v1.hex")).unwrap();
                let donor = tempfile::tempdir().unwrap();
                let donor_chain = donor.path().join("chain");
                let donor_public = donor.path().join("ocomp");
                let h = signed_frames(&donor_chain, 0, H, &signer)[H as usize];
                let day = outbe_primitives::time::worldwide_day_from_timestamp(H);
                let f = fixture(&donor_public, 0x71, WorldwideDay::from(day), 257);
                let leaves = artifact(&donor_public, &f);
                seed_native(&donor_chain, &f, signer.address(), &leaves);
                let mut donor_runtime = copied_native::runtime(
                    copied_native::provider(&donor_chain),
                    &donor_public,
                    f.bundle.clone(),
                );
                copied_native::catch_up(&mut donor_runtime, h);
                assert!(donor_runtime.jobs.is_empty());
                assert_eq!(donor_runtime.closure_checkpoint.current().unwrap(), h);
                drop(donor_runtime);
                // A distinct donor key makes accidental identity copying detectable.
                write_key(&donor_public.join("ocomp-evm-key.hex"), 0x61);
                write_key(&donor_public.join("ocomp-key-v1.hex"), 0x62);
                let copy = PublicCopy {
                    donor,
                    fixture: f,
                    closed: h,
                };
                copy.place_public_files(receiver.path());
                let PublicCopy {
                    donor,
                    fixture: f,
                    closed: _,
                } = copy;
                donor.close().unwrap();
                assert_eq!(
                    fs::read(public.join("ocomp-evm-key.hex")).unwrap(),
                    own_evm_key
                );
                assert_eq!(
                    fs::read(public.join("ocomp-key-v1.hex")).unwrap(),
                    own_result_key
                );
                let head = read_native_pending_head(&copied_native::provider(&chain_root));
                assert_eq!(head.next_nod_ordinal, 256);
                assert_eq!(head.nod_count, 257);
                assert_eq!(head.last_progress_height, H);
                let subtree = outbe_chain_constants::get_nod_materialization_batch_subtree_height();
                // Existing build_remaining fixture currently uses subtree height 3.
                // This is the ordinary default, not a production timing override.
                assert_eq!(subtree, 3);
                let expected_nod = encode_materialize_certified_nods_calldata(
                    &build_remaining(&public, &f, &head).unwrap().batch,
                    &poc_schema_limits(),
                )
                .unwrap();
                let expected_pay = expected_payout(&f, &leaves);
                let active = scripted_active(&f, &leaves);
                let replies = native_payout_replies(&chain_root, &f, active.clone());
                let rpc = ScriptedRpc::start(
                    h,
                    signer.address(),
                    replies.clone(),
                    BTreeMap::from([
                        (NOD_FACTORY_ADDRESS, expected_nod),
                        (INTEX_FACTORY_ADDRESS, expected_pay),
                    ]),
                );
                let mut quiet = copied_native::runtime(
                    copied_native::provider(&chain_root),
                    &public,
                    f.bundle.clone(),
                );
                if validator {
                    enable_validator(&mut quiet, &public, &f.bundle, &rpc.url);
                }
                assert_eq!(quiet.closure_checkpoint.current().unwrap(), h);
                assert!(quiet.jobs.is_empty() && quiet.requests.is_empty());
                // The quiet C=H branch in run.rs refreshes jobs; it does not call the
                // effect drivers without a new finalized frame. Empty requests model
                // pruned terminal jobs, not successful Completed verification.
                quiet.refresh_jobs(H, h.block_hash, true).await.unwrap();
                quiet.flush_closure_checkpoint().unwrap();
                assert_eq!(quiet.closure_checkpoint.current().unwrap(), h);
                assert!(quiet.materialization_active.is_none() && !quiet.payout_active);
                assert!(matches!(
                    quiet.materialization_rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ));
                assert!(matches!(
                    quiet.payout_rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ));
                rpc.assert_quiet();
                assert_no_submission_files(&public);
                drop(quiet);

                let retry = outbe_chain_constants::get_nod_materialization_retry_interval_blocks();
                assert!(retry > 0);
                let k_height = H.checked_add(retry).unwrap();
                let k = *signed_frames(&chain_root, H + 1, k_height, &signer)
                    .last()
                    .unwrap();
                assert_eq!(
                    outbe_primitives::time::worldwide_day_from_timestamp(k_height),
                    day
                );
                // Native state remains unchanged in these non-executed frame fixtures.
                // Assert the RPC table still matches copied native round/paid authority.
                assert_eq!(native_payout_replies(&chain_root, &f, active), replies);
                *rpc.point.lock().unwrap() = k;
                let mut resumed = copied_native::runtime(
                    copied_native::provider(&chain_root),
                    &public,
                    f.bundle.clone(),
                );
                if validator {
                    enable_validator(&mut resumed, &public, &f.bundle, &rpc.url);
                }
                assert_eq!(resumed.closure_checkpoint.current().unwrap(), h);
                let source = RethFinalizedFrameSource::new(resumed.provider.clone());
                let mut visited = Vec::new();
                while let Some(batch) = read_bounded_finalized_frames(
                    &source,
                    resumed.scanned_height + 1,
                    (k.block_number, k.block_hash).into(),
                )
                .unwrap()
                {
                    for frame in batch.frames() {
                        visited.push(frame.identity().number);
                        resumed.record_scanned_frame(frame).unwrap();
                        if frame.identity().number == k.block_number {
                            resumed
                                .refresh_jobs(k.block_number, k.block_hash, false)
                                .await
                                .unwrap();
                            // Actual effect entrypoints, including finalized proposer
                            // recovery, native role resolution, and detached workers.
                            resumed.reconcile_materialization(frame).unwrap();
                            resumed.drive_payout(frame.block().header.timestamp());
                        }
                    }
                    resumed.flush_closure_checkpoint().unwrap();
                }
                // The frame reader owns a provider clone. Close it before the
                // final ordinary reopen of this same native MDBX environment.
                drop(source);
                assert_eq!(visited, (H + 1..=k.block_number).collect::<Vec<_>>());
                assert_eq!(resumed.closure_checkpoint.current().unwrap(), k);
                if validator {
                    assert!(resumed.materialization_active.is_some() && resumed.payout_active);
                    // Drop the originals: after each result, Disconnected proves the
                    // producer released its channel. The isolated process bounds a
                    // worker stuck before that point; no worker join API is invented.
                    drop(std::mem::replace(
                        &mut resumed.materialization_tx,
                        std::sync::mpsc::channel().0,
                    ));
                    drop(std::mem::replace(
                        &mut resumed.payout_tx,
                        std::sync::mpsc::channel().0,
                    ));
                    let nod = resumed
                        .materialization_rx
                        .recv_timeout(Duration::from_secs(45))
                        .expect("actual materialization result");
                    match &nod {
                        EmbeddedMaterializationOutcomeV1::Finalized {
                            job_id,
                            queue_sequence,
                            first_nod_ordinal,
                            success,
                        } => {
                            assert_eq!(*job_id, f.job_id);
                            assert_eq!(*queue_sequence, 1);
                            assert_eq!(*first_nod_ordinal, 256);
                            assert!(!success);
                        }
                        EmbeddedMaterializationOutcomeV1::Unavailable { detail, .. } => {
                            panic!("actual materialization unavailable: {detail}")
                        }
                    }
                    resumed.handle_materialization(nod);
                    assert!(matches!(
                        resumed
                            .materialization_rx
                            .recv_timeout(Duration::from_secs(2)),
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                    ));
                    let payout = resumed
                        .payout_rx
                        .recv_timeout(Duration::from_secs(45))
                        .expect("actual payout result");
                    assert!(
                        matches!(&payout, EmbeddedPayoutOutcomeV1::Ticked(PayoutTickOutcomeV1::Finalized { worldwide_day, start_index: 0, success: false }) if *worldwide_day == day),
                        "actual payout did not finalize: {payout:?}"
                    );
                    resumed.handle_payout(payout);
                    assert!(matches!(
                        resumed.payout_rx.recv_timeout(Duration::from_secs(2)),
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                    ));
                    assert!(resumed.materialization_active.is_none() && !resumed.payout_active);
                    let evidence = rpc.evidence.lock().unwrap();
                    assert_eq!(evidence.sent.len(), 2);
                    // These early Unix-time frames map to the first supported
                    // day. Earlier candidate dates clamp to that same day, and
                    // the submitter stops at its first unpaid round.
                    assert_eq!(
                        evidence.round_days,
                        vec![day],
                        "the copied current unpaid round must be selected"
                    );
                } else {
                    assert!(resumed.materialization_active.is_none() && !resumed.payout_active);
                    assert!(matches!(
                        resumed.materialization_rx.try_recv(),
                        Err(std::sync::mpsc::TryRecvError::Empty)
                    ));
                    assert!(matches!(
                        resumed.payout_rx.try_recv(),
                        Err(std::sync::mpsc::TryRecvError::Empty)
                    ));
                    rpc.assert_quiet();
                    assert_no_submission_files(&public);
                    assert_eq!(resumed.domain.validator_sender_address(), None);
                }
                // Reverted scripted receipts leave both real pending owners untouched.
                assert_eq!(
                    read_native_pending_head(&resumed.provider).next_nod_ordinal,
                    256
                );
                drop(resumed);
                rpc.finish();
                let reopened = copied_native::runtime(
                    copied_native::provider(&chain_root),
                    &public,
                    f.bundle.clone(),
                );
                assert_eq!(reopened.closure_checkpoint.current().unwrap(), k);
                assert_eq!(
                    read_native_pending_head(&reopened.provider).next_nod_ordinal,
                    256
                );
                assert_eq!(
                    fs::read(public.join("ocomp-evm-key.hex")).unwrap(),
                    own_evm_key
                );
                assert_eq!(
                    fs::read(public.join("ocomp-key-v1.hex")).unwrap(),
                    own_result_key
                );
                drop(reopened);
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn copied_validator_spawns_pending_nod_and_payout_after_eligible_frame() {
                if isolated("copied_validator_spawns_pending_nod_and_payout_after_eligible_frame") {
                    exercise(true).await;
                }
            }
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn copied_fullnode_with_resident_keys_never_submits_pending_nod_or_payout() {
                if isolated(
                    "copied_fullnode_with_resident_keys_never_submits_pending_nod_or_payout",
                ) {
                    exercise(false).await;
                }
            }
        }
    }
}
