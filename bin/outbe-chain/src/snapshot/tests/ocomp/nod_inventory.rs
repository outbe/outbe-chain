mod present_admissions;

use super::super::headers::fingerprint;
use super::{
    queued_owner, seed_nod_generation, with_owner_storage, CanonicalInventory, Incomplete,
};
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_lysis::program_v1::{
    planner::{LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedUnitPositionV1},
    result::{
        encode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1,
        RootReduceSummaryV1,
    },
};
use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
use outbe_ocomp::nod_materialization::build_nod_materialization_batch_with_references;
use outbe_ocomp::{
    admission_catalog::{AdmissionCatalogReader, AdmissionPositionV1, VerifiedAdmissionCatalog},
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
        materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind, InputManifestV1,
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
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
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
    cas_root: PathBuf,
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
    for subjects in partition_lysis_opening_subjects(&owners, &reference_isos, &limits).unwrap() {
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
    let mut nod_root = StreamingOrderedListRoot::new(ListKind::NodActions, tribute_count).unwrap();
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
        cas_root,
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

fn canonical_owner(jobs: &[(&Fixture, u32)], damage: Option<&str>) -> HashMapStorageProvider {
    let mut owner = queued_owner(0);
    StorageHandle::enter(&mut owner, |storage| {
        let nod = NodContract::new(storage.clone());
        nod.ocomp_materialization_tail_sequence
            .write(jobs.len() as u64 + 1)
            .unwrap();
        for (index, (f, start)) in jobs.iter().enumerate() {
            let sequence = index as u64 + 1;
            let mut p = seed_nod_generation(storage.clone(), f.day, sequence);
            p.job_id = f.job_id;
            p.program_semantics_hash = f.bundle.bundle().lysis_program_semantics_hash;
            p.protocol_bundle_hash = f.bundle.hash();
            p.nod_root = f.nod_root;
            p.bucket_root = f.bucket_root;
            p.output_manifest_root = f.output_manifest_root;
            p.tribute_count = f.nod_count;
            p.nod_count = f.nod_count;
            p.bucket_count = f.nod_count;
            p.nod_amount_total = U256::from(f.nod_count) * U256::from(2);
            p.lysis_allocation_minor = U256::from(f.nod_count);
            p.next_nod_ordinal = *start;
            if let Some(damage) = damage {
                match damage {
                    "root" => p.nod_root = hash(0x99),
                    "job" => p.job_id = hash(0x99),
                    "bundle" => p.protocol_bundle_hash = hash(0x99),
                    _ => unreachable!(),
                }
            }
            write_projection(&nod, &p);
        }
    });
    owner
}

fn write_projection(nod: &NodContract<'_>, p: &NodCertifiedGenerationProjection) {
    let day = p.worldwide_day;
    nod.ocomp_namespace_root.write(&day, p.nod_root).unwrap();
    nod.ocomp_bucket_root.write(&day, p.bucket_root).unwrap();
    nod.ocomp_output_manifest_root
        .write(&day, p.output_manifest_root)
        .unwrap();
    nod.ocomp_generation_metadata
        .write(&day, p.metadata_word())
        .unwrap();
    nod.ocomp_nod_amount_total
        .write(&day, p.nod_amount_total)
        .unwrap();
    nod.ocomp_lysis_allocation_minor
        .write(&day, p.lysis_allocation_minor)
        .unwrap();
    nod.ocomp_materialization_job_id
        .write(&day, p.job_id)
        .unwrap();
    nod.ocomp_materialization_protocol_bundle_hash
        .write(&day, p.protocol_bundle_hash)
        .unwrap();
    nod.ocomp_materialization_program_semantics_hash
        .write(&day, p.program_semantics_hash)
        .unwrap();
    nod.ocomp_materialization_next_nod_ordinal
        .write(&day, p.next_nod_ordinal)
        .unwrap();
}

fn object_path(f: &Fixture, chunk: usize) -> PathBuf {
    let digest = hex::encode(f.result_chunk_refs[chunk].transport_digest);
    f.cas_root
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..])
}

fn first_native_batch(root: &Path, f: &Fixture) -> usize {
    let limits = poc_schema_limits();
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let job = hex::encode(f.job_id);
    let inputs = VerifiedInputChunkRefCatalog::reopen(
        root.join("exporter-v1/input-refs").join(&job),
        &cas,
        limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions = AdmissionCatalogReader::open_existing(
        root.join("supervisor-v1/jobs")
            .join(&job)
            .join("admissions"),
        &cas,
        limits,
    )
    .unwrap();
    let audit =
        LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &limits)
            .unwrap();
    let head = NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: f.job_id,
        program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
        worldwide_day: f.day.value(),
        generation: 1,
        nod_root: f.nod_root,
        nod_count: f.nod_count,
        next_nod_ordinal: 0,
        last_progress_height: 90,
    };
    build_nod_materialization_batch_with_references(&audit, &head, 3)
        .unwrap()
        .batch
        .actions
        .len()
}

mod lifecycle;

mod availability;

mod bounds;

mod authority;
