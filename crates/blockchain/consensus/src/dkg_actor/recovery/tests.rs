use super::*;
use commonware_cryptography::Signer as _;
use commonware_utils::ordered::Set;

fn fixture() -> (
    Info<MinSig, bls12381::PublicKey>,
    bls12381::PrivateKey,
    bls12381::PublicKey,
    DealerPubMsg<MinSig>,
    DealerPrivMsg,
    DkgCeremonyId,
) {
    let mut keys: Vec<_> = (1..=3).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participant_keys = keys
        .iter()
        .map(commonware_cryptography::Signer::public_key)
        .collect::<Vec<_>>();
    let participants: Set<_> = participant_keys.try_into().unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        commonware_cryptography::bls12381::primitives::sharing::Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();
    let dealer_key = keys[0].clone();
    let player_key = keys[1].clone();
    let player_pk = player_key.public_key();
    let dealer_pk = dealer_key.public_key();
    let (_, pub_msg, priv_msgs) =
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Dealer::<
            MinSig,
            bls12381::PrivateKey,
        >::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            info.clone(),
            dealer_key,
            None,
        )
        .unwrap();
    let priv_msg = priv_msgs
        .into_iter()
        .find(|(key, _)| key == &player_pk)
        .unwrap()
        .1;
    let ceremony_id = DkgCeremonyId::new(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        &participants,
    );
    (info, player_key, dealer_pk, pub_msg, priv_msg, ceremony_id)
}

#[test]
fn player_dealing_survives_restart_and_replays_the_same_ack() {
    let (info, player_key, dealer, pub_msg, priv_msg, ceremony_id) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    let max_players = NonZeroU32::new(3).unwrap();
    let (mut player, mut snapshot) = restore_player(
        info.clone(),
        player_key.clone(),
        ceremony_id,
        max_players,
        Some(&store),
    )
    .unwrap();
    let first = handle_player_bundle(
        &mut player,
        &mut snapshot,
        Some(&store),
        crate::dkg_actor::recovery::PlayerDealerBundle {
            dealer: dealer.clone(),
            pub_msg: pub_msg.clone(),
            priv_msg: priv_msg.clone(),
        },
    )
    .unwrap();
    let PlayerBundleAction::SendAck(first_ack) = first else {
        panic!("first valid dealing must be acknowledged");
    };

    let (mut restarted, mut recovered) =
        restore_player(info, player_key, ceremony_id, max_players, Some(&store)).unwrap();
    let duplicate = handle_player_bundle(
        &mut restarted,
        &mut recovered,
        Some(&store),
        crate::dkg_actor::recovery::PlayerDealerBundle {
            dealer,
            pub_msg,
            priv_msg,
        },
    )
    .unwrap();
    let PlayerBundleAction::DuplicateAck(replayed_ack) = duplicate else {
        panic!("replayed dealing must reuse its durable ACK");
    };
    assert_eq!(first_ack.encode(), replayed_ack.encode());
}

#[test]
fn clear_removes_both_role_snapshots() {
    let (_, _, _, _, _, ceremony_id) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    store
        .save_dealer(&DkgDealerRetrySnapshot {
            ceremony_id,
            seed: zeroize::Zeroizing::new([7; 32]),
            accepted_acks: BTreeMap::new(),
        })
        .unwrap();
    store
        .save_player(&DkgPlayerRetrySnapshot::empty(ceremony_id))
        .unwrap();

    store.clear().unwrap();

    assert!(!dir.path().join(DKG_DEALER_RETRY_FILE).exists());
    assert!(!dir.path().join(DKG_PLAYER_RETRY_FILE).exists());
}

#[test]
fn persistence_failure_prevents_acknowledgement() {
    let (info, player_key, dealer, pub_msg, priv_msg, ceremony_id) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let invalid_keys_dir = dir.path().join("not-a-directory");
    std::fs::write(&invalid_keys_dir, b"file").unwrap();
    let store = DkgRetryStore::in_keys_dir(&invalid_keys_dir, crate::bls::KeyBackend::Plaintext);
    let (mut player, mut snapshot) = restore_player(
        info,
        player_key,
        ceremony_id,
        NonZeroU32::new(3).unwrap(),
        Some(&store),
    )
    .unwrap();

    let error = handle_player_bundle(
        &mut player,
        &mut snapshot,
        Some(&store),
        crate::dkg_actor::recovery::PlayerDealerBundle {
            dealer,
            pub_msg,
            priv_msg,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("player retry"));
}

#[test]
fn player_snapshot_is_ceremony_scoped_and_truncation_fails_closed() {
    let (_, _, _, _, _, ceremony_id) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    store
        .save_player(&DkgPlayerRetrySnapshot::empty(ceremony_id))
        .unwrap();

    assert!(store
        .load_player(
            DkgCeremonyId {
                round: ceremony_id.round + 1,
                info_hash: ceremony_id.info_hash,
            },
            NonZeroU32::new(3).unwrap(),
        )
        .unwrap()
        .is_none());

    crate::bls::save_raw(
        &store.player_path,
        b"ODKGPR01",
        &crate::bls::KeyBackend::Plaintext,
    )
    .unwrap();
    assert!(store
        .load_player(ceremony_id, NonZeroU32::new(3).unwrap())
        .is_err());
}

#[test]
fn duplicate_dealer_in_player_snapshot_fails_closed() {
    let (info, player_key, dealer, pub_msg, priv_msg, ceremony_id) = fixture();
    let (mut player, mut snapshot) = restore_player(
        info,
        player_key,
        ceremony_id,
        NonZeroU32::new(3).unwrap(),
        None,
    )
    .unwrap();
    assert!(matches!(
        handle_player_bundle(
            &mut player,
            &mut snapshot,
            None,
            crate::dkg_actor::recovery::PlayerDealerBundle {
                dealer,
                pub_msg,
                priv_msg
            }
        )
        .unwrap(),
        PlayerBundleAction::SendAck(_)
    ));
    let encoded = encode_player_retry_snapshot(&snapshot).unwrap();
    let first_entry = &encoded[DKG_PLAYER_RETRY_HEADER_LEN..];
    let mut duplicated = encoded[..DKG_PLAYER_RETRY_HEADER_LEN].to_vec();
    duplicated[48..52].copy_from_slice(&2u32.to_be_bytes());
    duplicated.extend_from_slice(first_entry);
    duplicated.extend_from_slice(first_entry);

    let error = decode_player_retry_snapshot(&duplicated, NonZeroU32::new(3).unwrap()).unwrap_err();
    assert!(error.to_string().contains("duplicate dealer"));
}

#[test]
fn restarted_player_finalizes_with_a_private_share() {
    use commonware_cryptography::bls12381::dkg::feldman_desmedt::{Dealer, Logs};
    use commonware_parallel::Sequential;

    let mut keys: Vec<_> = (101..=103).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<_> = keys
        .iter()
        .map(commonware_cryptography::Signer::public_key)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        commonware_cryptography::bls12381::primitives::sharing::Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();
    let ceremony_id = DkgCeremonyId::new(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        &participants,
    );
    let target_key = keys[1].clone();
    let target_pk = target_key.public_key();
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    let max_players = NonZeroU32::new(3).unwrap();
    let (mut target_player, mut snapshot) = restore_player(
        info.clone(),
        target_key.clone(),
        ceremony_id,
        max_players,
        Some(&store),
    )
    .unwrap();
    let mut dealer_logs = BTreeMap::new();

    for dealer_key in &keys {
        let dealer_pk = dealer_key.public_key();
        let (mut dealer, pub_msg, priv_msgs) =
            Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                info.clone(),
                dealer_key.clone(),
                None,
            )
            .unwrap();
        for (player_pk, priv_msg) in priv_msgs {
            let ack = if player_pk == target_pk {
                match handle_player_bundle(
                    &mut target_player,
                    &mut snapshot,
                    Some(&store),
                    crate::dkg_actor::recovery::PlayerDealerBundle {
                        dealer: dealer_pk.clone(),
                        pub_msg: pub_msg.clone(),
                        priv_msg,
                    },
                )
                .unwrap()
                {
                    PlayerBundleAction::SendAck(ack) => ack,
                    other => panic!("unexpected target Player action: {other:?}"),
                }
            } else {
                let player_key = keys
                    .iter()
                    .find(|key| key.public_key() == player_pk)
                    .unwrap();
                let mut player =
                    Player::<MinSig, bls12381::PrivateKey>::new(info.clone(), player_key.clone())
                        .unwrap();
                player
                    .dealer_message::<N3f1>(dealer_pk.clone(), pub_msg.clone(), priv_msg)
                    .expect("fixture dealing must be valid")
                    .unwrap()
            };
            dealer.receive_player_ack(player_pk, ack).unwrap();
        }
        let signed_log = dealer.finalize::<N3f1>();
        let (checked_dealer, log) = signed_log.check(&info).expect("valid finalized dealer log");
        assert_eq!(checked_dealer, dealer_pk);
        dealer_logs.insert(checked_dealer, log);
    }

    drop(target_player);
    let (restarted_player, _) = restore_player(
        info.clone(),
        target_key,
        ceremony_id,
        max_players,
        Some(&store),
    )
    .unwrap();
    let mut logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info);
    for (dealer, log) in dealer_logs {
        logs.record(dealer, log);
    }
    let (output, _private_share) = restarted_player
        .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
            &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            logs,
            &Sequential,
        )
        .expect("restarted target Player must finalize with a private share");
    assert_eq!(output.players(), &participants);
}

#[test]
fn oversized_stored_player_snapshot_is_rejected_before_read() {
    let (_, _, _, _, _, ceremony_id) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    let file = std::fs::File::create(&store.player_path).unwrap();
    file.set_len(MAX_DKG_PLAYER_RETRY_STORED_BYTES + 1).unwrap();

    let error = store
        .load_player(ceremony_id, NonZeroU32::new(3).unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("stored DKG player retry"));
}

#[test]
fn replay_inconsistent_ack_fails_closed() {
    let (info, player_key, dealer, pub_msg, priv_msg, ceremony_id) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::Plaintext);
    let max_players = NonZeroU32::new(3).unwrap();
    let (mut player, mut snapshot) = restore_player(
        info.clone(),
        player_key.clone(),
        ceremony_id,
        max_players,
        None,
    )
    .unwrap();
    handle_player_bundle(
        &mut player,
        &mut snapshot,
        None,
        crate::dkg_actor::recovery::PlayerDealerBundle {
            dealer: dealer.clone(),
            pub_msg,
            priv_msg,
        },
    )
    .unwrap();

    let (_, _, _, other_pub_msg, other_priv_msg, _) = fixture();
    let mut other_player =
        Player::<MinSig, bls12381::PrivateKey>::new(info.clone(), player_key.clone()).unwrap();
    let different_ack = other_player
        .dealer_message::<N3f1>(dealer.clone(), other_pub_msg, other_priv_msg)
        .expect("fixture dealing must be valid")
        .unwrap();
    snapshot.accepted_dealings.get_mut(&dealer).unwrap().ack = different_ack;
    store.save_player(&snapshot).unwrap();

    let error = match restore_player(info, player_key, ceremony_id, max_players, Some(&store)) {
        Ok(_) => panic!("replay-inconsistent ACK must fail closed"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("different ACK"));
}

#[test]
fn dealer_retry_snapshot_debug_redacts_seed() {
    let snapshot = DkgDealerRetrySnapshot {
        ceremony_id: DkgCeremonyId {
            round: 3,
            info_hash: B256::repeat_byte(0x11),
        },
        seed: zeroize::Zeroizing::new([0xA5; 32]),
        accepted_acks: BTreeMap::new(),
    };
    let rendered = format!("{snapshot:?}");
    assert!(
        rendered.contains("<redacted>"),
        "Debug dropped the redaction marker: {rendered}"
    );
    assert!(
        !rendered.contains("165") && !rendered.contains("a5") && !rendered.contains("A5"),
        "Debug leaked the seed: {rendered}"
    );
}

#[test]
fn dkg_retry_ceremony_leaves_zero_keychain_accounts_after_clear() {
    let guard = crate::bls::install_test_keychain();
    let mut keys: Vec<_> = (1..=3).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<_> = keys
        .iter()
        .map(commonware_cryptography::Signer::public_key)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        commonware_cryptography::bls12381::primitives::sharing::Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();
    let ceremony_id = DkgCeremonyId::new(
        &crate::config::outbe_app_namespace(),
        0,
        None,
        &participants,
    );
    let dir = tempfile::tempdir().unwrap();
    let store = DkgRetryStore::in_keys_dir(dir.path(), crate::bls::KeyBackend::OsLevel);
    let max_players = NonZeroU32::new(3).unwrap();
    let player_key = keys[1].clone();
    let player_pk = player_key.public_key();
    let (mut player, mut player_snapshot) = restore_player(
        info.clone(),
        player_key,
        ceremony_id,
        max_players,
        Some(&store),
    )
    .unwrap();
    let mut dealer_snapshot = DkgDealerRetrySnapshot {
        ceremony_id,
        seed: zeroize::Zeroizing::new([7; 32]),
        accepted_acks: BTreeMap::new(),
    };

    for dealer_key in [&keys[0], &keys[2]] {
        let dealer_pk = dealer_key.public_key();
        let (_, pub_msg, priv_msgs) =
            commonware_cryptography::bls12381::dkg::feldman_desmedt::Dealer::<
                MinSig,
                bls12381::PrivateKey,
            >::start::<N3f1>(
                rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                info.clone(),
                dealer_key.clone(),
                None,
            )
            .unwrap();
        let priv_msg = priv_msgs
            .into_iter()
            .find(|(key, _)| key == &player_pk)
            .unwrap()
            .1;
        let action = handle_player_bundle(
            &mut player,
            &mut player_snapshot,
            Some(&store),
            PlayerDealerBundle {
                dealer: dealer_pk.clone(),
                pub_msg,
                priv_msg,
            },
        )
        .unwrap();
        let PlayerBundleAction::SendAck(ack) = action else {
            panic!("each distinct dealing must be acknowledged and saved");
        };
        persist_dealer_ack(Some(&mut dealer_snapshot), Some(&store), dealer_pk, ack).unwrap();
    }

    assert!(
        guard.len() >= 2,
        "ceremony saves must reach the test keychain"
    );
    store.clear().unwrap();
    assert_eq!(guard.len(), 0);
    assert!(!dir.path().join(DKG_DEALER_RETRY_FILE).exists());
    assert!(!dir.path().join(DKG_PLAYER_RETRY_FILE).exists());
}
