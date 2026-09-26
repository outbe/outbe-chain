//! Committee admission invariants: a certificate is checked before any
//! committee state changes, and a boundary cannot disagree with the committee
//! its carrier pre-announced.

use super::*;

#[test]
fn bootstrap_anchor_does_not_mutate_on_bad_certificate() {
    let c0 = committee(10);
    let impostor = committee(30);
    let e0 = Epoch::new(0);
    let epocher = FollowerEpocher::new(10, 0);
    // The genuine genesis boundary outcome, but finalized by a foreign quorum.
    let source = ArchivedFinalizedSource {
        by_height: Arc::new(BTreeMap::from([(
            1,
            certified_block(&impostor, e0, 1, c0.boundary_block_extra_data(e0)),
        )])),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));

    for attempt in 0..2 {
        let result = futures::executor::block_on(engine::prepare_committee_chain(
            &chain,
            &source,
            &epocher,
            e0,
            Height::new(0),
        ));
        assert!(result.is_err(), "attempt {attempt}: forged anchor accepted");
        assert_eq!(
            chain.lock().highest_registered(),
            None,
            "attempt {attempt}: a rejected anchor must leave the chain untouched"
        );
    }
}

#[test]
fn rebuild_rejects_boundary_outcome_conflicting_with_its_carrier() {
    let c0 = committee(10);
    let c1 = committee(30);
    let other = committee(90);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let epocher = FollowerEpocher::new(10, 0);
    // Epoch 1's committee is chained from c0's pre-announce, but the boundary
    // block c1 finalizes carries a different epoch-1 outcome.
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
                certified_block(&c1, e1, 11, other.boundary_block_extra_data(e1)),
            ),
        ])),
    };
    let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));

    let error = futures::executor::block_on(engine::prepare_committee_chain(
        &chain,
        &source,
        &epocher,
        e0,
        Height::new(11),
    ))
    .expect_err("restart must reject what live delivery rejects");

    assert!(
        error.to_string().contains("conflict"),
        "unexpected rejection: {error}"
    );
}

fn anchored(c0: &Committee) -> CommitteeChain {
    let e0 = Epoch::new(0);
    let mut chain = CommitteeChain::new(e0, c0.participants.clone());
    let anchor = certified_block(c0, e0, 1, c0.boundary_block_extra_data(e0));
    chain
        .admit_anchor(&anchor.finalization, &c0.boundary_block_extra_data(e0))
        .expect("genesis anchor");
    chain
}

fn admit(
    chain: &mut CommitteeChain,
    signer: &Committee,
    certified: Epoch,
    extra: Vec<u8>,
    policy: AdmissionPolicy,
) -> eyre::Result<Admission> {
    let block = certified_block(signer, certified, 5, extra.clone());
    chain.admit(&block.finalization, &extra, policy)
}

#[test]
fn routed_policy_enforces_epoch_rules_before_any_change() {
    let (c0, c1) = (committee(10), committee(30));
    let (e0, e1, e2) = (Epoch::new(0), Epoch::new(1), Epoch::new(2));
    let mut chain = anchored(&c0);
    let routed = AdmissionPolicy::Routed { routed_epoch: e0 };

    for (name, signer, certified, extra) in [
        ("certificate two epochs ahead", &c0, e2, Vec::new()),
        (
            "non-successor pre-announce",
            &c0,
            e0,
            c1.preannounce_block_extra_data(e2),
        ),
        (
            "epoch change without its boundary",
            &c1,
            e1,
            c1.preannounce_block_extra_data(e2),
        ),
    ] {
        assert!(
            admit(&mut chain, signer, certified, extra, routed).is_err(),
            "{name}"
        );
        assert_eq!(
            chain.highest_registered(),
            Some(e0),
            "{name} changed the chain"
        );
    }

    assert_eq!(
        admit(
            &mut chain,
            &c0,
            e0,
            c1.preannounce_block_extra_data(e1),
            routed
        )
        .unwrap(),
        Admission::SuccessorRegistered(e1)
    );
    assert_eq!(
        admit(
            &mut chain,
            &c1,
            e1,
            c1.boundary_block_extra_data(e1),
            routed
        )
        .unwrap(),
        Admission::BoundaryRegistered(e1)
    );
    assert_eq!(
        admit(&mut chain, &c0, e0, Vec::new(), routed).unwrap(),
        Admission::Unchanged
    );
}

#[test]
fn successor_policy_requires_the_successor_preannounce() {
    let (c0, c1) = (committee(10), committee(30));
    let (e0, e1) = (Epoch::new(0), Epoch::new(1));
    let mut chain = anchored(&c0);

    for (name, extra) in [
        ("no artifact", Vec::new()),
        ("boundary instead", c0.boundary_block_extra_data(e0)),
        (
            "non-successor",
            c1.preannounce_block_extra_data(Epoch::new(2)),
        ),
    ] {
        assert!(
            admit(&mut chain, &c0, e0, extra, AdmissionPolicy::Successor).is_err(),
            "{name}"
        );
        assert_eq!(chain.highest_registered(), Some(e0), "{name}");
    }
    assert_eq!(
        admit(
            &mut chain,
            &c0,
            e0,
            c1.preannounce_block_extra_data(e1),
            AdmissionPolicy::Successor
        )
        .unwrap(),
        Admission::SuccessorRegistered(e1)
    );
}

#[test]
fn carrier_policy_registers_only_the_target_preannounce() {
    let (c0, c1) = (committee(10), committee(30));
    let (e0, e1) = (Epoch::new(0), Epoch::new(1));
    let mut chain = anchored(&c0);
    let carrier = AdmissionPolicy::CarrierFor { epoch: e1 };

    for extra in [
        Vec::new(),
        c0.boundary_block_extra_data(e0),
        c1.preannounce_block_extra_data(Epoch::new(2)),
    ] {
        assert_eq!(
            admit(&mut chain, &c0, e0, extra, carrier).unwrap(),
            Admission::Unchanged
        );
    }
    assert_eq!(
        admit(
            &mut chain,
            &c0,
            e0,
            c1.preannounce_block_extra_data(e1),
            carrier
        )
        .unwrap(),
        Admission::SuccessorRegistered(e1)
    );
}

#[test]
fn anchor_is_established_once_and_only_from_the_genesis_committee() {
    let (c0, impostor) = (committee(10), committee(30));
    let e0 = Epoch::new(0);

    let mut foreign = CommitteeChain::new(e0, c0.participants.clone());
    let block = certified_block(&impostor, e0, 1, impostor.boundary_block_extra_data(e0));
    assert!(foreign
        .admit_anchor(&block.finalization, &impostor.boundary_block_extra_data(e0))
        .is_err());
    assert_eq!(foreign.highest_registered(), None);

    let mut chain = anchored(&c0);
    let again = certified_block(&c0, e0, 1, c0.boundary_block_extra_data(e0));
    assert!(chain
        .admit_anchor(&again.finalization, &c0.boundary_block_extra_data(e0))
        .is_err());
    assert_eq!(chain.highest_registered(), Some(e0));
}
