//! Replay-suffix conflict detection and committee rebuild from finality.

use super::*;

#[test]
fn restart_replay_suffix_rejects_conflicting_preannounces() {
    let c0 = committee(10);
    let c1 = committee(30);
    let conflicting_c1 = committee(50);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (
            7,
            certified_block(&c0, e0, 7, conflicting_c1.preannounce_block_extra_data(e1)),
        ),
        (
            8,
            certified_block(&c0, e0, 8, c1.preannounce_block_extra_data(e1)),
        ),
        (9, certified_block(&c0, e0, 9, Vec::new())),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..7);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    for height in [7_u64, 8, 9] {
        certificates
            .by_height
            .lock()
            .unwrap()
            .insert(height, records[&height].finalization.clone());
        blocks
            .by_height
            .lock()
            .unwrap()
            .insert(height, records[&height].block.clone());
    }

    let error = futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(9),
        Height::new(9),
        certificates.clone(),
        blocks.clone(),
    ))
    .unwrap_err()
    .to_string();

    assert!(error.contains("conflicting committee outcome replay for epoch 1"));
}

#[test]
fn restart_replay_suffix_rejects_boundary_outcome_conflicting_with_preannounce() {
    let c0 = committee(10);
    let c1 = committee(30);
    let conflicting_c1 = committee(50);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (
            8,
            certified_block(&c0, e0, 8, c1.preannounce_block_extra_data(e1)),
        ),
        (
            11,
            certified_block(&c1, e1, 11, conflicting_c1.boundary_block_extra_data(e1)),
        ),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..8);
    fill_plain_finalized_range(&mut records, &c0, e0, 9..11);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();

    let error = futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(8),
        Height::new(11),
        certificates.clone(),
        blocks.clone(),
    ))
    .unwrap_err()
    .to_string();

    assert!(error.contains("boundary outcome conflicts with authenticated epoch 1 outcome"));
    assert!(epocher.activation_height(e1).is_none());
}

#[test]
fn restart_replay_suffix_rejects_wrong_upstream_payload_before_archive_write() {
    let c0 = committee(10);
    let e0 = Epoch::new(0);
    let epocher = FollowerEpocher::new(10, 0);
    let honest = certified_block(&c0, e0, 9, Vec::new());
    let other = certified_block(&c0, e0, 8, Vec::new());
    let malformed = CertifiedFinalizedBlock {
        finalization: other.finalization,
        block: honest.block,
    };
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (9, malformed),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..9);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();

    let error = futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(9),
        Height::new(9),
        certificates.clone(),
        blocks.clone(),
    ))
    .unwrap_err()
    .to_string();

    assert!(error.contains("finalization payload differs from block at height 9"));
    assert!(!certificates.by_height.lock().unwrap().contains_key(&9));
    assert!(!blocks.by_height.lock().unwrap().contains_key(&9));
}

#[test]
fn restart_replay_suffix_rejects_wrong_height_epoch_and_forged_certificate() {
    let c0 = committee(10);
    let c1 = committee(30);
    let e0 = Epoch::new(0);
    let wrong_height = certified_block(&c0, e0, 8, Vec::new());
    let wrong_epoch = certified_block(&c0, Epoch::new(2), 9, Vec::new());
    let honest = certified_block(&c0, e0, 9, Vec::new());
    let forged = CertifiedFinalizedBlock {
        finalization: c1.finalization_for(e0, honest.block.digest()),
        block: honest.block,
    };

    for (name, candidate, expected_error) in [
        (
            "wrong height",
            wrong_height,
            "certified block reports height 8, expected 9",
        ),
        (
            "wrong epoch",
            wrong_epoch,
            "recovered height 9 precedes activation window",
        ),
        (
            "forged certificate",
            forged,
            "finalization certificate failed verification for epoch 0",
        ),
    ] {
        let mut records = BTreeMap::from([
            (
                1,
                certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
            ),
            (9, candidate),
        ]);
        fill_plain_finalized_range(&mut records, &c0, e0, 2..9);
        let source = ArchivedFinalizedSource {
            by_height: Arc::new(records),
        };
        let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
        let epocher = FollowerEpocher::new(10, 0);
        let certificates = MemoryCertificates::default();
        let blocks = MemoryBlocks::default();

        let error = futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
            &chain,
            &source,
            &epocher,
            e0,
            Height::new(9),
            Height::new(9),
            certificates.clone(),
            blocks.clone(),
        ))
        .unwrap_err()
        .to_string();

        assert!(error.contains(expected_error), "{name}: {error}");
        assert!(certificates.by_height.lock().unwrap().is_empty(), "{name}");
        assert!(blocks.by_height.lock().unwrap().is_empty(), "{name}");
    }
}

#[test]
fn restart_repairs_both_durable_archive_crash_cuts() {
    let c0 = committee(10);
    let e0 = Epoch::new(0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (9, certified_block(&c0, e0, 9, Vec::new())),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..9);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };

    // Crash cut 1: the finalization sync committed, then block sync failed.
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let epocher = FollowerEpocher::new(10, 0);
    let certificates = MemoryCertificates::default();
    let blocks = DurableCrashBlocks {
        fail_next_sync: true,
        ..Default::default()
    };
    let error = futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(9),
        Height::new(9),
        certificates.clone(),
        blocks.clone(),
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("failed to sync repaired follower replay blocks"));
    assert!(certificates.by_height.lock().unwrap().contains_key(&9));
    assert!(!blocks.durable.lock().unwrap().contains_key(&9));

    // Restart sees the durable finalization-only tail and repairs its block.
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let epocher = FollowerEpocher::new(10, 0);
    let blocks = DurableCrashBlocks::default();
    futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(9),
        Height::new(9),
        certificates.clone(),
        blocks.clone(),
    ))
    .expect("restart must repair a durable finalization-only crash cut");
    assert!(blocks.durable.lock().unwrap().contains_key(&9));

    // Crash cut 2: a block-only durable tail is repaired symmetrically.
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let epocher = FollowerEpocher::new(10, 0);
    let certificates = MemoryCertificates::default();
    let blocks = DurableCrashBlocks {
        durable: Arc::new(std::sync::Mutex::new(BTreeMap::from([(
            9,
            records[&9].block.clone(),
        )]))),
        ..Default::default()
    };
    futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(9),
        Height::new(9),
        certificates.clone(),
        blocks.clone(),
    ))
    .expect("restart must repair a durable block-only crash cut");
    assert_eq!(
        certificates.by_height.lock().unwrap()[&9].encode(),
        records[&9].finalization.encode()
    );
    assert_eq!(blocks.durable.lock().unwrap().len(), 1);
}

#[test]
fn restart_replay_suffix_reconciliation_is_idempotent() {
    let c0 = committee(10);
    let e0 = Epoch::new(0);
    let epocher = FollowerEpocher::new(10, 0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (9, certified_block(&c0, e0, 9, Vec::new())),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..9);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();

    for _ in 0..2 {
        futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
            &chain,
            &source,
            &epocher,
            e0,
            Height::new(9),
            Height::new(9),
            certificates.clone(),
            blocks.clone(),
        ))
        .expect("repeated authenticated repair must be idempotent");
    }

    assert_eq!(certificates.by_height.lock().unwrap().len(), 1);
    assert_eq!(blocks.by_height.lock().unwrap().len(), 1);
    assert_eq!(
        certificates.by_height.lock().unwrap()[&9].encode(),
        records[&9].finalization.encode()
    );
    assert_eq!(
        blocks.by_height.lock().unwrap()[&9].encode(),
        records[&9].block.encode()
    );
}
