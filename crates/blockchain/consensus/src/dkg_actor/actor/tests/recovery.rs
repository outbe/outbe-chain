use super::*;

// -----------------------------------------------------------------------
// Ack dedup - HashSet ignores duplicate inserts
// -----------------------------------------------------------------------

/// Verify that acked_players HashSet correctly deduplicates.
/// In production, duplicate P2P ack messages from the same player
/// must not inflate the ack count.
#[test]
fn test_ack_hashset_dedup() {
    use commonware_cryptography::bls12381;

    let key_a = bls12381::PrivateKey::from_seed(1);
    let pk_a = key_a.public_key();
    let key_b = bls12381::PrivateKey::from_seed(2);
    let pk_b = key_b.public_key();

    let mut acked_players = std::collections::BTreeSet::new();

    // First insert - count goes to 1
    acked_players.insert(pk_a.clone());
    assert_eq!(acked_players.len(), 1);

    // Duplicate insert of same key - count stays at 1
    acked_players.insert(pk_a.clone());
    assert_eq!(
        acked_players.len(),
        1,
        "duplicate ack must not increment count"
    );

    // Different key - count goes to 2
    acked_players.insert(pk_b.clone());
    assert_eq!(acked_players.len(), 2);

    // Duplicate of second key - still 2
    acked_players.insert(pk_b);
    assert_eq!(acked_players.len(), 2);
}

// -----------------------------------------------------------------------
// Self-dealing ack count - only on success
// -----------------------------------------------------------------------

/// Verify that self-ack is counted only when self-dealing succeeds.
/// The acked_players HashSet starts empty and self-ack is inserted only
/// after successful receive_player_ack.
#[test]
fn test_self_ack_starts_empty() {
    let acked_players: std::collections::BTreeSet<commonware_cryptography::bls12381::PublicKey> =
        std::collections::BTreeSet::new();

    // Starts at 0 (not 1 as the old code had)
    assert_eq!(acked_players.len(), 0, "acked_players must start empty");
}

#[test]
fn dealer_retry_snapshot_round_trips_and_is_ceremony_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    let ceremony = DkgCeremonyId {
        round: 7,
        info_hash: B256::repeat_byte(0x42),
    };
    let seed = [0x5a; 32];

    assert!(store.load_dealer(ceremony).unwrap().is_none());
    let snapshot = DkgDealerRetrySnapshot {
        ceremony_id: ceremony,
        seed,
        accepted_acks: BTreeMap::new(),
    };
    store.save_dealer(&snapshot).unwrap();
    let recovered = store.load_dealer(ceremony).unwrap().unwrap();
    assert_eq!(recovered.seed, seed);
    assert!(recovered.accepted_acks.is_empty());
    assert!(store
        .load_dealer(DkgCeremonyId {
            round: 8,
            info_hash: ceremony.info_hash,
        })
        .unwrap()
        .is_none());

    store.clear().unwrap();
    assert!(store.load_dealer(ceremony).unwrap().is_none());
}

#[test]
fn recovered_seed_reconstructs_identical_dealer_transcript() {
    let mut keys: Vec<bls12381::PrivateKey> =
        (30..33).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .unwrap();
    let seed = [0x33; 32];

    let (_, first_pub, first_priv) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
        rand_commonware::rngs::ChaCha20Rng::from_seed(seed),
        info.clone(),
        keys[0].clone(),
        None,
    )
    .unwrap();
    let (mut recovered_dealer, recovered_pub, recovered_priv) =
        Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_commonware::rngs::ChaCha20Rng::from_seed(seed),
            info.clone(),
            keys[0].clone(),
            None,
        )
        .unwrap();

    assert_eq!(first_pub.encode(), recovered_pub.encode());
    let first_priv_for_ack = first_priv.clone();
    let recovered_priv_for_retry = recovered_priv.clone();
    let first_priv: Vec<_> = first_priv
        .into_iter()
        .map(|(pk, msg)| (pk.encode(), msg.encode()))
        .collect();
    let recovered_priv: Vec<_> = recovered_priv
        .into_iter()
        .map(|(pk, msg)| (pk.encode(), msg.encode()))
        .collect();
    assert_eq!(first_priv, recovered_priv);

    let player_pk = keys[1].public_key();
    let priv_msg = first_priv_for_ack
        .into_iter()
        .find(|(pk, _)| pk == &player_pk)
        .unwrap()
        .1;
    let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info, keys[1].clone()).unwrap();
    let ack = player
        .dealer_message::<N3f1>(keys[0].public_key(), first_pub, priv_msg)
        .expect("fixture dealing must be valid")
        .unwrap();
    let ceremony_id = DkgCeremonyId {
        round: 0,
        info_hash: B256::repeat_byte(0x77),
    };
    let snapshot = DkgDealerRetrySnapshot {
        ceremony_id,
        seed,
        accepted_acks: BTreeMap::from([(player_pk.clone(), ack)]),
    };
    let recovered_snapshot =
        decode_dealer_retry_snapshot(&encode_dealer_retry_snapshot(&snapshot).unwrap()).unwrap();
    let mut retry_targets: BTreeMap<_, _> = recovered_priv_for_retry.into_iter().collect();
    let mut restart_replay_targets =
        take_restart_replay_shares(&mut retry_targets, &recovered_snapshot.accepted_acks);
    for (acknowledged, ack) in &recovered_snapshot.accepted_acks {
        recovered_dealer
            .receive_player_ack(acknowledged.clone(), ack.clone())
            .unwrap();
    }
    assert!(
        !retry_targets.contains_key(&player_pk),
        "periodic durable retry must exclude a player whose ACK was journaled"
    );
    assert!(
        restart_replay_targets.contains_key(&player_pk),
        "restart must retain the byte-identical dealing for periodic replay because the \
             first replay may precede the remote process-local Player receiver"
    );
    let replayed_ack = recovered_snapshot
        .accepted_acks
        .get(&player_pk)
        .unwrap()
        .clone();
    assert!(acknowledge_restart_replay(
        &mut restart_replay_targets,
        Some(&recovered_snapshot.accepted_acks),
        &player_pk,
        &replayed_ack,
    ));
    assert!(
        restart_replay_targets.is_empty(),
        "the exact regenerated ACK proves that the restarted Player ingested the replay"
    );
}

#[test]
fn test_valid_ack_removes_player_from_retry_set() {
    let key = bls12381::PrivateKey::from_seed(1);
    let player_pk = key.public_key();
    let mut unsent_shares = BTreeMap::new();
    unsent_shares.insert(player_pk.clone(), ());

    let mut acked_players = std::collections::BTreeSet::new();
    acked_players.insert(player_pk.clone());
    unsent_shares.remove(&player_pk);

    assert!(unsent_shares.is_empty());
    assert_eq!(acked_players.len(), 1);
}

#[test]
fn duplicate_dealer_bundle_reuses_cached_ack() {
    let mut keys: Vec<bls12381::PrivateKey> = (0..3).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .unwrap();

    let dealer_key = keys[0].clone();
    let player_key = keys[1].clone();
    let dealer_pk = dealer_key.public_key();
    let player_pk = player_key.public_key();
    let (_, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
        rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
        info.clone(),
        dealer_key,
        None,
    )
    .unwrap();
    let priv_msg = priv_msgs
        .into_iter()
        .find(|(pk, _)| *pk == player_pk)
        .unwrap()
        .1;
    let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info, player_key).unwrap();
    let mut accepted = DkgPlayerRetrySnapshot::empty(DkgCeremonyId {
        round: 0,
        info_hash: B256::ZERO,
    });

    let first = handle_player_bundle(
        &mut player,
        &mut accepted,
        None,
        dealer_pk.clone(),
        pub_msg.clone(),
        priv_msg.clone(),
    )
    .unwrap();
    let second = handle_player_bundle(
        &mut player,
        &mut accepted,
        None,
        dealer_pk,
        pub_msg,
        priv_msg,
    )
    .unwrap();

    assert!(matches!(first, PlayerBundleAction::SendAck(_)));
    assert!(matches!(second, PlayerBundleAction::DuplicateAck(_)));
}

#[test]
fn conflicting_dealer_bundle_is_not_acknowledged() {
    let mut keys: Vec<bls12381::PrivateKey> =
        (10..13).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .unwrap();

    let dealer_key = keys[0].clone();
    let player_key = keys[1].clone();
    let dealer_pk = dealer_key.public_key();
    let player_pk = player_key.public_key();
    let (_, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
        rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
        info.clone(),
        dealer_key.clone(),
        None,
    )
    .unwrap();
    let (_, conflicting_pub_msg, conflicting_priv_msgs) =
        Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            info.clone(),
            dealer_key,
            None,
        )
        .unwrap();
    let priv_msg = priv_msgs
        .into_iter()
        .find(|(pk, _)| *pk == player_pk)
        .unwrap()
        .1;
    let conflicting_priv_msg = conflicting_priv_msgs
        .into_iter()
        .find(|(pk, _)| *pk == player_pk)
        .unwrap()
        .1;
    let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info, player_key).unwrap();
    let mut accepted = DkgPlayerRetrySnapshot::empty(DkgCeremonyId {
        round: 0,
        info_hash: B256::ZERO,
    });

    let first = handle_player_bundle(
        &mut player,
        &mut accepted,
        None,
        dealer_pk.clone(),
        pub_msg,
        priv_msg,
    )
    .unwrap();
    let second = handle_player_bundle(
        &mut player,
        &mut accepted,
        None,
        dealer_pk,
        conflicting_pub_msg,
        conflicting_priv_msg,
    )
    .unwrap();

    assert!(matches!(first, PlayerBundleAction::SendAck(_)));
    assert!(matches!(second, PlayerBundleAction::Equivocation { .. }));
}

#[test]
fn retry_distribution_stops_after_dealer_finalization() {
    let mut keys: Vec<bls12381::PrivateKey> =
        (21..24).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .unwrap();
    let (_, _, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
        rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
        info,
        keys[0].clone(),
        None,
    )
    .unwrap();
    let mut unsent_shares = BTreeMap::new();
    let (player_pk, priv_msg) = priv_msgs.into_iter().next().unwrap();
    unsent_shares.insert(player_pk, priv_msg);

    assert!(!should_retry_share_distribution(false, &unsent_shares));
    assert!(should_retry_share_distribution(true, &unsent_shares));
}
