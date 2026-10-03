use super::super::with_prepared_owner_storage;
use super::{canonical_job, stored_job};
use crate::{
    snapshot::{tests::headers::fingerprint, validation::ocomp::verify_local_result},
    OutbeHeader,
};
use alloy_primitives::{B256, U256};
use outbe_node::ocomp::local_result::LocalLysisResultStore;
use outbe_ocomp_protocol::{
    hash::hash_framed,
    profile::poc_schema_limits,
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisResultV1,
        MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    state::{OcompJobRecordV1, OcompJobStatus},
};
use outbe_primitives::time::WorldwideDay;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn refresh_arithmetic(result: &mut LysisResultV1) {
    result.arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &result
            .arithmetic_summary()
            .encode_canonical(&poc_schema_limits())
            .unwrap(),
    )
    .unwrap();
    result.encode_canonical(&poc_schema_limits()).unwrap();
}

// Compact native-result fixture, bound to the actual canonical JobIntent
// and B-derived JobId. This proves stored evidence, not worker execution.
pub(super) fn result_for(job: &OcompJobRecordV1) -> LysisResultV1 {
    let intent = &job.intent;
    let frozen = &intent.frozen_metadosis_values;
    let unused = frozen.lysis_limit_minor;
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: intent.authenticated_day_nominal,
        eligible_nominal_total: U256::ZERO,
        day_limit: frozen.day_limit,
        gratis_demand: frozen.gratis_demand,
        day_gratis_limit_minor: frozen.day_gratis_limit_minor,
        lysis_limit_minor: frozen.lysis_limit_minor,
        desis_limit_minor: frozen.desis_limit_minor,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: unused,
        carry_over_credit: unused,
        nod_cost_total: U256::ZERO,
    };
    let mut result = LysisResultV1 {
        protocol_bundle_hash: intent.protocol_bundle_hash,
        job_id: job.finalized.as_ref().unwrap().job_id,
        attempt: intent.attempt,
        input_manifest_hash: B256::repeat_byte(0x35),
        plan_hash: B256::repeat_byte(0x36),
        unit_artifact_root: B256::repeat_byte(0x37),
        fidelity_fraction_root: B256::repeat_byte(0x38),
        gratis_prefix_root: B256::repeat_byte(0x39),
        result_chunk_count: 1,
        result_chunk_list_root: B256::repeat_byte(0x3a),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: intent.wwd,
            reason: CarryOverReason::UnusedLysis,
            amount: unused,
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
            unused_lysis_limit_minor: unused,
            carry_over_credit: unused,
            status: CompletionStatus::Completed,
            logical_evaluation_height: intent.logical_evaluation_height,
            logical_evaluation_time: intent.logical_evaluation_time,
        },
        tribute_count: intent.authenticated_day_count,
        tribute_nominal_total: intent.authenticated_day_nominal,
        unused_lysis_limit_minor: unused,
        roots: ResultRootsV1 {
            nod_root: B256::repeat_byte(0x31),
            bucket_root: B256::repeat_byte(0x32),
            contributor_root: B256::repeat_byte(0x33),
            output_manifest_root: B256::repeat_byte(0x34),
        },
        counts: ExactCountsV1 {
            tribute_count: intent.authenticated_day_count,
            nod_count: intent.authenticated_day_count,
            bucket_count: 0,
            contributor_count: 0,
            semantic_event_count: 0,
        },
        conservation,
        arithmetic_commitment: B256::ZERO,
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    };
    refresh_arithmetic(&mut result);
    result.validate_finalized_intent(intent).unwrap();
    result
}

pub(super) fn authority(request: &OutbeHeader, completed: bool) -> OcompJobRecordV1 {
    let limits = poc_schema_limits();
    let mut job = canonical_job(request, completed);
    if completed {
        let result = result_for(&job);
        let digest = result.result_digest(&limits).unwrap();
        job.finalized
            .as_mut()
            .unwrap()
            .quorum
            .as_mut()
            .unwrap()
            .result_digest = digest;
        let binding = job
            .terminal
            .as_mut()
            .unwrap()
            .completed_binding
            .as_mut()
            .unwrap();
        binding.result_digest = digest;
        binding.result_evidence_hash = result.result_evidence_hash(&limits).unwrap();
        binding.terminal_receipt.binding.result_digest = digest;
        binding.terminal_receipt.event_summary_hash = result.event_summary_hash;
        binding.terminal_receipt_hash = binding
            .terminal_receipt
            .terminal_receipt_hash(&limits)
            .unwrap();
    } else {
        job.status = OcompJobStatus::VotingOpen;
    }
    job.validate_semantics(&limits).unwrap();
    job
}

fn with_authority(version: u32, completed: bool, check: impl FnOnce(OcompJobRecordV1)) {
    with_prepared_owner_storage(
        version,
        400,
        |request| {
            let job = authority(request, completed);
            stored_job(
                job.intent.intent_id(&poc_schema_limits()).unwrap(),
                &job.encode_canonical(&poc_schema_limits()).unwrap(),
            )
        },
        |state, view| {
            let expected = authority(&view.header(100).unwrap().unwrap(), completed);
            let actual = state
                .metadosis_job(
                    expected.intent.intent_id(&poc_schema_limits()).unwrap(),
                    WorldwideDay::new(expected.intent.wwd),
                    Some(expected.finalized.as_ref().unwrap().job_id),
                )
                .unwrap();
            assert_eq!(actual, expected);
            check(actual);
        },
    );
}

fn result_root(root: &Path) -> PathBuf {
    root.join("node-v1/local-results")
}

pub(super) fn write_result(root: &Path, result: &LysisResultV1) -> PathBuf {
    fs::create_dir_all(root.join("node-v1")).unwrap();
    let directory = result_root(root);
    let writer = LocalLysisResultStore::open(&directory, poc_schema_limits()).unwrap();
    writer
        .commit(
            result.job_id,
            &result.encode_canonical(&poc_schema_limits()).unwrap(),
        )
        .unwrap();
    drop(writer);
    directory.join(format!(
        "{}.lysis-result-v1.ocb1",
        hex::encode(result.job_id)
    ))
}

fn inspect(root: &Path, job: &OcompJobRecordV1) -> eyre::Result<Option<(LysisResultV1, bool)>> {
    let before = fingerprint(root);
    let result = verify_local_result(root, job)
        .map(|observation| observation.map(|audit| (audit.result, audit.terminal_digest_checked)));
    assert_eq!(
        fingerprint(root),
        before,
        "local result inspection mutated source"
    );
    result
}

#[test]
fn voting_open_accepts_native_local_result_without_terminal_or_old_intermediates() {
    for version in [1, 2] {
        with_authority(version, false, |job| {
            assert_eq!(job.status, OcompJobStatus::VotingOpen);
            assert!(job.terminal.is_none());
            let public = tempfile::tempdir().unwrap();
            let result = result_for(&job);
            write_result(public.path(), &result);
            assert_eq!(inspect(public.path(), &job).unwrap(), Some((result, false)));
            assert!(!public.path().join("exporter-v1").exists());
            assert!(!public.path().join("supervisor-v1").exists());
        });
    }
}

#[test]
fn completed_binding_compares_exact_native_digest_without_plan_or_admissions() {
    for version in [1, 2] {
        with_authority(version, true, |job| {
            let public = tempfile::tempdir().unwrap();
            let result = result_for(&job);
            write_result(public.path(), &result);
            assert_eq!(inspect(public.path(), &job).unwrap(), Some((result, true)));
            assert!(!public.path().join("exporter-v1").exists());
            assert!(!public.path().join("supervisor-v1").exists());
        });
    }
}

#[test]
fn absent_optional_root_or_job_is_none_even_after_canonical_completion() {
    for completed in [false, true] {
        with_authority(1, completed, |job| {
            let public = tempfile::tempdir().unwrap();
            assert_eq!(inspect(public.path(), &job).unwrap(), None);
            assert!(!public.path().join("node-v1").exists());
            fs::create_dir(public.path().join("node-v1")).unwrap();
            let writer =
                LocalLysisResultStore::open(result_root(public.path()), poc_schema_limits())
                    .unwrap();
            drop(writer);
            assert_eq!(inspect(public.path(), &job).unwrap(), None);
            // A valid result for a different job does not fabricate this
            // job's missing result, and is not a filename corruption.
            let mut unrelated = result_for(&job);
            unrelated.job_id = B256::repeat_byte(0x91);
            refresh_arithmetic(&mut unrelated);
            write_result(public.path(), &unrelated);
            assert_eq!(inspect(public.path(), &job).unwrap(), None);
        });
    }
}

#[test]
fn semantically_valid_local_result_with_different_terminal_digest_fails() {
    with_authority(1, true, |job| {
        let public = tempfile::tempdir().unwrap();
        let mut changed = result_for(&job);
        changed.input_manifest_hash = B256::repeat_byte(0x92);
        refresh_arithmetic(&mut changed);
        changed.validate_finalized_intent(&job.intent).unwrap();
        assert_ne!(
            changed.result_digest(&poc_schema_limits()).unwrap(),
            job.terminal
                .as_ref()
                .unwrap()
                .completed_binding
                .as_ref()
                .unwrap()
                .result_digest
        );
        write_result(public.path(), &changed);
        assert!(inspect(public.path(), &job).is_err());
    });
}

#[test]
fn matching_job_id_does_not_authorize_foreign_intent_fields() {
    with_authority(1, false, |job| {
        for field in 0..5 {
            let public = tempfile::tempdir().unwrap();
            let mut changed = result_for(&job);
            match field {
                0 => changed.protocol_bundle_hash = B256::repeat_byte(0x93),
                1 => changed.attempt += 1,
                2 => changed.metadosis_completion_summary.pending_nonce += 1,
                3 => changed.metadosis_completion_summary.logical_evaluation_time += 1,
                _ => {
                    changed.metadosis_completion_summary.wwd += 1;
                    changed.carry_over_credit.source_wwd += 1;
                }
            }
            refresh_arithmetic(&mut changed);
            assert!(changed.validate_finalized_intent(&job.intent).is_err());
            write_result(public.path(), &changed);
            assert!(
                inspect(public.path(), &job).is_err(),
                "accepted foreign field {field}"
            );
        }
    });
}

#[test]
fn pending_publication_corrupt_bytes_and_foreign_filename_are_not_repaired() {
    with_authority(1, false, |job| {
        for damage in ["pending", "linked-pending", "foreign", "corrupt"] {
            let public = tempfile::tempdir().unwrap();
            let result = result_for(&job);
            let path = write_result(public.path(), &result);
            let pending =
                result_root(public.path()).join(format!(".{}.pending", hex::encode(result.job_id)));
            match damage {
                "pending" => fs::rename(&path, &pending).unwrap(),
                "linked-pending" => fs::hard_link(&path, &pending).unwrap(),
                "foreign" => {
                    let foreign = result_root(public.path()).join(format!(
                        "{}.lysis-result-v1.ocb1",
                        hex::encode(B256::repeat_byte(0x94))
                    ));
                    fs::rename(&path, foreign).unwrap();
                }
                _ => fs::write(&path, b"invalid canonical result").unwrap(),
            }
            assert!(inspect(public.path(), &job).is_err(), "accepted {damage}");
            if damage == "pending" || damage == "linked-pending" {
                assert!(pending.exists());
            }
        }
    });
}
