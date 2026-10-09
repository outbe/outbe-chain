mod rejections;

use super::*;
use alloy_primitives::address;

fn finalization_record(hash_byte: u8, height: u64) -> CertifiedParentProofRecord {
    CertifiedParentProofRecord {
        kind: ProofKind::Finalization {
            finalized_block_number: height,
        },
        finalized_block_hash: B256::with_last_byte(hash_byte),
        finalized_epoch: 1,
        finalized_view: 100,
        parent_view: 99,
        ordered_committee: vec![address!("0x1111111111111111111111111111111111111111")],
        signer_bitmap: vec![1],
        encoded_proof: Bytes::from_static(b"cert"),
        ..CertifiedParentProofRecord::default()
    }
}

fn notarization_record(hash_byte: u8, _height: u64) -> CertifiedParentProofRecord {
    CertifiedParentProofRecord {
        kind: ProofKind::CertifiedNotarization,
        finalized_block_hash: B256::with_last_byte(hash_byte),
        finalized_epoch: 1,
        finalized_view: 100,
        parent_view: 99,
        ordered_committee: vec![address!("0x2222222222222222222222222222222222222222")],
        signer_bitmap: vec![3],
        encoded_proof: Bytes::from_static(b"notar"),
        ..CertifiedParentProofRecord::default()
    }
}

fn key(hash_byte: u8) -> CertifiedParentProofKey {
    CertifiedParentProofKey::new(1, 100, B256::with_last_byte(hash_byte))
}

/// Pins the dual-semantic invariants that the per-proof-type `kind` split
/// retired:
/// - A `Finalization` record carries its own height.
/// - A `CertifiedNotarization` witness carries none. The selector resolves it
///   to the parent height passed to `to_v2_metadata`. This replaces the former
///   sentinel-0-then-mutate promotion.
/// - `is_certification_witness` and `proof_kind` now derive from the variant,
///   not from stored bools/fields.
#[test]
fn proof_kind_retires_dual_semantic_fields() {
    let fin = finalization_record(0xAA, 41);
    assert_eq!(fin.finalized_block_number(), Some(41));
    assert_eq!(fin.proof_kind(), ParentParticipationProof::Finalization);
    assert!(!fin.is_certification_witness());
    assert_eq!(fin.to_v2_metadata(41).finalized_block_number, 41);

    let cn = notarization_record(0xBB, 7);
    assert_eq!(
        cn.finalized_block_number(),
        None,
        "a certified-notarization witness carries no block number of its own"
    );
    assert_eq!(
        cn.proof_kind(),
        ParentParticipationProof::CertifiedNotarization
    );
    assert!(cn.is_certification_witness());
    // The selector promotes the witness to the supplied parent height.
    let meta = cn.to_v2_metadata(99);
    assert_eq!(meta.finalized_block_number, 99);
    assert_eq!(
        meta.proof_kind,
        ParentParticipationProof::CertifiedNotarization
    );
}

#[test]
fn prune_above_height_drops_only_ahead_finalization_records() {
    let store = FinalizedParentCertStore::new();
    store
        .put_finalization(finalization_record(0xAA, 50))
        .unwrap();
    store
        .put_finalization(finalization_record(0xBB, 100))
        .unwrap();
    // CN witness carries a consensus round, not a height.
    store
        .put_certified_notarization(notarization_record(0xCC, 100))
        .unwrap();

    // Recovered finalized height = 60: the height-100 finalization record is
    // ahead of the view, so the prune drops it. The height-50 record stays.
    let dropped = store.prune_above_height(60).unwrap();
    assert_eq!(dropped, 1);
    assert!(store.get_finalization(key(0xAA)).is_some());
    assert!(store.get_finalization(key(0xBB)).is_none());
    // The CertifiedNotarization slot is view-keyed, not height-comparable, so
    // an above-height prune never touches it.
    assert!(store.get_certified_notarization(key(0xCC)).is_some());
}

#[test]
fn put_finalization_get_finalization_roundtrip() {
    let store = FinalizedParentCertStore::new();
    let r = finalization_record(0xAA, 100);
    store.put_finalization(r.clone()).unwrap();
    assert_eq!(store.get_finalization(key(0xAA)), Some(r));
    assert_eq!(store.get_finalization(key(0xBB)), None);
    assert_eq!(store.len(), 1);
}

#[test]
fn put_certified_notarization_get_certified_notarization_exact_key_only() {
    let store = FinalizedParentCertStore::new();
    let r = notarization_record(0xAA, 100);
    store.put_certified_notarization(r.clone()).unwrap();
    // exact-key lookup only - no fuzzy match.
    assert_eq!(store.get_certified_notarization(key(0xAA)), Some(r));
    assert_eq!(store.get_certified_notarization(key(0xBB)), None);
}

#[test]
fn put_finalization_get_best_parent_proof_returns_finalization_first() {
    // best-proof finalization-first.
    let store = FinalizedParentCertStore::new();
    store
        .put_certified_notarization(notarization_record(0xAA, 100))
        .unwrap();
    store
        .put_finalization(finalization_record(0xAA, 100))
        .unwrap();
    let best = store.get_best_parent_proof(key(0xAA)).unwrap();
    assert_eq!(best.proof_kind(), ParentParticipationProof::Finalization);
}

#[test]
fn get_best_for_parent_is_single_snapshot_and_reports_cn_promotion_need() {
    let store = FinalizedParentCertStore::new();
    let witness = notarization_record(0xAA, 0);
    store.put_certified_notarization(witness).unwrap();

    let best = store.get_best_for_parent(key(0xAA), 100).unwrap();
    assert!(matches!(
        best,
        ParentProofSelection::CertifiedNotarization(_)
    ));

    store
        .put_finalization(finalization_record(0xAA, 100))
        .unwrap();
    let best = store.get_best_for_parent(key(0xAA), 100).unwrap();
    assert!(matches!(best, ParentProofSelection::Finalization(_)));
}

#[test]
fn get_best_parent_proof_falls_back_to_certified_notarization() {
    let store = FinalizedParentCertStore::new();
    store
        .put_certified_notarization(notarization_record(0xAA, 100))
        .unwrap();
    let best = store.get_best_parent_proof(key(0xAA)).unwrap();
    assert_eq!(
        best.proof_kind(),
        ParentParticipationProof::CertifiedNotarization
    );
}

#[test]
fn local_certification_witness_is_separate_from_persistent_cn_lookup() {
    let store = FinalizedParentCertStore::new();
    assert!(!store.has_local_certification_witness(key(0xAA)));
    store
        .put_certified_notarization(notarization_record(0xAA, 100))
        .unwrap();
    assert!(store.has_local_certification_witness(key(0xAA)));
    assert!(store.remove(key(0xAA)).unwrap());
    assert!(!store.has_local_certification_witness(key(0xAA)));
}

#[test]
fn round_retention_survives_epoch_reset_replay_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut record = notarization_record(0xAC, 0);
    record.finalized_epoch = 2;
    record.finalized_view = 2;
    let key = record.proof_key();
    {
        let store = FinalizedParentCertStore::open(dir.path()).unwrap();
        store.mark_local_certification_witness(key);
        store.put_certified_notarization(record.clone()).unwrap();
        store.mark_local_certification_witness(key);
        store.put_certified_notarization(record.clone()).unwrap();
        // A high chain height must never prune a fresh epoch's small view.
        store.prune_below_height(10_000).unwrap();
        store
            .prune_certified_notarizations_below_round(Round::new(
                Epoch::new(1),
                View::new(1_000_000),
            ))
            .unwrap();
        assert_eq!(store.get_certified_notarization(key), Some(record.clone()));
    }
    let store = FinalizedParentCertStore::open(dir.path()).unwrap();
    assert!(store.has_local_certification_witness(key));
    // The exact boundary is retained, then both proof and witness expire together.
    store
        .prune_certified_notarizations_below_round(Round::new(Epoch::new(2), View::new(2)))
        .unwrap();
    assert_eq!(store.get_certified_notarization(key), Some(record));
    store
        .prune_certified_notarizations_below_round(Round::new(Epoch::new(2), View::new(3)))
        .unwrap();
    assert!(!store.has_local_certification_witness(key));
    assert!(store.get_certified_notarization(key).is_none());
    drop(store);
    assert!(!FinalizedParentCertStore::open(dir.path())
        .unwrap()
        .has_local_certification_witness(key));
}

#[test]
fn a_large_old_epoch_view_does_not_outlive_the_round_floor() {
    let store = FinalizedParentCertStore::new();
    let mut old = notarization_record(0xAE, 0);
    old.finalized_epoch = 1;
    old.finalized_view = 1_000_000;
    let old_key = old.proof_key();
    store.put_certified_notarization(old).unwrap();
    let fresh = CertifiedParentProofKey::new(2, 1, B256::ZERO);
    store.mark_local_certification_witness(fresh);
    store
        .prune_certified_notarizations_below_round(Round::new(Epoch::new(2), View::new(0)))
        .unwrap();
    assert!(!store.has_local_certification_witness(old_key));
    assert!(store.get_certified_notarization(old_key).is_none());
    assert!(store.has_local_certification_witness(fresh));
}

#[test]
fn pending_witnesses_are_bounded_without_finalization_progress() {
    let store = FinalizedParentCertStore::new();
    let first = CertifiedParentProofKey::new(2, 1, B256::ZERO);
    for view in 1..=(MAX_PENDING_CERTIFICATION_WITNESSES as u64 + 1) {
        store.mark_local_certification_witness(CertifiedParentProofKey::new(2, view, B256::ZERO));
    }
    assert!(!store.has_local_certification_witness(first));
    let last = CertifiedParentProofKey::new(
        2,
        MAX_PENDING_CERTIFICATION_WITNESSES as u64 + 1,
        B256::ZERO,
    );
    assert!(store.has_local_certification_witness(last));
    store
        .prune_certified_notarizations_below_round(Round::new(Epoch::new(3), View::new(0)))
        .unwrap();
    assert!(!store.has_local_certification_witness(last));
}

#[test]
fn durable_cn_cap_preserves_selected_owned_proof() {
    let store = FinalizedParentCertStore::new();
    let original = notarization_record(0xAD, 0);
    let key = original.proof_key();
    store.put_certified_notarization(original.clone()).unwrap();
    let selected = store.get_best_for_parent(key, 1).unwrap();
    for view in 1..=MAX_CERTIFIED_NOTARIZATION_RECORDS as u64 {
        let mut record = original.clone();
        record.finalized_view = view + original.finalized_view;
        store.put_certified_notarization(record).unwrap();
    }
    assert_eq!(store.len(), MAX_CERTIFIED_NOTARIZATION_RECORDS);
    assert!(store.get_certified_notarization(key).is_none());
    assert!(!store.has_local_certification_witness(key));
    assert_eq!(
        selected,
        ParentProofSelection::CertifiedNotarization(original),
        "selection owns its proof across eviction/wait"
    );
}

#[test]
fn put_with_wrong_format_version_returns_unknown_format_version() {
    // The write-side guard rejects records with the wrong version even
    // before they reach disk.
    let store = FinalizedParentCertStore::new();
    let mut bad = finalization_record(0xCC, 1);
    // 2 is the retired pre-V3 version. The write-side guard must reject it.
    bad.format_version = 2;
    let err = store.put_finalization(bad).expect_err("must reject");
    assert!(matches!(
        err,
        ParentProofStoreError::UnknownFormatVersion { version: 2, .. }
    ));
}

#[test]
fn height_pruning_only_removes_finalization_records() {
    let store = FinalizedParentCertStore::new();
    store
        .put_finalization(finalization_record(0x01, 10))
        .unwrap();
    store
        .put_finalization(finalization_record(0x02, 50))
        .unwrap();
    store
        .put_certified_notarization(notarization_record(0x03, 20))
        .unwrap();
    store
        .put_certified_notarization(notarization_record(0x04, 100))
        .unwrap();
    let dropped = store.prune_below_height(50).unwrap();
    // CN has no authenticated height and is pruned by round separately.
    assert_eq!(dropped, 1);
    assert_eq!(store.len(), 3);
    assert!(store.get_finalization(key(0x01)).is_none());
    assert!(store.get_certified_notarization(key(0x03)).is_some());
    assert!(store.has_local_certification_witness(key(0x03)));
    assert!(store.has_local_certification_witness(key(0x04)));
}

#[test]
fn put_same_hash_same_slot_overwrites() {
    let store = FinalizedParentCertStore::new();
    store
        .put_finalization(finalization_record(0xAA, 100))
        .unwrap();
    let mut updated = finalization_record(0xAA, 100);
    updated.signer_bitmap = vec![0];
    store.put_finalization(updated.clone()).unwrap();
    assert_eq!(store.len(), 1);
    assert_eq!(store.get_finalization(key(0xAA)), Some(updated));
}

#[test]
fn store_clone_shares_state() {
    let store = FinalizedParentCertStore::new();
    let other = store.clone();
    store
        .put_finalization(finalization_record(0xAB, 7))
        .unwrap();
    assert!(other.get_finalization(key(0xAB)).is_some());
}

#[test]
fn durable_store_recovers_post_put_pre_phase1_record() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("records");
    let f = finalization_record(0xCA, 77);
    let n = notarization_record(0xCB, 78);
    {
        let store = FinalizedParentCertStore::open(&dir).unwrap();
        store.put_finalization(f.clone()).unwrap();
        store.put_certified_notarization(n.clone()).unwrap();
    }
    let reopened = FinalizedParentCertStore::open(&dir).unwrap();
    assert_eq!(reopened.get_finalization(key(0xCA)), Some(f));
    assert_eq!(reopened.get_certified_notarization(key(0xCB)), Some(n));
    assert!(reopened.get_finalization(key(0xCC)).is_none());
}

#[test]
fn durable_prune_removes_disk_entry() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("records");
    {
        let store = FinalizedParentCertStore::open(&dir).unwrap();
        store
            .put_finalization(finalization_record(0x01, 10))
            .unwrap();
        store
            .put_finalization(finalization_record(0x02, 50))
            .unwrap();
        assert_eq!(store.prune_below_height(50).unwrap(), 1);
    }
    let reopened = FinalizedParentCertStore::open(&dir).unwrap();
    assert!(reopened.get_finalization(key(0x01)).is_none());
    assert!(reopened.get_finalization(key(0x02)).is_some());
}

#[test]
fn remove_drops_both_slots_for_same_hash() {
    let store = FinalizedParentCertStore::new();
    store
        .put_finalization(finalization_record(0xAA, 10))
        .unwrap();
    store
        .put_certified_notarization(notarization_record(0xAA, 10))
        .unwrap();
    assert!(store.remove(key(0xAA)).unwrap());
    assert!(store.get_best_parent_proof(key(0xAA)).is_none());
}

#[test]
fn oldest_stored_height_only_tracks_finalizations() {
    let store = FinalizedParentCertStore::new();
    store
        .put_finalization(finalization_record(0x01, 100))
        .unwrap();
    store
        .put_certified_notarization(notarization_record(0x02, 20))
        .unwrap();
    assert_eq!(store.oldest_stored_height(), Some(100));
}
