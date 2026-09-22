use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    hash::hash_framed,
    intent::DayType,
    profile::poc_schema_limits,
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
};

use crate::ocomp::local_result::{LocalLysisResultReader, LocalLysisResultStore};

fn canonical_result() -> (B256, LysisResultV1, Vec<u8>) {
    let limits = poc_schema_limits();
    let roots = ResultRootsV1 {
        nod_root: B256::repeat_byte(0x31),
        bucket_root: B256::repeat_byte(0x32),
        contributor_root: B256::repeat_byte(0x33),
        output_manifest_root: B256::repeat_byte(0x34),
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
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: U256::ZERO,
        desis_limit_minor: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        carry_over_credit: U256::ZERO,
        nod_cost_total: U256::ZERO,
    };
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: B256::repeat_byte(0x35),
        plan_hash: B256::repeat_byte(0x36),
        unit_artifact_root: B256::repeat_byte(0x37),
        fidelity_fraction_root: B256::repeat_byte(0x38),
        gratis_prefix_root: B256::repeat_byte(0x39),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    let job_id = B256::repeat_byte(0x21);
    let result = LysisResultV1 {
        protocol_bundle_hash: B256::repeat_byte(0x20),
        job_id,
        attempt: 0,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: B256::repeat_byte(0x3a),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: 1,
            reason: CarryOverReason::UnusedLysis,
            amount: U256::ZERO,
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: 1,
            pending_nonce: 0,
            day_type: DayType::Green,
            tribute_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: U256::ZERO,
            desis_limit_minor: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            carry_over_credit: U256::ZERO,
            status: CompletionStatus::Completed,
            logical_evaluation_height: 1,
            logical_evaluation_time: 1,
        },
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(&limits).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    };
    let encoded = result
        .encode_canonical(&limits)
        .expect("fixture result encodes canonically");
    (job_id, result, encoded)
}

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

fn fingerprint(root: &Path) -> Vec<(String, u32, Vec<u8>)> {
    let mut entries = vec![(
        String::new(),
        fs::metadata(root).unwrap().permissions().mode(),
        vec![],
    )];
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let metadata = fs::symlink_metadata(entry.path()).unwrap();
        let bytes = if metadata.file_type().is_symlink() {
            fs::read_link(entry.path())
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            fs::read(entry.path()).unwrap()
        };
        entries.push((
            entry.file_name().to_str().unwrap().into(),
            metadata.permissions().mode(),
            bytes,
        ));
    }
    entries.sort();
    entries
}

#[test]
fn reader_loads_native_results_and_missing_jobs_without_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("results");
    let limits = poc_schema_limits();
    let (job, result, encoded) = canonical_result();
    let writer = LocalLysisResultStore::open(&root, limits).unwrap();
    let committed = writer.commit(job, &encoded).unwrap();
    drop(writer);
    let before = fingerprint(&root);
    let reader = LocalLysisResultReader::open_existing(&root, limits).unwrap();
    let loaded = reader.load(job).unwrap().unwrap();
    assert_eq!(loaded.committed, committed);
    assert_eq!(loaded.canonical_result, encoded);
    assert_eq!(
        loaded.committed.result_digest,
        result.result_digest(&limits).unwrap()
    );
    assert!(reader.load(B256::repeat_byte(0x77)).unwrap().is_none());
    drop(reader);
    assert_eq!(fingerprint(&root), before);
}

#[test]
fn absent_reader_root_is_never_created() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("absent");
    assert!(LocalLysisResultReader::open_existing(&root, poc_schema_limits()).is_err());
    assert!(!root.exists());
}

#[test]
fn pending_conflict_corruption_and_foreign_jobs_are_observed_without_recovery() {
    for damage in [
        "pending",
        "linked-pending",
        "conflict",
        "foreign",
        "malformed",
        "oversized",
        "symlink",
        "wrong-mode",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("results");
        let (job, _, encoded) = canonical_result();
        let writer = LocalLysisResultStore::open(&root, poc_schema_limits()).unwrap();
        writer.commit(job, &encoded).unwrap();
        drop(writer);
        let path = root.join(format!("{}.lysis-result-v1.ocb1", hex::encode(job)));
        let pending = root.join(format!(".{}.pending", hex::encode(job)));
        match damage {
            "pending" => fs::rename(&path, &pending).unwrap(),
            "linked-pending" => fs::hard_link(&path, &pending).unwrap(),
            "conflict" => {
                fs::write(&pending, b"conflicting pending").unwrap();
                fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "foreign" => fs::rename(
                &path,
                root.join(format!(
                    "{}.lysis-result-v1.ocb1",
                    hex::encode(B256::repeat_byte(0x55))
                )),
            )
            .unwrap(),
            "malformed" => fs::write(&path, b"not an OCB1 result").unwrap(),
            "oversized" => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(poc_schema_limits().codec.max_body_bytes as u64 + 1024)
                .unwrap(),
            "symlink" => {
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink("absent", &path).unwrap();
            }
            "wrong-mode" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            _ => unreachable!(),
        }
        let before = fingerprint(&root);
        assert!(
            LocalLysisResultReader::open_existing(&root, poc_schema_limits()).is_err(),
            "{damage}"
        );
        assert_eq!(fingerprint(&root), before, "{damage}");
    }
}

#[test]
fn reader_checks_records_when_loaded_not_only_when_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("results");
    let (job, _, encoded) = canonical_result();
    let writer = LocalLysisResultStore::open(&root, poc_schema_limits()).unwrap();
    writer.commit(job, &encoded).unwrap();
    drop(writer);
    let reader = LocalLysisResultReader::open_existing(&root, poc_schema_limits()).unwrap();
    let path = root.join(format!("{}.lysis-result-v1.ocb1", hex::encode(job)));
    fs::write(path, b"changed source").unwrap();
    let before = fingerprint(&root);
    assert!(reader.load(job).is_err());
    assert_eq!(fingerprint(&root), before);
}
