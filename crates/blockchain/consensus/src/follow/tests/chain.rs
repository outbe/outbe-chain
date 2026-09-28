//! Committee-chain admission: pre-announce chaining, anchors, outcome checks.

use super::*;

#[test]
fn restart_rebuilds_every_committee_from_prior_epoch_finality_before_current_epoch() {
    let c0 = committee(10);
    let c1 = committee(30);
    let c2 = committee(50);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let e2 = Epoch::new(2);
    let epocher = FollowerEpocher::new(10, 0);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(BTreeMap::from([
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
        ])),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));

    futures::executor::block_on(engine::prepare_committee_chain(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(21),
    ))
    .expect("restart must rebuild the authenticated chain through the recovered epoch");

    let guard = chain.lock();
    assert_eq!(guard.highest_registered(), Some(e2));
    guard
        .verify_finalization(e2, &c2.finalization(e2))
        .expect("current epoch finality must verify after restart reconstruction");
}

#[test]
fn next_committee_preannounce_may_precede_prior_epoch_final_block() {
    let c0 = committee(10);
    let c1 = committee(30);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 0);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(BTreeMap::from([
            (
                1,
                certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
            ),
            (
                8,
                certified_block(&c0, e0, 8, c1.preannounce_block_extra_data(e1)),
            ),
            (10, certified_block(&c0, e0, 10, Vec::new())),
            (
                11,
                certified_block(&c1, e1, 11, c1.boundary_block_extra_data(e1)),
            ),
        ])),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));

    futures::executor::block_on(engine::prepare_committee_chain(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(11),
    ))
    .expect("trusted pre-announce before the epoch-final block must register epoch 1");

    chain
        .lock()
        .verify_finalization(e1, &c1.finalization(e1))
        .expect("epoch 1 finality verifies through the prior-epoch carrier");
}

#[test]
fn restart_reconstructs_delayed_boundaries_and_ignores_a_later_preannounce_trap() {
    let c0 = committee(10);
    let c1 = committee(30);
    let c2 = committee(50);
    let c3 = committee(70);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let e2 = Epoch::new(2);
    let e3 = Epoch::new(3);
    let epocher = FollowerEpocher::new(10, 3);
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(BTreeMap::from([
            (
                1,
                certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0)),
            ),
            (
                12,
                certified_block(&c0, e0, 12, c1.preannounce_block_extra_data(e1)),
            ),
            (
                13,
                certified_block(&c1, e1, 13, c1.boundary_block_extra_data(e1)),
            ),
            (
                23,
                certified_block(&c1, e1, 23, c2.preannounce_block_extra_data(e2)),
            ),
            (
                25,
                certified_block(&c2, e2, 25, c3.preannounce_block_extra_data(e3)),
            ),
            (
                26,
                certified_block(&c2, e2, 26, c2.boundary_block_extra_data(e2)),
            ),
            (28, certified_block(&c2, e2, 28, Vec::new())),
        ])),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));

    let recovered_epoch = futures::executor::block_on(engine::prepare_committee_chain(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(28),
    ))
    .expect("restart must derive both delayed boundaries from authenticated history");

    assert_eq!(recovered_epoch, e2);
    assert_eq!(epocher.first(e1), Some(Height::new(13)));
    assert_eq!(epocher.first(e2), Some(Height::new(26)));
    assert_eq!(epocher.last(e1), Some(Height::new(25)));
    assert_eq!(epocher.containing(Height::new(28)).unwrap().epoch(), e2);
    assert_eq!(chain.lock().highest_registered(), Some(e2));
}

#[test]
fn live_delivery_mutates_only_after_certificate_authentication() {
    let c0 = committee(10);
    let c1 = committee(30);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 3);
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
    chain
        .lock()
        .register_epoch_from_outcome(e0, &c0.outcome(e0))
        .unwrap();

    let forged_carrier = certified_block(&c1, e0, 10, c1.preannounce_block_extra_data(e1));
    assert!(engine::authenticate_live_finalized(
        &chain,
        &epocher,
        Height::new(10),
        &forged_carrier,
    )
    .is_err());
    assert_eq!(chain.lock().highest_registered(), Some(e0));
    assert_eq!(epocher.first(e1), None);

    let carrier = certified_block(&c0, e0, 12, c1.preannounce_block_extra_data(e1));
    engine::authenticate_live_finalized(&chain, &epocher, Height::new(12), &carrier)
        .expect("prior-committee-certified preannounce must register epoch one");
    assert_eq!(chain.lock().highest_registered(), Some(e1));

    let boundary = certified_block(&c1, e1, 13, c1.boundary_block_extra_data(e1));
    engine::authenticate_live_finalized(&chain, &epocher, Height::new(13), &boundary)
        .expect("registered next committee must authenticate its delayed boundary");
    assert_eq!(epocher.first(e1), Some(Height::new(13)));
}

#[test]
fn committee_chain_anchors_then_chains_across_epochs() {
    let (e5, e6) = (Epoch::new(5), Epoch::new(6));
    let c5 = committee(10);
    let c6 = committee(50);
    let mut chain = CommitteeChain::new(e5, c5.participants.clone());

    chain
        .register_epoch_from_outcome(e5, &c5.outcome(e5))
        .unwrap();
    chain.verify_finalization(e5, &c5.finalization(e5)).unwrap();

    // Chain forward to epoch 6 (a different committee) and verify it.
    chain
        .register_epoch_from_outcome(e6, &c6.outcome(e6))
        .unwrap();
    chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();
    assert_eq!(chain.highest_registered(), Some(e6));
    let verifier = chain
        .scheme_provider()
        .scoped(e6)
        .expect("epoch-6 verifier is registered");
    assert_eq!(
        verifier.expected_vrf_material_version(),
        e6.get(),
        "the authenticated epoch must restore the canonical VRF material version"
    );

    // A finalization can't be verified for an unregistered epoch.
    assert!(chain
        .verify_finalization(Epoch::new(7), &c6.finalization(e6))
        .is_err());
}

#[test]
fn admission_cursor_has_no_256_epoch_horizon_and_retains_one_verifier() {
    let committee = committee(10);
    let mut chain = CommitteeChain::new(Epoch::new(0), committee.participants.clone());

    for raw_epoch in 0..=300_u64 {
        let epoch = Epoch::new(raw_epoch);
        chain
            .register_epoch_from_outcome(epoch, &committee.outcome(epoch))
            .unwrap();
        chain.retain_only_highest();

        assert_eq!(chain.highest_registered(), Some(epoch));
        assert_eq!(chain.outcome_hashes.len(), 1);
        assert!(commonware_cryptography::certificate::Provider::scoped(
            chain.scheme_provider(),
            epoch,
        )
        .is_some());
        if raw_epoch > 0 {
            assert!(
                commonware_cryptography::certificate::Provider::scoped(
                    chain.scheme_provider(),
                    Epoch::new(raw_epoch - 1),
                )
                .is_none(),
                "the prior verifier must be pruned after epoch {raw_epoch}"
            );
        }
    }
}

#[test]
fn preannounce_registers_and_self_finalized_boundary_cannot_override() {
    // The D1 fix, end to end at the follower: epoch 6's committee is registered
    // from its E-1 PRE-ANNOUNCE (carried in a block finalized by the trusted
    // epoch-5 committee - the chained path). A later self-finalized epoch-6
    // boundary announcing a DIFFERENT (forged) committee must NOT override it.
    let (e5, e6) = (Epoch::new(5), Epoch::new(6));
    let c5 = committee(10);
    let c6 = committee(50); // the real epoch-6 committee, pre-announced by trusted e5
    let forged6 = committee(77); // what a malicious self-finalized e6 boundary would claim
    let mut chain = CommitteeChain::new(e5, c5.participants.clone());
    chain
        .register_epoch_from_outcome(e5, &c5.outcome(e5))
        .unwrap();

    // Pre-announce epoch 6 in an e5-finalized block -> registered (chained trust).
    let pre6 = c6.preannounce_block_extra_data(e6);
    assert_eq!(
        chain.advance_from_block_extra_data(&pre6).unwrap(),
        Some(e6)
    );
    chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();

    // A forged, self-finalized epoch-6 boundary is a NO-OP - it cannot overwrite
    // the chained committee (that overwrite would be the D1 bug).
    let forged_boundary = forged6.boundary_block_extra_data(e6);
    assert_eq!(
        chain
            .advance_from_block_extra_data(&forged_boundary)
            .unwrap(),
        None
    );
    // The forged committee's finalization is rejected; the real one still verifies.
    assert!(chain
        .verify_finalization(e6, &forged6.finalization(e6))
        .is_err());
    chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();
}

#[test]
fn committee_chain_rejects_anchor_mismatch() {
    let e5 = Epoch::new(5);
    let c5 = committee(10);
    let wrong = committee(99);
    let mut chain = CommitteeChain::new(e5, wrong.participants.clone());
    let err = chain
        .register_epoch_from_outcome(e5, &c5.outcome(e5))
        .unwrap_err()
        .to_string();
    assert!(err.contains("anchor mismatch"), "error: {err}");
}

#[test]
fn committee_chain_rejects_noncanonical_or_mislabelled_outcomes() {
    let epoch = Epoch::new(5);
    let committee = committee(10);

    let mut trailing = committee.outcome(epoch);
    trailing.push(0);
    let mut chain = CommitteeChain::new(epoch, committee.participants.clone());
    assert!(chain.register_epoch_from_outcome(epoch, &trailing).is_err());

    let mut wrong_version = committee.outcome(epoch);
    wrong_version[4] ^= 1;
    let mut chain = CommitteeChain::new(epoch, committee.participants.clone());
    assert!(chain
        .register_epoch_from_outcome(epoch, &wrong_version)
        .is_err());

    let wrong_epoch = committee.outcome(Epoch::new(epoch.get() + 1));
    let mut chain = CommitteeChain::new(epoch, committee.participants.clone());
    assert!(chain
        .register_epoch_from_outcome(epoch, &wrong_epoch)
        .unwrap_err()
        .to_string()
        .contains("epoch label"));
}

#[test]
fn committee_chain_advances_from_boundary_block_extra_data() {
    let e6 = Epoch::new(6);
    let c6 = committee(70);
    // Anchor on epoch 6 - the boundary block we process announces it.
    let mut chain = CommitteeChain::new(e6, c6.participants.clone());
    // Feeding the boundary block's extra_data registers epoch 6's committee.
    let extra = c6.boundary_block_extra_data(e6);
    assert_eq!(
        chain.advance_from_block_extra_data(&extra).unwrap(),
        Some(e6)
    );
    // That epoch's finalization now verifies.
    chain.verify_finalization(e6, &c6.finalization(e6)).unwrap();
    // A non-boundary block (empty extra_data) registers nothing.
    assert_eq!(chain.advance_from_block_extra_data(&[]).unwrap(), None);
}
