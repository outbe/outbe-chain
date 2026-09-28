//! Restart replay-suffix authentication and archive repair.

use super::*;

#[test]
fn restart_authenticates_and_pairs_archive_suffix_across_epoch_boundary() {
    let c0 = committee(10);
    let c1 = committee(30);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (
            10,
            certified_block(&c0, e0, 10, c1.preannounce_block_extra_data(e1)),
        ),
        (
            11,
            certified_block(&c1, e1, 11, c1.boundary_block_extra_data(e1)),
        ),
        (12, certified_block(&c1, e1, 12, Vec::new())),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..10);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    for height in [10_u64, 11, 12] {
        let record = records.get(&height).unwrap();
        certificates
            .by_height
            .lock()
            .unwrap()
            .insert(height, record.finalization.clone());
        blocks
            .by_height
            .lock()
            .unwrap()
            .insert(height, record.block.clone());
    }

    futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(10),
        Height::new(12),
        certificates.clone(),
        blocks.clone(),
    ))
    .expect("paired suffix must authenticate through the epoch boundary");

    assert_eq!(chain.lock().highest_registered(), Some(e1));
    assert_eq!(epocher.containing(Height::new(12)).unwrap().epoch(), e1);
    assert_eq!(certificates.last_index(), Some(Height::new(12)));
    assert_eq!(blocks.last_index(), Some(Height::new(12)));
}

#[test]
fn restart_recovers_preannounce_before_suffix_lower_bound() {
    let c0 = committee(10);
    let c1 = committee(30);
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
        (9, certified_block(&c0, e0, 9, Vec::new())),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..8);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    for height in [8_u64, 9] {
        let record = records.get(&height).unwrap();
        certificates
            .by_height
            .lock()
            .unwrap()
            .insert(height, record.finalization.clone());
    }
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(9, records[&9].block.clone());

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
    .expect("restart must recover an earlier authenticated successor preannounce");

    assert_eq!(chain.lock().highest_registered(), Some(e1));

    let boundary = certified_block(&c1, e1, 11, c1.boundary_block_extra_data(e1));
    engine::authenticate_live_finalized(&chain, &epocher, Height::new(11), &boundary)
        .expect("the subsequent live boundary must verify with the recovered successor");
    assert_eq!(epocher.containing(Height::new(11)).unwrap().epoch(), e1);
}

#[test]
fn restart_repairs_both_archive_halves_from_authenticated_suffix() {
    let c0 = committee(10);
    let c1 = committee(30);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (
            10,
            certified_block(&c0, e0, 10, c1.preannounce_block_extra_data(e1)),
        ),
        (
            11,
            certified_block(&c1, e1, 11, c1.boundary_block_extra_data(e1)),
        ),
        (12, certified_block(&c1, e1, 12, Vec::new())),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..10);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    certificates
        .by_height
        .lock()
        .unwrap()
        .insert(10, records[&10].finalization.clone());
    certificates
        .by_height
        .lock()
        .unwrap()
        .insert(12, records[&12].finalization.clone());
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(10, records[&10].block.clone());
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(11, records[&11].block.clone());

    futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(10),
        Height::new(12),
        certificates.clone(),
        blocks.clone(),
    ))
    .expect("authenticated suffix must repair either missing archive companion");

    for height in 10_u64..=12 {
        assert_eq!(
            certificates.by_height.lock().unwrap()[&height].encode(),
            records[&height].finalization.encode()
        );
        assert_eq!(
            blocks.by_height.lock().unwrap()[&height].encode(),
            records[&height].block.encode()
        );
    }
    assert_eq!(chain.lock().highest_registered(), Some(e1));
}

#[test]
fn restart_authenticates_multiple_epochs_in_one_replay_suffix() {
    let c0 = committee(10);
    let c1 = committee(30);
    let c2 = committee(50);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let e2 = Epoch::new(2);
    let epocher = FollowerEpocher::new(10, 0);
    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (
            10,
            certified_block(&c0, e0, 10, c1.preannounce_block_extra_data(e1)),
        ),
        (
            11,
            certified_block(&c1, e1, 11, c1.boundary_block_extra_data(e1)),
        ),
        (
            20,
            certified_block(&c1, e1, 20, c2.preannounce_block_extra_data(e2)),
        ),
        (
            21,
            certified_block(&c2, e2, 21, c2.boundary_block_extra_data(e2)),
        ),
        (22, certified_block(&c2, e2, 22, Vec::new())),
    ]);
    for height in 12_u64..20 {
        records.insert(height, certified_block(&c1, e1, height, Vec::new()));
    }
    fill_plain_finalized_range(&mut records, &c0, e0, 2..10);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    for height in 10_u64..=22 {
        let record = &records[&height];
        certificates
            .by_height
            .lock()
            .unwrap()
            .insert(height, record.finalization.clone());
        blocks
            .by_height
            .lock()
            .unwrap()
            .insert(height, record.block.clone());
    }

    futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(10),
        Height::new(22),
        certificates.clone(),
        blocks.clone(),
    ))
    .expect("one replay suffix may authenticate several epoch transitions");

    assert_eq!(chain.lock().highest_registered(), Some(e2));
    assert_eq!(epocher.containing(Height::new(22)).unwrap().epoch(), e2);
}

#[test]
fn restart_replay_suffix_rejects_missing_upstream_height() {
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
    certificates
        .by_height
        .lock()
        .unwrap()
        .insert(9, records[&9].finalization.clone());
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(9, records[&9].block.clone());

    let error = futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(9),
        Height::new(10),
        certificates.clone(),
        blocks.clone(),
    ))
    .unwrap_err()
    .to_string();

    assert!(error.contains("upstream did not return follower replay suffix height 10"));
}

#[test]
fn restart_replay_suffix_rejects_local_block_conflict() {
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
    certificates
        .by_height
        .lock()
        .unwrap()
        .insert(9, records[&9].finalization.clone());
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(9, certified_block(&c0, e0, 9, vec![0xFF]).block);

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

    assert!(error
        .contains("local follower replay block differs from authenticated upstream at height 9"));
}

#[test]
fn restart_replay_suffix_accepts_distinct_valid_quorum_for_same_proposal() {
    let c0 = committee(10);
    let e0 = Epoch::new(0);
    let epocher = FollowerEpocher::new(10, 0);
    let mut height_nine = certified_block(&c0, e0, 9, Vec::new());
    height_nine.finalization =
        c0.finalization_for_signers(e0, height_nine.block.digest(), &[0, 1, 2]);
    let local_finalization =
        c0.finalization_for_signers(e0, height_nine.block.digest(), &[1, 2, 3]);
    assert_eq!(
        local_finalization.proposal,
        height_nine.finalization.proposal
    );
    assert_ne!(
        local_finalization.encode(),
        height_nine.finalization.encode()
    );

    let mut records = BTreeMap::from([
        (
            1,
            certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
        ),
        (9, height_nine),
    ]);
    fill_plain_finalized_range(&mut records, &c0, e0, 2..9);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(records.clone()),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    certificates
        .by_height
        .lock()
        .unwrap()
        .insert(9, local_finalization.clone());
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(9, records[&9].block.clone());

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
    .expect("distinct valid quorum certificates for one proposal must reconcile");

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
    .expect("restarting with the retained alternate certificate must be idempotent");

    assert_eq!(
        certificates.by_height.lock().unwrap()[&9].encode(),
        local_finalization.encode(),
        "reconciliation must retain the already-valid local certificate"
    );
}

#[test]
fn restart_replay_suffix_rejects_invalid_local_certificate_for_same_proposal() {
    let c0 = committee(10);
    let attacker = committee(50);
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
    let forged = attacker.finalization_for(e0, records[&9].block.digest());
    assert_eq!(forged.proposal, records[&9].finalization.proposal);
    certificates.by_height.lock().unwrap().insert(9, forged);
    blocks
        .by_height
        .lock()
        .unwrap()
        .insert(9, records[&9].block.clone());

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

    assert!(
        error.contains(
            "local follower replay finalization certificate failed verification at height 9"
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn restart_replay_suffix_rejects_local_finalization_conflict() {
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
    let digest = records[&9].block.digest();
    let signer_indices: Vec<_> = (0..c0.keys.len()).collect();
    let conflicts = [
        (
            "payload",
            certified_block(&c0, e0, 8, Vec::new()).finalization,
        ),
        (
            "parent view",
            c0.finalization_for_proposal(
                e0,
                Proposal::new(Round::new(e0, View::new(2)), View::new(0), digest),
                &signer_indices,
            ),
        ),
        (
            "round view",
            c0.finalization_for_proposal(
                e0,
                Proposal::new(Round::new(e0, View::new(3)), View::new(1), digest),
                &signer_indices,
            ),
        ),
        (
            "epoch",
            c0.finalization_for_proposal(
                e0,
                Proposal::new(
                    Round::new(Epoch::new(1), View::new(2)),
                    View::new(1),
                    digest,
                ),
                &signer_indices,
            ),
        ),
    ];

    for (name, conflict) in conflicts {
        let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
        let epocher = FollowerEpocher::new(10, 0);
        let certificates = MemoryCertificates::default();
        let blocks = MemoryBlocks::default();
        certificates.by_height.lock().unwrap().insert(9, conflict);
        blocks
            .by_height
            .lock()
            .unwrap()
            .insert(9, records[&9].block.clone());

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

        assert!(
            error.contains(
                "local follower replay finalization proposal differs from authenticated upstream at height 9"
            ),
            "{name}: unexpected error: {error}"
        );
    }
}
