//! Promotion of the committed DKG boundary to durable storage.
//!
//! The committed boundary for epoch E+1 must reach disk even when the next
//! ceremony (dealing E+2) runs or completes before the execution-height arm got
//! to take it, and even when live finalization delivery never marked it
//! committed. Otherwise a crash after E+2 completes but before E+2 activates
//! leaves no E+1 threshold material to restart from.

use super::*;

fn validator_set(keys: &[bls12381::PrivateKey]) -> validators::ValidatorSet {
    validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: (0..keys.len())
            .map(|index| Address::with_last_byte(0x11 + index as u8))
            .collect(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; keys.len()],
    }
}

fn boundary(
    epoch: u64,
    validator_set: &validators::ValidatorSet,
    output: &Output<MinSig, bls12381::PublicKey>,
) -> DkgBoundaryArtifact {
    dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(epoch),
        validator_set,
        output,
        is_full_dkg: false,
        dkg_cycle: epoch,
        freeze_height: 90,
        planned_activation_height: 120,
        vrf_material_version: epoch,
        is_validator_set_change: false,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn promotion_before_next_ceremony_completes_keeps_the_committed_boundary() {
    let (keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let current = boundary(1, &validator_set(&keys), &output);
    let (next_keys, _, next_output, _, _) = run_test_dkg_complete();
    let next = boundary(2, &validator_set(&next_keys), &next_output);
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;
    // Stale E+1 pending material plus the live E+2 ceremony's retry store.
    save_pending_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();
    std::fs::write(dir.path().join(DKG_DEALER_RETRY_FILE), b"live-e2-ceremony").unwrap();

    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(current.clone());
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(current)));

    // The E+2 ceremony is running (reshare in progress): promote before its
    // completion overwrites the manager's pending boundary.
    let promotion = promote_committed_boundary(
        &manager,
        Some(dir.path()),
        &backend,
        ActiveDkgMaterial {
            local_key: &keys[0].public_key(),
            output: Some(&output),
            share: Some(&share),
            polynomial: &polynomial,
        },
        RetireScope::PendingMaterialOnly,
    )
    .await
    .unwrap();
    manager.note_ceremony_completed(next);

    assert_eq!(promotion, BoundaryPromotion::Promoted);
    let (_, _, saved_output) = load_saved_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("the committed E+1 material must be durable");
    assert_eq!(saved_output, output);
    assert!(load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .is_none());
    assert!(
        dir.path().join(DKG_DEALER_RETRY_FILE).exists(),
        "the running ceremony's retry store must survive the promotion"
    );
}

/// Why the promotion must run before the next completion: once the manager
/// records the next ceremony's boundary, the earlier commit can never be taken.
#[tokio::test]
async fn next_ceremony_completion_drops_an_untaken_commit() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let current = boundary(1, &validator_set(&keys), &output);
    let (next_keys, _, next_output, _, _) = run_test_dkg_complete();
    let next = boundary(2, &validator_set(&next_keys), &next_output);
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(current.clone());
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(current)));

    manager.note_ceremony_completed(next);

    assert_eq!(manager.take_committed_boundary_artifact().await, None);
}

#[tokio::test]
async fn promoted_and_next_pending_material_are_both_restorable() {
    let (keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let current = boundary(1, &validator_set(&keys), &output);
    let (next_keys, participants, next_output, next_share, _) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(current.clone());
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(current)));

    promote_committed_boundary(
        &manager,
        Some(dir.path()),
        &backend,
        ActiveDkgMaterial {
            local_key: &keys[0].public_key(),
            output: Some(&output),
            share: Some(&share),
            polynomial: &polynomial,
        },
        RetireScope::PendingMaterialOnly,
    )
    .await
    .unwrap();
    // Crash cut: the E+2 ceremony completed and persisted, E+2 not yet active.
    persist_completed_dkg_before_activation(
        dir.path(),
        &backend,
        Epoch::new(1),
        2,
        &participants,
        &FrozenDkgTarget {
            dkg_cycle: 2,
            freeze_height: 90,
            planned_activation_height: 120,
            validator_set: validator_set(&next_keys),
            participants: participants.clone(),
            tee_expired_target_exclusions: Vec::new(),
            is_validator_set_change: false,
        },
        &dkg_actor::DkgComplete {
            output: next_output.clone(),
            share: next_share,
            participants: participants.clone(),
        },
        104,
    )
    .unwrap();

    let (_, _, saved) = load_saved_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("active E+1 material restorable after the crash");
    let (_, _, pending) = load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("completed E+2 material restorable after the crash");
    assert_eq!(saved, output);
    assert_eq!(pending, next_output);
}

#[tokio::test]
async fn boundary_excluding_the_local_key_exits_validator_mode() {
    let (keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let (other_keys, _, other_output, _, _) = run_test_dkg_complete();
    let foreign = boundary(1, &validator_set(&other_keys), &other_output);
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(foreign.clone());
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(foreign)));

    let promotion = promote_committed_boundary(
        &manager,
        None,
        &bls::KeyBackend::Plaintext,
        ActiveDkgMaterial {
            local_key: &keys[0].public_key(),
            output: Some(&output),
            share: Some(&share),
            polynomial: &polynomial,
        },
        RetireScope::All,
    )
    .await
    .unwrap();

    assert_eq!(promotion, BoundaryPromotion::LocalExcluded);
}

#[test]
fn finalized_carrier_is_adopted_when_live_delivery_never_committed_it() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let current = boundary(1, &validator_set(&keys), &output);
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(current.clone());
    let activation_anchor = 120;
    let mut provider = MockFinalizedHeaderProvider::default();
    provider.insert(
        activation_anchor + 1,
        Some(ConsensusHeaderArtifact::BoundaryOutcome(current.clone())),
    );

    assert!(
        !adopt_finalized_boundary_carrier(
            &manager,
            &provider,
            activation_anchor,
            activation_anchor
        )
        .unwrap(),
        "the carrier is not finalized yet"
    );
    assert!(adopt_finalized_boundary_carrier(
        &manager,
        &provider,
        activation_anchor,
        activation_anchor + 1
    )
    .unwrap());

    let taken = commonware_runtime::tokio::Runner::default()
        .start(|_| async move { manager.take_committed_boundary_artifact().await });
    assert_eq!(taken, Some(current));
}

#[test]
fn finalized_carrier_of_another_boundary_is_not_adopted() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let pending = boundary(2, &validator_set(&keys), &output);
    let older = boundary(1, &validator_set(&keys), &output);
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(pending);
    let mut provider = MockFinalizedHeaderProvider::default();
    provider.insert(121, Some(ConsensusHeaderArtifact::BoundaryOutcome(older)));

    assert!(!adopt_finalized_boundary_carrier(&manager, &provider, 120, 200).unwrap());
    let taken = commonware_runtime::tokio::Runner::default()
        .start(|_| async move { manager.take_committed_boundary_artifact().await });
    assert_eq!(taken, None);
}
