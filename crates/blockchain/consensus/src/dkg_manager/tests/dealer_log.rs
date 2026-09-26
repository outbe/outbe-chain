//! Dealer-log gossip, ceremony and canonical-reconstruction tests.

use super::*;

#[tokio::test]
async fn dealer_log_roundtrips_through_manager() {
    let (_keys, participants, _output, _polynomial, local_log) = run_test_dkg_complete();
    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();
    manager
        .note_local_dealer_log(Epoch::new(0), local_log.clone())
        .unwrap();

    let served = manager.get_dealer_log(Epoch::new(0)).await.unwrap();
    assert_eq!(served, local_log);
    let _dealer = manager
        .verify_dealer_log(Epoch::new(0), served.to_vec())
        .await
        .unwrap();

    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(served)));
    assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
}

#[tokio::test]
async fn pending_p2p_dealer_log_can_be_served_and_drained() {
    let keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let (_info, _output, _shares, _logs, signed_logs) =
        run_round(&keys, participants.clone(), None, None, 7);
    let first = signed_logs.values().next().unwrap().clone();

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();
    manager
        .note_pending_dealer_log(Epoch::new(0), first.clone())
        .unwrap();

    assert_eq!(
        manager.get_dealer_log(Epoch::new(0)).await,
        Some(first.clone())
    );
    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(first)));
    assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
}

#[tokio::test]
async fn reshare_ceremony_uses_previous_players_as_dealers() {
    let old_keys: Vec<bls12381::PrivateKey> =
        (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    let old_participants: Set<bls12381::PublicKey> = old_keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let (_info, previous_output, _shares, _logs, _signed_logs) =
        run_round(&old_keys, old_participants.clone(), None, None, 0);

    let new_key = bls12381::PrivateKey::from_seed(100);
    let new_pk = new_key.public_key();
    let mut target_keys = old_keys.clone();
    target_keys.push(new_key);
    let target_participants: Set<bls12381::PublicKey> = target_keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 1, Some(previous_output), target_participants)
        .unwrap();

    let dealers = manager.with_state(|state| {
        state
            .ceremony
            .as_ref()
            .expect("ceremony initialized")
            .canonical
            .dealers()
            .clone()
    });
    assert_eq!(dealers, old_participants);
    assert!(dealers.position(&new_pk).is_none());
}

#[tokio::test]
async fn reshare_ceremony_keeps_removed_old_player_as_dealer() {
    let old_keys: Vec<bls12381::PrivateKey> =
        (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    let old_participants: Set<bls12381::PublicKey> = old_keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let (_info, previous_output, _shares, _logs, _signed_logs) =
        run_round(&old_keys, old_participants.clone(), None, None, 0);

    let removed_pk = old_keys[0].public_key();
    let target_participants: Set<bls12381::PublicKey> = old_keys
        .iter()
        .filter(|key| key.public_key() != removed_pk)
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 1, Some(previous_output), target_participants)
        .unwrap();

    let dealers = manager.with_state(|state| {
        state
            .ceremony
            .as_ref()
            .expect("ceremony initialized")
            .canonical
            .dealers()
            .clone()
    });
    assert_eq!(dealers, old_participants);
    assert!(dealers.position(&removed_pk).is_some());
}

#[tokio::test]
async fn pending_p2p_dealer_log_rejects_wrong_ceremony() {
    let keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let (_info, _output, _shares, _logs, signed_logs) =
        run_round(&keys, participants.clone(), None, None, 7);
    let first = signed_logs.values().next().unwrap().clone();

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 8, None, participants)
        .unwrap();

    assert!(manager
        .note_pending_dealer_log(Epoch::new(0), first)
        .is_err());
    assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
}

#[tokio::test]
async fn pending_p2p_dealer_log_rejects_non_committee_dealer() {
    let keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let (_info, _output, _shares, _logs, signed_logs) =
        run_round(&keys, participants.clone(), None, None, 7);
    let (dealer, bytes) = signed_logs.iter().next().unwrap();

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();
    manager.with_state(|state| {
        let ceremony = state.ceremony.as_mut().unwrap();
        ceremony.canonical.remove_dealer_for_test(dealer);
    });

    assert!(manager
        .note_pending_dealer_log(Epoch::new(0), bytes.clone())
        .is_err());
    assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
}

#[tokio::test]
async fn pending_p2p_dealer_log_rejects_conflicting_duplicate() {
    let keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let (_info, _output, _shares, _logs, signed_logs_a) =
        run_round(&keys, participants.clone(), None, None, 7);
    let (_info, _output, _shares, _logs, signed_logs_b) =
        run_round(&keys, participants.clone(), None, None, 7);
    let dealer = signed_logs_a.keys().next().unwrap();
    let first = signed_logs_a.get(dealer).unwrap().clone();
    let conflicting = signed_logs_b.get(dealer).unwrap().clone();
    assert_ne!(first, conflicting);

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();
    manager
        .note_pending_dealer_log(Epoch::new(0), first.clone())
        .unwrap();
    manager
        .note_pending_dealer_log(Epoch::new(0), conflicting)
        .unwrap();

    assert_eq!(manager.get_dealer_log(Epoch::new(0)).await, Some(first));
}

#[test]
fn chain_finalized_replay_rejects_non_committee_dealer() {
    let keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let (_info, _output, _shares, _logs, signed_logs) =
        run_round(&keys, participants.clone(), None, None, 7);
    let (dealer, bytes) = signed_logs.iter().next().unwrap();

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();
    manager.with_state(|state| {
        let ceremony = state.ceremony.as_mut().unwrap();
        ceremony.canonical.remove_dealer_for_test(dealer);
    });

    manager
        .note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(bytes.clone())));
    let recorded =
        manager.with_state(|state| state.ceremony.as_ref().unwrap().canonical.finalized_len());
    assert_eq!(recorded, 0);
}

/// The canonical state machine is a deterministic, replayable fold over the
/// chain-finalized dealer logs: feeding the *same* finalized-log order into two
/// fresh managers yields the same canonical output (crash-replay safety),
/// reconstruction is frozen once it first succeeds, and a duplicate finalized
/// log is idempotent. (Cross-order is intentionally NOT asserted: DKG completes
/// on threshold participation, so a different freeze-time subset is a different
/// group key - determinism comes from canonical chain order.)
#[test]
fn canonical_reconstruction_is_replay_deterministic_and_frozen() {
    let mut keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    keys.sort_by_key(|a| a.public_key().encode());
    let participants: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let (_info, _output, _shares, _logs, signed_logs) =
        run_round(&keys, participants.clone(), None, None, 11);
    // Deterministic order (BTreeMap iteration = sorted by dealer pubkey).
    let order: Vec<Bytes> = signed_logs.values().cloned().collect();
    assert!(order.len() >= 3, "need >= threshold logs to reconstruct");

    let feed = |seq: &[Bytes]| -> Option<_> {
        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 11, None, participants.clone())
            .unwrap();
        for bytes in seq {
            manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
                bytes.clone(),
            )));
        }
        manager.canonical_output(Epoch::new(0))
    };

    // Same-order replay -> identical canonical output (deterministic rebuild).
    let out_a = feed(&order).expect("reconstructed from full set");
    let out_b = feed(&order).expect("reconstructed on replay");
    assert_eq!(out_a, out_b);

    // Freeze-once: output is fixed at first successful reconstruction and a
    // later finalized log never changes it.
    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 11, None, participants.clone())
        .unwrap();
    for bytes in order.iter().take(3) {
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
            bytes.clone(),
        )));
    }
    let frozen = manager
        .canonical_output(Epoch::new(0))
        .expect("threshold reached");
    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
        order[3].clone(),
    )));
    assert_eq!(
        manager.canonical_output(Epoch::new(0)),
        Some(frozen),
        "reconstruction must be frozen once produced"
    );

    // Duplicate finalized log is idempotent: the same dealer is not recorded
    // twice.
    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 11, None, participants)
        .unwrap();
    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
        order[0].clone(),
    )));
    manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
        order[0].clone(),
    )));
    let recorded =
        manager.with_state(|state| state.ceremony.as_ref().unwrap().canonical.finalized_len());
    assert_eq!(
        recorded, 1,
        "duplicate finalized dealer log must not double-count"
    );
}

#[test]
fn dealer_log_size_within_extra_data_for_n128() {
    let mut keys: Vec<bls12381::PrivateKey> = (0..128)
        .map(|i| bls12381::PrivateKey::from_seed(i + 1))
        .collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &config::outbe_app_namespace(),
        7,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .unwrap();

    let dealer_key = keys[0].clone();
    let dealer_pk = dealer_key.public_key();
    let (mut dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
        rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
        info.clone(),
        dealer_key,
        None,
    )
    .unwrap();
    for (player_pk, priv_msg) in priv_msgs {
        let mut player = Player::new(
            info.clone(),
            keys.iter()
                .find(|key| key.public_key() == player_pk)
                .unwrap()
                .clone(),
        )
        .unwrap();
        let ack = player
            .dealer_message::<N3f1>(dealer_pk.clone(), pub_msg.clone(), priv_msg)
            .expect("fixture dealing must be valid")
            .unwrap();
        dealer.receive_player_ack(player_pk, ack).unwrap();
    }
    let dealer_log = Bytes::from(dealer.finalize::<N3f1>().encode());

    let encoded = outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
        &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
            execution_summary: Some(
                outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                    validator_fee_sum: U256::MAX,
                },
            ),
            consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(dealer_log)),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            compressed_entities_root: None,
        },
    )
    .unwrap();

    assert!(
        encoded.len() <= outbe_primitives::consensus::OUTBE_MAX_EXTRA_DATA_SIZE,
        "encoded artifact size {} must fit OUTBE_MAX_EXTRA_DATA_SIZE {}",
        encoded.len(),
        outbe_primitives::consensus::OUTBE_MAX_EXTRA_DATA_SIZE
    );
}
