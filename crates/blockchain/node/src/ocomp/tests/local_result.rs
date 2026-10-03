use super::fixtures::canonical_result;
use alloy_primitives::B256;
use outbe_ocomp_protocol::profile::poc_schema_limits;

use crate::ocomp::local_result::{LocalLysisResultError, LocalLysisResultStore};

#[test]
fn local_result_store_survives_restart_and_accepts_exact_replay() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("local-results");
    let limits = poc_schema_limits();
    let (job_id, result, encoded) = canonical_result().expect("canonical local-result fixture");

    let store = LocalLysisResultStore::open(&store_root, limits).unwrap();
    assert_eq!(store.load(job_id).unwrap(), None);
    let committed = store.commit(job_id, &encoded).unwrap();
    assert_eq!(committed.job_id, job_id);
    assert_eq!(
        committed.result_digest,
        result.result_digest(&limits).unwrap()
    );
    assert_eq!(store.commit(job_id, &encoded).unwrap(), committed);
    drop(store);

    let reopened = LocalLysisResultStore::open(&store_root, limits).unwrap();
    let loaded = reopened
        .load(job_id)
        .unwrap()
        .expect("committed local result survives restart");
    assert_eq!(loaded.committed, committed);
    assert_eq!(loaded.canonical_result, encoded);
    reopened.verify_exact(job_id, &result).unwrap();
}

#[test]
fn local_result_store_fails_closed_for_missing_mismatch_and_conflict() {
    let root = tempfile::tempdir().unwrap();
    let store_root = root.path().join("local-results");
    let limits = poc_schema_limits();
    let (job_id, result, encoded) = canonical_result().expect("canonical local-result fixture");
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
