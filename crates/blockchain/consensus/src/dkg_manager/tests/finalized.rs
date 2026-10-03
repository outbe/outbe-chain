//! Regression coverage for finalized effects, fenced replay and ancestry lookups.

use super::*;
use crate::test_fixtures::{
    block_with_number_and_parent, dkg_runtime_artifacts, validator_set_from_keys,
    TestAncestryReader,
};

fn boundary_fixture() -> DkgBoundaryArtifact {
    let (keys, _, output, _, _) = dkg_runtime_artifacts();
    build_boundary_artifact(BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set_from_keys(&keys),
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 1,
        freeze_height: 119,
        planned_activation_height: 120,
        vrf_material_version: 1,
        is_validator_set_change: false,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap()
}

fn dealer_logs_fixture() -> (Set<bls12381::PublicKey>, Vec<Bytes>) {
    let mut keys: Vec<_> = (20..24).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let (_, _, _, _, signed_logs) = run_round(&keys, participants.clone(), None, None, 11);
    (participants, signed_logs.into_values().collect())
}

#[tokio::test]
async fn finalized_notifications_ignore_invalid_and_duplicate_logs_and_survive_closed_actor() {
    let (participants, logs) = dealer_logs_fixture();
    let manager = Mailbox::new();
    let epoch = Epoch::new(3);
    let (tx, mut rx) = mpsc::unbounded_channel();
    manager
        .note_ceremony_started_with_finalized_log_tx(epoch, 11, None, participants, Some(tx))
        .unwrap();
    manager
        .note_pending_dealer_log(epoch, logs[0].clone())
        .unwrap();
    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
        Bytes::from_static(b"invalid"),
    )));
    assert!(rx.try_recv().is_err());
    assert_eq!(manager.get_dealer_log(epoch).await, Some(logs[0].clone()));
    assert_eq!(manager.canonical_output(epoch), None);

    for bytes in logs.iter().take(3) {
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
            bytes.clone(),
        )));
        assert_eq!(rx.try_recv().unwrap(), *bytes);
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
            bytes.clone(),
        )));
        assert!(
            rx.try_recv().is_err(),
            "duplicates must not notify the actor"
        );
    }
    assert!(manager.get_dealer_log(epoch).await.is_none());
    let frozen = manager
        .canonical_output(epoch)
        .expect("threshold reconstructs");
    drop(rx);
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(logs[3].clone())));
    assert_eq!(manager.canonical_output(epoch), Some(frozen));
    assert_eq!(
        manager.with_state(|state| state.ceremony.as_ref().unwrap().canonical.finalized_len()),
        4,
        "actor closure must not prevent canonical insertion"
    );

    let (participants, logs) = dealer_logs_fixture();
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let manager = Mailbox::new();
    manager
        .note_ceremony_started_with_finalized_log_tx(epoch, 11, None, participants, Some(tx))
        .unwrap();
    for bytes in logs {
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(bytes)));
    }
    assert!(
        manager.canonical_output(epoch).is_some(),
        "send failure must not skip reconstruction"
    );
}

#[tokio::test]
async fn only_matching_boundary_outcome_promotes_pending_and_nonzero_carriers_are_cached() {
    let pending = boundary_fixture();
    let manager = Mailbox::new();
    manager.note_bootstrap_outcome(pending.clone());
    let carrier_hash = B256::repeat_byte(0x71);
    manager.note_finalized_header_artifact_at(120, carrier_hash, None);
    manager.note_finalized_header_artifact_at(
        120,
        carrier_hash,
        Some(&ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: pending.epoch,
            outcome: pending.outcome.clone(),
        }),
    );
    assert!(manager.take_committed_boundary_artifact().await.is_none());
    let pending_hash = Mailbox::boundary_artifact_hash(&pending).unwrap();
    assert!(manager
        .cached_boundary_status(carrier_hash, pending_hash)
        .is_none());

    let mut other = pending.clone();
    other.vrf_material_version += 1;
    manager.note_finalized_header_artifact_at(
        120,
        carrier_hash,
        Some(&ConsensusHeaderArtifact::BoundaryOutcome(other.clone())),
    );
    assert!(manager.take_committed_boundary_artifact().await.is_none());
    let other_hash = Mailbox::boundary_artifact_hash(&other).unwrap();
    assert!(matches!(
        manager.cached_boundary_status(carrier_hash, other_hash),
        Some(BoundaryStatus::BoundaryCommitted(committed))
            if committed.artifact == other && committed.block_number == 120
                && committed.block_hash == carrier_hash
    ));

    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(
        pending.clone(),
    )));
    assert!(manager
        .cached_boundary_status(B256::ZERO, pending_hash)
        .is_none());
    assert_eq!(
        manager.take_committed_boundary_artifact().await,
        Some(pending)
    );
}

#[test]
fn fenced_replay_resets_ceremony_and_preserves_notification_order() {
    let (participants, logs) = dealer_logs_fixture();
    let epoch = Epoch::new(3);
    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(2), 11, None, participants.clone())
        .unwrap();
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(logs[3].clone())));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut replay_logs = logs.clone();
    replay_logs.insert(1, logs[0].clone());
    replay_logs.insert(0, Bytes::from_static(b"invalid"));
    manager
        .lock_finalized_replay()
        .restart_ceremony_with_finalized_logs(CeremonyReplayRequest {
            epoch,
            round: 11,
            previous_output: None,
            participants: participants.clone(),
            finalized_dealer_log_tx: Some(tx),
            finalized_logs: replay_logs
                .into_iter()
                .enumerate()
                .map(|(index, bytes)| (120 + index as u64, B256::ZERO, bytes)),
        })
        .unwrap();
    assert!(manager.canonical_output(Epoch::new(2)).is_none());
    let frozen = manager
        .canonical_output(epoch)
        .expect("replayed prefix reconstructs");
    for bytes in &logs {
        assert_eq!(rx.try_recv().unwrap(), *bytes);
    }
    assert!(rx.try_recv().is_err());
    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(logs[0].clone())));
    assert!(rx.try_recv().is_err());
    assert_eq!(manager.canonical_output(epoch), Some(frozen.clone()));

    let empty = std::iter::empty::<bls12381::PublicKey>()
        .try_collect()
        .unwrap();
    let must_not_iterate = std::iter::from_fn(|| -> Option<(u64, B256, Bytes)> {
        panic!("failed ceremony setup must not consume replay logs")
    });
    assert!(manager
        .lock_finalized_replay()
        .restart_ceremony_with_finalized_logs(CeremonyReplayRequest {
            epoch: Epoch::new(4),
            round: 12,
            previous_output: None,
            participants: empty,
            finalized_dealer_log_tx: None,
            finalized_logs: must_not_iterate,
        })
        .is_err());
    assert_eq!(manager.canonical_output(epoch), Some(frozen));
}

#[tokio::test]
async fn ancestor_lookup_accepts_height_hit_and_checks_hash_fallback_height() {
    let pending = boundary_fixture();
    let ancestor = block_with_number_and_parent(119, B256::ZERO);
    let parent = block_with_number_and_parent(120, ancestor.block_hash());
    for (ancestry, expected_lookups) in [
        (TestAncestryReader::ready().with_block(ancestor.clone()), 1),
        (TestAncestryReader::ready().with_hash_block(ancestor), 2),
    ] {
        let manager = Mailbox::new();
        assert_eq!(
            manager
                .resolve_boundary(Some(&parent), Some(&pending), &ancestry)
                .await
                .unwrap(),
            BoundaryRequirement::MustEmit
        );
        assert_eq!(ancestry.lookup_count(), expected_lookups);
        assert_eq!(
            manager
                .resolve_boundary(
                    Some(&parent),
                    Some(&pending),
                    &TestAncestryReader::not_ready()
                )
                .await
                .unwrap(),
            BoundaryRequirement::MustEmit
        );
    }
    let wrong_height = block_with_number_and_parent(118, B256::ZERO);
    let parent = block_with_number_and_parent(120, wrong_height.block_hash());
    let ancestry = TestAncestryReader::ready().with_hash_block(wrong_height.clone());
    let error = Mailbox::new()
        .resolve_boundary(Some(&parent), Some(&pending), &ancestry)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        BoundaryRequirementError::Unavailable(format!(
            "DKG boundary ancestry unavailable: parent {} resolved at height 118, expected 119",
            wrong_height.block_hash(),
        ))
    );
    assert_eq!(ancestry.lookup_count(), 2);
}
