#[path = "../src/test_support/common.rs"]
mod fixture_support;

mod support;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{encode_tribute_v1, TributeBodyV1};
use outbe_lysis::program_v1::planner::{LysisPlanTopologyV1, PlannedUnitPositionV1};
use outbe_ocomp::{
    admission_catalog::{AdmissionPositionV1, VerifiedAdmissionCatalog},
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    input_ref_catalog::{InputRefCatalogError, VerifiedInputChunkRefCatalog},
    lysis_plan_audit::{ExactLysisPlanError, LocalLysisPlanAuditV1, LysisPlanAuditStepV1},
    lysis_result_catalog::{
        verified_result_chunk_at, ExactLysisResultCatalogCursorV1, LysisResultCatalogError,
        LysisResultCatalogStepV1,
    },
    nod_materialization::build_nod_materialization_batch,
    nod_proof::{build_certified_nod_proof, NodProofBuildError},
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    input::{AuthenticatedInputChunkV1, CheckpointIdentityV1, InputChunkKind, InputManifestV1},
    list::verify_ordered_list_membership,
    nod_materialization::NodMaterializationHeadV1,
    registry::ObjectKind,
    result::{ActiveNodSetV1, ContributorActionV1, OutputManifestEntryV1, ResultChunkV1},
    unit::{UnitArtifactV1, UnitPhase, UnitSpecV1, WorkOutputHeaderV1},
    CasObjectRefV1, ListKind, StreamingOrderedListRoot,
};
use outbe_primitives::time::WorldwideDay;

const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 64 * 1_048_576,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

struct Fixture {
    _directory: tempfile::TempDir,
    cas_root: std::path::PathBuf,
    input_ref_root: std::path::PathBuf,
    admission_root: std::path::PathBuf,
    limits: outbe_ocomp_protocol::SchemaLimits,
    bundle: PinnedProtocolBundle,
    target_ordinal: u32,
    expected_count: u32,
    result_chunk_refs: Vec<CasObjectRefV1>,
}

impl Fixture {
    fn with_audit<T>(&self, read: impl FnOnce(&LocalLysisPlanAuditV1<'_>) -> T) -> T {
        let fixture = self;

        let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
        let input_refs = VerifiedInputChunkRefCatalog::reopen(
            &fixture.input_ref_root,
            &reader,
            fixture.limits,
            poc_input_list_limits(),
        )
        .unwrap();
        let admissions =
            VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits)
                .unwrap();
        let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
            &admissions,
            &input_refs,
            &reader,
            &fixture.bundle,
            &fixture.limits,
        )
        .unwrap();
        read(&audit)
    }
    fn result_catalog_steps(&self) -> Vec<LysisResultCatalogStepV1> {
        self.with_audit(|audit| {
            ExactLysisResultCatalogCursorV1::open(audit)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
    }
    fn active_nod_set(&self) -> ActiveNodSetV1 {
        let fixture = self;
        self.with_audit(|audit| {
            let mut root =
                StreamingOrderedListRoot::new(ListKind::NodActions, audit.plan().tribute_count)
                    .unwrap();
            for step in ExactLysisResultCatalogCursorV1::open(audit).unwrap() {
                if let LysisResultCatalogStepV1::Chunk(chunk) = step.unwrap() {
                    for action in &chunk.chunk().ordered_nod_actions {
                        root.push(
                            &action.encode_canonical_record(&fixture.limits).unwrap(),
                            fixture.limits.max_bounded_bytes,
                        )
                        .unwrap();
                    }
                }
            }
            ActiveNodSetV1 {
                job_id: audit.plan().job_id,
                program_semantics_hash: fixture.bundle.bundle().lysis_program_semantics_hash,
                worldwide_day: audit.plan().wwd,
                generation: 1,
                nod_root: root.finish().unwrap(),
                nod_count: audit.plan().tribute_count,
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResultCatalogFault {
    None,
    ContributorCarrier,
    EligibleTotal,
    HeaderCoverage,
    ContributorBoundary,
    NodBoundary,
    AlternateResultChunk,
}

/// Synthetic protocol-shaped catalog fixture.
///
/// Input artifacts and result leaves use production codecs and exact CAS
/// descriptors. Other phase payloads are deliberately minimal and do not
/// claim to be evidence of a real worker-pipeline execution.
fn synthetic_fixture(substitute_bucket_spec: bool, corrupt_fidelity_root: bool) -> Fixture {
    synthetic_fixture_with_options(
        substitute_bucket_spec,
        corrupt_fidelity_root,
        ResultCatalogFault::None,
        257,
    )
}

fn synthetic_fixture_with_result_fault(
    substitute_bucket_spec: bool,
    corrupt_fidelity_root: bool,
    result_fault: ResultCatalogFault,
) -> Fixture {
    synthetic_fixture_with_options(
        substitute_bucket_spec,
        corrupt_fidelity_root,
        result_fault,
        257,
    )
}

fn synthetic_fixture_with_tribute_count(tribute_count: u32) -> Fixture {
    synthetic_fixture_with_options(false, false, ResultCatalogFault::None, tribute_count)
}

#[path = "lysis_plan_audit/fixture.rs"]
mod fixture;
use fixture::synthetic_fixture_with_options;

fn result_catalog_error_for_fault(fault: ResultCatalogFault) -> LysisResultCatalogError {
    let fixture = synthetic_fixture_with_result_fault(false, false, fault);
    let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &fixture.input_ref_root,
        &reader,
        fixture.limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits).unwrap();
    let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();
    let mut cursor = ExactLysisResultCatalogCursorV1::open(&audit).unwrap();
    loop {
        match cursor.next() {
            Some(Ok(LysisResultCatalogStepV1::Complete)) => {
                panic!("faulted result catalog must not complete")
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => return error,
            None => panic!("faulted result catalog must fail"),
        }
    }
}

fn assert_invalid_materialization_heads_are_rejected(
    audit: &LocalLysisPlanAuditV1<'_>,
    head: &NodMaterializationHeadV1,
) {
    use outbe_ocomp::nod_materialization::NodMaterializationBuildErrorV1;

    type ChangeHead = fn(&mut NodMaterializationHeadV1);
    let cases: [(&str, ChangeHead); 6] = [
        ("job", |head| head.job_id = B256::repeat_byte(0xff)),
        ("semantics", |head| {
            head.program_semantics_hash = B256::repeat_byte(0xfe)
        }),
        ("day", |head| head.worldwide_day += 1),
        ("count", |head| head.nod_count += 1),
        ("complete cursor", |head| {
            head.next_nod_ordinal = head.nod_count
        }),
        ("cursor beyond count", |head| {
            head.next_nod_ordinal = head.nod_count + 1
        }),
    ];
    for (name, change) in cases {
        let mut invalid = head.clone();
        change(&mut invalid);
        assert!(
            matches!(
                build_nod_materialization_batch(audit, &invalid, 3),
                Err(NodMaterializationBuildErrorV1::AuthorityMismatch)
            ),
            "invalid materialization {name} must fail authority validation"
        );
    }
}

#[path = "lysis_plan_audit/catalog.rs"]
mod catalog;

#[path = "lysis_plan_audit/materialization.rs"]
mod materialization;

#[path = "lysis_plan_audit/results.rs"]
mod results;
