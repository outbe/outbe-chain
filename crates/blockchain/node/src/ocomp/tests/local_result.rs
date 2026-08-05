use alloy_primitives::B256;
use outbe_metadosis::api::OcompLocalResultAuthority;
use outbe_ocomp_protocol::{profile::poc_schema_limits, result::LysisResultV1};
use std::{sync::Arc, time::Duration};

use crate::ocomp::local_result::{LocalLysisResultError, LocalLysisResultStore};

use super::attestation::fixture;

fn canonical_result() -> (B256, LysisResultV1, Vec<u8>) {
    let (_, authority, result) = fixture();
    let encoded = result
        .encode_canonical(&poc_schema_limits())
        .expect("fixture result encodes canonically");
    (authority.job_id, result, encoded)
}

#[test]
fn local_result_store_survives_restart_and_accepts_exact_replay() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("local-results");
    let limits = poc_schema_limits();
    let (job_id, result, encoded) = canonical_result();

    let store = LocalLysisResultStore::open(&store_root, limits).unwrap();
    let committed = store.commit(job_id, &encoded).unwrap();
    assert_eq!(committed.job_id, job_id);
    assert_eq!(
        committed.result_digest,
        result.result_digest(&limits).unwrap()
    );
    assert_eq!(store.commit(job_id, &encoded).unwrap(), committed);
    drop(store);

    let reopened = LocalLysisResultStore::open(&store_root, limits).unwrap();
    reopened.verify_exact(job_id, &result).unwrap();
}

#[test]
fn local_result_store_fails_closed_for_missing_mismatch_and_conflict() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("local-results");
    let limits = poc_schema_limits();
    let (job_id, result, encoded) = canonical_result();
    let store = LocalLysisResultStore::open(&store_root, limits).unwrap();

    assert!(matches!(
        store.verify_exact(job_id, &result),
        Err(LocalLysisResultError::Missing { .. })
    ));
    store.commit(job_id, &encoded).unwrap();

    let mut different = result.clone();
    different.result_chunk_list_root = B256::repeat_byte(0xE1);
    let different_bytes = different.encode_canonical(&limits).unwrap();
    assert!(matches!(
        store.commit(job_id, &different_bytes),
        Err(LocalLysisResultError::Conflict { .. })
    ));
    assert!(matches!(
        store.verify_exact(job_id, &different),
        Err(LocalLysisResultError::Mismatch { .. })
    ));
}

#[test]
fn fullnode_waits_for_exact_local_result_and_wakes_after_durable_commit() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("local-results");
    let limits = poc_schema_limits();
    let (job_id, result, encoded) = canonical_result();
    let store = Arc::new(LocalLysisResultStore::open(&store_root, limits).unwrap());

    let writer = store.clone();
    let join = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        writer.commit(job_id, &encoded).unwrap();
    });

    OcompLocalResultAuthority::verify_exact(store.as_ref(), job_id, &result, &limits)
        .expect("the production q-forming authority must wait for the exact durable commit");
    join.join().unwrap();
}
