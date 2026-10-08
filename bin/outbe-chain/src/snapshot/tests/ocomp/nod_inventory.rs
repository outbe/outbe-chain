mod present_admissions;

use super::super::headers::fingerprint;
use super::{
    queued_owner, seed_nod_generation, with_owner_storage, CanonicalInventory, Incomplete,
};
use alloy_primitives::{B256, U256};
use outbe_lysis::program_v1::planner::{LysisPlanTopologyV1, PlannedUnitPositionV1};
use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
use outbe_ocomp::nod_materialization::build_nod_materialization_batch_with_references;
use outbe_ocomp::{
    admission_catalog::{AdmissionCatalogReader, AdmissionPositionV1, VerifiedAdmissionCatalog},
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::poc_input_list_limits,
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    lysis_plan_audit::LocalLysisPlanAuditV1,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    input::InputManifestV1,
    registry::ObjectKind,
    result::{OutputManifestEntryV1, ResultChunkV1},
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
// support structural/proof tests. This is not real worker-pipeline E2E evidence.
fn protocol_bundle() -> ProtocolBundleV1 {
    outbe_ocomp::test_support::protocol_bundle_fixture()
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

    let tributes = outbe_ocomp::test_support::tribute_population(day, tribute_count);
    let contributors_by_owner = outbe_ocomp::test_support::contributor_population(&tributes);
    let nod_action_tributes = tributes.clone();
    let openings =
        outbe_ocomp::test_support::fixture_openings(&bundle, job_id, day, &tributes, &limits);
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
    let outbe_ocomp::snapshot_test_support::PublishedFixturePlan {
        plan,
        plan_ref,
        manifest_ref,
    } = outbe_ocomp::snapshot_test_support::publish_fixture_plan(
        &cas,
        &input_ref_root,
        outbe_ocomp::snapshot_test_support::FixturePlanInputs {
            bundle: &bundle,
            job_id,
            day,
            tributes: &tributes,
            fidelity_openings: openings.fidelity,
            oracle_opening: openings.oracle,
        },
    );

    let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
    let input_refs =
        VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
            .unwrap();
    let mut admissions =
        VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
            .unwrap();
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
    let plan_hash = plan.plan_hash(&limits).unwrap();
    let nod_root = StreamingOrderedListRoot::new(ListKind::NodActions, tribute_count).unwrap();
    let result_chunk_refs = Vec::new();
    let output_manifest_root = StreamingOrderedListRoot::new(
        ListKind::CompleteOutputManifest,
        plan.primary_work_unit_count,
    )
    .unwrap();
    let bucket_root =
        StreamingOrderedListRoot::new(ListKind::BucketRecords, tribute_count).unwrap();

    let mut output = ExpectedOutputs {
        nod_root,
        bucket_root,
        output_manifest_root,
        result_chunk_refs,
    };
    let page_inputs = RootPageInputs {
        bundle_hash,
        job_id,
        plan_hash,
        tributes: &tributes,
        nod_action_tributes: &nod_action_tributes,
        contributors_by_owner: &contributors_by_owner,
        limits: &limits,
    };

    FixtureAdmission {
        admissions: &mut admissions,
        input_refs: &input_refs,
        reader: &reader,
        bundle: &pinned_bundle,
        cas: &cas,
    }
    .populate(topology, &page_inputs, &mut output);
    drop(admissions);
    drop(input_refs);
    drop(reader);
    drop(cas);

    Fixture {
        cas_root,
        job_id,
        day,
        bundle: pinned_bundle,
        nod_root: output.nod_root.finish().unwrap(),
        bucket_root: output.bucket_root.finish().unwrap(),
        output_manifest_root: output.output_manifest_root.finish().unwrap(),
        nod_count: tribute_count,
        result_chunk_refs: output.result_chunk_refs,
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

struct ExpectedOutputs {
    nod_root: StreamingOrderedListRoot,
    bucket_root: StreamingOrderedListRoot,
    output_manifest_root: StreamingOrderedListRoot,
    result_chunk_refs: Vec<CasObjectRefV1>,
}
struct RootPageInputs<'a> {
    bundle_hash: B256,
    job_id: B256,
    plan_hash: B256,
    tributes: &'a [outbe_compressed_entities::TributeBodyV1],
    nod_action_tributes: &'a [outbe_compressed_entities::TributeBodyV1],
    contributors_by_owner: &'a [outbe_ocomp_protocol::result::ContributorActionV1],
    limits: &'a outbe_ocomp_protocol::SchemaLimits,
}
fn root_page_artifact(
    spec: &outbe_ocomp_protocol::unit::UnitSpecV1,
    index: u32,
    inputs: &RootPageInputs<'_>,
    cas: &FilesystemCas,
    output: &mut ExpectedOutputs,
) -> (UnitArtifactV1, Option<OutputManifestEntryV1>) {
    let RootPageInputs {
        bundle_hash,
        job_id,
        plan_hash,
        tributes,
        nod_action_tributes,
        contributors_by_owner,
        limits,
    } = *inputs;
    let start = usize::try_from(index * 256).unwrap();
    let end = (start + 256).min(tributes.len());
    let actions = outbe_ocomp::test_support::nod_actions(
        &nod_action_tributes[start..end],
        u32::try_from(start).unwrap(),
    );
    for action in &actions {
        output
            .nod_root
            .push(
                &action.encode_canonical_record(limits).unwrap(),
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
    let chunk_hash = chunk.result_chunk_hash(limits).unwrap();
    let mut chunk_ref = cas
        .publish_bytes(&chunk.encode_canonical(limits).unwrap())
        .unwrap();
    chunk_ref.expected_ocb1_kind = Some(ObjectKind::ResultChunkV1.tag());
    output.result_chunk_refs.push(chunk_ref.clone());
    let entry = OutputManifestEntryV1 {
        chunk_ordinal: index,
        result_chunk_hash: chunk_hash,
        result_chunk_ref: chunk_ref,
    };
    output
        .output_manifest_root
        .push(
            &entry.encode_canonical_record(limits).unwrap(),
            limits.max_bounded_bytes,
        )
        .unwrap();
    let bucket_records = (start..end)
        .map(|ordinal| ordinal.to_be_bytes().to_vec())
        .collect::<Vec<_>>();
    for record in &bucket_records {
        output
            .bucket_root
            .push(record, limits.max_bounded_bytes)
            .unwrap();
    }
    let summary = outbe_ocomp::test_support::root_summary_fixture(
        plan_hash,
        &chunk,
        &tributes[start..end],
        &entry,
        limits,
    );
    let coverage_root = summary.result_chunk_hashes.tree_root;
    let output_coverage_root = coverage_root;
    (
        outbe_ocomp::test_support::root_leaf_artifact(
            spec,
            summary,
            &entry,
            output_coverage_root,
            limits,
        ),
        Some(entry),
    )
}

struct FixtureAdmission<'a> {
    admissions: &'a mut VerifiedAdmissionCatalog,
    input_refs: &'a VerifiedInputChunkRefCatalog,
    reader: &'a FilesystemCasReader,
    bundle: &'a PinnedProtocolBundle,
    cas: &'a FilesystemCas,
}
impl FixtureAdmission<'_> {
    fn populate(
        &mut self,
        topology: LysisPlanTopologyV1,
        page_inputs: &RootPageInputs<'_>,
        output: &mut ExpectedOutputs,
    ) {
        for plan_ordinal in 0..topology.total_unit_count() {
            let spec = {
                let audit = LocalLysisPlanAuditV1::open(
                    self.admissions,
                    self.input_refs,
                    self.reader,
                    self.bundle,
                    page_inputs.limits,
                )
                .unwrap();
                audit.candidate_spec_at(plan_ordinal).unwrap()
            };
            let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
                PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    level: 0,
                    index,
                } => root_page_artifact(&spec, index, page_inputs, self.cas, output),
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
                        page_inputs.limits,
                    )
                    .unwrap(),
                    None,
                ),
            };
            let mut artifact_ref = self
                .cas
                .publish_bytes(&artifact.encode_canonical(page_inputs.limits).unwrap())
                .unwrap();
            artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
            self.admissions
                .admit_verified_unit(
                    AdmissionPositionV1 { plan_ordinal },
                    &spec,
                    artifact_ref,
                    result_entry,
                )
                .unwrap();
        }
    }
}
