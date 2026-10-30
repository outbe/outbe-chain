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
/// chain-finalized dealer logs. This test checks that:
///
/// - two fresh managers fed the *same* finalized-log order yield the same
///   canonical output (crash-replay safety).
/// - reconstruction is frozen once it first succeeds.
/// - a duplicate finalized log is idempotent.
///
/// The test intentionally does NOT assert cross-order. DKG completes on
/// threshold participation, so a different freeze-time subset is a different
/// group key. Determinism comes from canonical chain order.
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
            radicle_publication_enabled: false,
            radicle_publication: None,
            execution_summary: Some(
                outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                    validator_fee_sum: U256::MAX,
                },
            ),
            consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(dealer_log)),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            enclave_witness: None,
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

struct ReshareParityFixture {
    info: Info<MinSig, bls12381::PublicKey>,
    dealers: Set<bls12381::PublicKey>,
    participants: Set<bls12381::PublicKey>,
    valid_signed_bytes: Vec<(bls12381::PublicKey, Bytes)>,
    garbage_signed_bytes: Vec<(bls12381::PublicKey, Bytes)>,
}

fn build_reshare_parity_fixture() -> ReshareParityFixture {
    use rand_commonware::{rngs::ChaCha20Rng, SeedableRng as _};

    let mut keys: Vec<bls12381::PrivateKey> =
        (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();

    // Round 0: deterministic genesis DKG to obtain `previous_output` and `previous_shares`.
    let info0 = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();

    let mut dealers0 = Vec::new();
    let mut pub_msgs0 = Vec::new();
    let mut priv_msgs0 = Vec::new();
    for (idx, key) in keys.iter().enumerate() {
        let (dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            ChaCha20Rng::from_seed([0x10 + idx as u8; 32]),
            info0.clone(),
            key.clone(),
            None,
        )
        .unwrap();
        dealers0.push(dealer);
        pub_msgs0.push(pub_msg);
        priv_msgs0.push(priv_msgs);
    }
    let mut players0: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|key| Player::new(info0.clone(), key.clone()).unwrap())
        .collect();
    crate::test_harness::acknowledge_fixture_dealings(
        &keys,
        crate::test_harness::FixtureDealings {
            public_messages: &pub_msgs0,
            private_messages: &priv_msgs0,
        },
        &mut dealers0,
        &mut players0,
    );
    let mut logs0 = BTreeMap::new();
    for dealer in dealers0 {
        let signed = dealer.finalize::<N3f1>();
        let (pk, log) = signed.check(&info0).unwrap();
        logs0.insert(pk, log);
    }
    let mut initial_shares = Vec::new();
    let mut initial_output = None;
    for (idx, player) in players0.into_iter().enumerate() {
        let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info0.clone());
        for (dealer, log) in logs0.clone() {
            dkg_logs.record(dealer, log);
        }
        let (out, share) = player
            .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                &mut ChaCha20Rng::from_seed([0x20 + idx as u8; 32]),
                dkg_logs,
                &Sequential,
            )
            .unwrap();
        initial_output = Some(out);
        initial_shares.push(share);
    }
    let previous_output = initial_output.unwrap();

    // Round 1: reshare ceremony over the same 4 validators (`2f + 1 = 3`).
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &config::outbe_app_namespace(),
        1,
        Some(previous_output),
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();

    let mut valid_dealers = Vec::new();
    let mut valid_pub_msgs = Vec::new();
    let mut valid_priv_msgs = Vec::new();
    for (idx, key) in keys.iter().enumerate() {
        let (dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            ChaCha20Rng::from_seed([0x30 + idx as u8; 32]),
            info.clone(),
            key.clone(),
            Some(initial_shares[idx].clone()),
        )
        .unwrap();
        valid_dealers.push(dealer);
        valid_pub_msgs.push(pub_msg);
        valid_priv_msgs.push(priv_msgs);
    }
    let mut players1: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|key| Player::new(info.clone(), key.clone()).unwrap())
        .collect();
    crate::test_harness::acknowledge_fixture_dealings(
        &keys,
        crate::test_harness::FixtureDealings {
            public_messages: &valid_pub_msgs,
            private_messages: &valid_priv_msgs,
        },
        &mut valid_dealers,
        &mut players1,
    );

    let mut valid_signed_bytes = Vec::new();
    for dealer in valid_dealers {
        let signed = dealer.finalize::<N3f1>();
        let encoded = Bytes::from(signed.encode());
        let (pk, _log) = signed.check(&info).unwrap();
        valid_signed_bytes.push((pk, encoded));
    }

    // Build signed-but-garbage dealer logs: dealer `idx` deals using dealer
    // `(idx + 1) % 4`'s previous share and signs the resulting log with its own
    // key. `SignedDealerLog::check(&info)` succeeds, but `observe`/`select`
    // rejects the commitment against `previous_output`.
    let mut garbage_signed_bytes = Vec::new();
    for (idx, key) in keys.iter().enumerate() {
        let wrong_share = initial_shares[(idx + 1) % keys.len()].clone();
        let (garbage_dealer, _pub_msg, _priv_msgs) =
            Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                ChaCha20Rng::from_seed([0x40 + idx as u8; 32]),
                info.clone(),
                key.clone(),
                Some(wrong_share),
            )
            .unwrap();
        let signed = garbage_dealer.finalize::<N3f1>();
        assert!(
            signed.clone().check(&info).is_some(),
            "signed-but-garbage log must pass signature-only check"
        );
        garbage_signed_bytes.push((key.public_key(), Bytes::from(signed.encode())));
    }

    ReshareParityFixture {
        info,
        dealers: participants.clone(),
        participants,
        valid_signed_bytes,
        garbage_signed_bytes,
    }
}

static RESHARE_PARITY_FIXTURE: std::sync::LazyLock<ReshareParityFixture> =
    std::sync::LazyLock::new(build_reshare_parity_fixture);

/// W-07: Consensus-scheme + serialization boundary parity test for
/// `commonware-cryptography` (currently pinned at `tag = "v2026.9.0"`,
/// rev `d476a2361ce6840d2b9d0aa6fb30a924429046d4` in workspace `Cargo.toml`).
///
/// Any `cargo update` that bumps `commonware-*` is a consensus-boundary change
/// and requires an explicit boundary review of `observe` selection, transcript
/// encoding, and `Output` serialization. Never blindly regenerate the golden
/// digests below inside a dependency bump commit.
#[test]
fn commonware_dkg_reconstruction_parity() {
    use alloy_primitives::b256;
    use rand_commonware::{rngs::ChaCha20Rng, SeedableRng as _};

    let fixture = &*RESHARE_PARITY_FIXTURE;
    let max_players = std::num::NonZeroU32::new(fixture.participants.len() as u32).unwrap();
    let ceremony = ceremony::DkgCeremony::new(
        Epoch::new(1),
        fixture.info.clone(),
        max_players,
        fixture.dealers.clone(),
    );

    // Frozen input digest of the 1 signed-but-garbage log (dealer 0) + 3 valid
    // logs (dealers 1..=3) under `commonware-cryptography` v2026.9.0.
    let mut input_transcript = Vec::new();
    let (_, ref garbage_0) = fixture.garbage_signed_bytes[0];
    input_transcript.extend_from_slice(garbage_0);
    for (_, valid_bytes) in fixture.valid_signed_bytes.iter().skip(1) {
        input_transcript.extend_from_slice(valid_bytes);
    }
    let input_digest = alloy_primitives::keccak256(&input_transcript);

    // Feed 1 signed-but-garbage log (dealer 0) + 2 valid logs (dealers 1..=2):
    // raw count reaches 3 (`2f + 1`), but only 2 logs are content-valid, so both
    // the actor gate and manager reconstruction must reject.
    let mut map = BTreeMap::new();
    let verified_garbage = ceremony.verify_dealer_log(garbage_0).unwrap();
    map.insert(verified_garbage.dealer, verified_garbage.log);
    for (_, valid_bytes) in fixture.valid_signed_bytes.iter().skip(1).take(2) {
        let verified = ceremony.verify_dealer_log(valid_bytes).unwrap();
        map.insert(verified.dealer, verified.log);
    }
    assert_eq!(map.len(), 3);
    assert!(!crate::dkg_actor::chain_finalized_reconstructable_with_rng(
        &fixture.info,
        &map,
        &mut ChaCha20Rng::from_seed([0x55; 32]),
    ));
    assert!(ceremony::try_reconstruct_with_rng(
        &fixture.info,
        &map,
        &mut ChaCha20Rng::from_seed([0x55; 32]),
    )
    .is_err());

    // Add the 3rd content-valid log (dealer 3): now `k = 3` valid + `m = 1`
    // garbage log are present. Both gates must succeed and filter out dealer 0's
    // garbage log to produce the exact canonical output.
    let verified_last = ceremony
        .verify_dealer_log(&fixture.valid_signed_bytes[3].1)
        .unwrap();
    map.insert(verified_last.dealer, verified_last.log);
    assert_eq!(map.len(), 4);
    assert!(crate::dkg_actor::chain_finalized_reconstructable_with_rng(
        &fixture.info,
        &map,
        &mut ChaCha20Rng::from_seed([0x55; 32]),
    ));
    let output = ceremony::try_reconstruct_with_rng(
        &fixture.info,
        &map,
        &mut ChaCha20Rng::from_seed([0x55; 32]),
    )
    .expect("3 valid + 1 garbage log must reconstruct");

    let output_hash = dkg_output_hash(&output);
    let poly_hash = public_polynomial_hash(output.public());

    assert_eq!(
        (input_digest, output_hash, poly_hash),
        (
            b256!("4e31a8a142cca363b610b681069d721f88d2a63b5f7a6b7cbd244e5a4b825c82"),
            b256!("becc574895bad0fd82be053ebb8f6a257db584a7450ba4526d2e9d31dcb752cf"),
            b256!("0f49395b3b8ea6c8d832c7db9ceaff48c6b9524976eadec05d92f176f2f6fa99"),
        ),
        "frozen v2026.9.0 digests changed"
    );
}

mod reconstruction_properties {
    use proptest::prelude::*;
    use rand_commonware::{rngs::ChaCha20Rng, SeedableRng as _};

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// W-09 (1): `observe` uses the caller's RNG only for batch-verification
        /// weights, so any two RNG seeds must agree on both the boolean gate and
        /// the reconstructed `Output` bytes.
        #[test]
        fn dkg_reconstruction_deterministic_across_rng_seeds(
            seed_a in any::<[u8; 32]>(),
            seed_b in any::<[u8; 32]>(),
            include_garbage in any::<bool>(),
            valid_take in 2usize..=4,
        ) {
            let fixture = &*RESHARE_PARITY_FIXTURE;
            let max_players = std::num::NonZeroU32::new(fixture.participants.len() as u32).unwrap();
            let ceremony = ceremony::DkgCeremony::new(
                Epoch::new(1),
                fixture.info.clone(),
                max_players,
                fixture.dealers.clone(),
            );

            let mut logs = BTreeMap::new();
            if include_garbage {
                let verified = ceremony
                    .verify_dealer_log(&fixture.garbage_signed_bytes[0].1)
                    .unwrap();
                logs.insert(verified.dealer, verified.log);
            }
            let start = usize::from(include_garbage);
            for (_, bytes) in fixture.valid_signed_bytes.iter().skip(start).take(valid_take) {
                let verified = ceremony.verify_dealer_log(bytes).unwrap();
                logs.insert(verified.dealer, verified.log);
            }

            let mut rng_a1 = ChaCha20Rng::from_seed(seed_a);
            let mut rng_b1 = ChaCha20Rng::from_seed(seed_b);
            let gate_a = crate::dkg_actor::chain_finalized_reconstructable_with_rng(
                &fixture.info,
                &logs,
                &mut rng_a1,
            );
            let gate_b = crate::dkg_actor::chain_finalized_reconstructable_with_rng(
                &fixture.info,
                &logs,
                &mut rng_b1,
            );
            prop_assert_eq!(gate_a, gate_b);

            let mut rng_a2 = ChaCha20Rng::from_seed(seed_a);
            let mut rng_b2 = ChaCha20Rng::from_seed(seed_b);
            let out_a = ceremony::try_reconstruct_with_rng(&fixture.info, &logs, &mut rng_a2);
            let out_b = ceremony::try_reconstruct_with_rng(&fixture.info, &logs, &mut rng_b2);
            prop_assert_eq!(out_a.is_ok(), gate_a);
            prop_assert_eq!(out_a.is_ok(), out_b.is_ok());
            if let (Ok(out_a), Ok(out_b)) = (out_a, out_b) {
                prop_assert_eq!(out_a.encode(), out_b.encode());
            }
        }

        /// W-09 (2): Permuting raw `SignedDealerLog` inputs before
        /// `verify_dealer_log` + `apply_finalized_dealer_log` yields the same
        /// canonical dealer map and reconstructed `Output` once all logs are
        /// recorded, and two `DkgCeremony` machines fed the same ordered
        /// sequence freeze the exact same output.
        #[test]
        fn dkg_reconstruction_order_independent(
            perm in Just(vec![0usize, 1, 2, 3]).prop_shuffle(),
            seed_a in any::<[u8; 32]>(),
            seed_b in any::<[u8; 32]>(),
        ) {
            let fixture = &*RESHARE_PARITY_FIXTURE;
            let max_players = std::num::NonZeroU32::new(fixture.participants.len() as u32).unwrap();

            // Canonical order reference (0, 1, 2, 3) recorded via verify_dealer_log +
            // apply_finalized_dealer_log before full-set reconstruction.
            let mut canonical_ceremony = ceremony::DkgCeremony::new(
                Epoch::new(1),
                fixture.info.clone(),
                max_players,
                fixture.dealers.clone(),
            );
            for (_, bytes) in &fixture.valid_signed_bytes {
                let verified = canonical_ceremony.verify_dealer_log(bytes).unwrap();
                let _ = canonical_ceremony.apply_finalized_dealer_log(verified);
            }
            let _ = canonical_ceremony
                .try_reconstruct_if_needed_with_rng(&mut ChaCha20Rng::from_seed(seed_a));
            let reference_output = canonical_ceremony
                .output()
                .expect("canonical full-set logs must reconstruct");

            // Permute the raw SignedDealerLog bytes before verify_dealer_log +
            // apply_finalized_dealer_log, then reconstruct the full set.
            let mut permuted_ceremony = ceremony::DkgCeremony::new(
                Epoch::new(1),
                fixture.info.clone(),
                max_players,
                fixture.dealers.clone(),
            );
            for &idx in &perm {
                let verified = permuted_ceremony
                    .verify_dealer_log(&fixture.valid_signed_bytes[idx].1)
                    .unwrap();
                let _ = permuted_ceremony.apply_finalized_dealer_log(verified);
            }
            let _ = permuted_ceremony
                .try_reconstruct_if_needed_with_rng(&mut ChaCha20Rng::from_seed(seed_b));
            let permuted_output = permuted_ceremony
                .output()
                .expect("permuted full-set logs must reconstruct");
            prop_assert_eq!(reference_output.encode(), permuted_output.encode());

            // Also verify that two DkgCeremony state machines stepping through
            // `apply_finalized_dealer_log` + `try_reconstruct_if_needed_with_rng`
            // on the same ordered sequence freeze the identical output.
            let mut ceremony_a = ceremony::DkgCeremony::new(
                Epoch::new(1),
                fixture.info.clone(),
                max_players,
                fixture.dealers.clone(),
            );
            let mut ceremony_b = ceremony::DkgCeremony::new(
                Epoch::new(1),
                fixture.info.clone(),
                max_players,
                fixture.dealers.clone(),
            );
            let mut rng_a = ChaCha20Rng::from_seed(seed_a);
            let mut rng_b = ChaCha20Rng::from_seed(seed_b);
            for &idx in &perm {
                let raw = &fixture.valid_signed_bytes[idx].1;
                let va = ceremony_a.verify_dealer_log(raw).unwrap();
                let vb = ceremony_b.verify_dealer_log(raw).unwrap();
                let _ = ceremony_a.apply_finalized_dealer_log(va);
                let _ = ceremony_b.apply_finalized_dealer_log(vb);
                let _ = ceremony_a.try_reconstruct_if_needed_with_rng(&mut rng_a);
                let _ = ceremony_b.try_reconstruct_if_needed_with_rng(&mut rng_b);
            }
            prop_assert_eq!(
                ceremony_a.output().unwrap().encode(),
                ceremony_b.output().unwrap().encode()
            );
        }

        /// W-09 (3): Signed-but-garbage logs pass signature check (`verify_dealer_log`)
        /// but are uniformly rejected by `observe`: `< 2f + 1` valid logs always fail
        /// even when padded with garbage logs to `>= 2f + 1`, whereas `>= 2f + 1`
        /// valid logs filter out the garbage log and reconstruct the exact same output
        /// as the valid-only set.
        #[test]
        fn dkg_observe_gate_rejects_garbage_uniformly(
            seed in any::<[u8; 32]>(),
            garbage_slot in 0usize..4,
            valid_count in 0usize..=3,
        ) {
            let fixture = &*RESHARE_PARITY_FIXTURE;
            let max_players = std::num::NonZeroU32::new(fixture.participants.len() as u32).unwrap();
            let ceremony = ceremony::DkgCeremony::new(
                Epoch::new(1),
                fixture.info.clone(),
                max_players,
                fixture.dealers.clone(),
            );

            let mut with_garbage = BTreeMap::new();
            let verified_garbage = ceremony
                .verify_dealer_log(&fixture.garbage_signed_bytes[garbage_slot].1)
                .unwrap();
            with_garbage.insert(verified_garbage.dealer, verified_garbage.log);

            let mut valid_only = BTreeMap::new();
            for (slot, (_, bytes)) in fixture
                .valid_signed_bytes
                .iter()
                .enumerate()
                .filter(|(slot, _)| *slot != garbage_slot)
                .take(valid_count)
            {
                let _ = slot;
                let verified = ceremony.verify_dealer_log(bytes).unwrap();
                with_garbage.insert(verified.dealer.clone(), verified.log.clone());
                valid_only.insert(verified.dealer, verified.log);
            }

            let mut rng_gate = ChaCha20Rng::from_seed(seed);
            let mut rng_reconstruct = ChaCha20Rng::from_seed(seed);
            let gate = crate::dkg_actor::chain_finalized_reconstructable_with_rng(
                &fixture.info,
                &with_garbage,
                &mut rng_gate,
            );
            let reconstructed = ceremony::try_reconstruct_with_rng(
                &fixture.info,
                &with_garbage,
                &mut rng_reconstruct,
            );

            if valid_count < 3 {
                prop_assert!(!gate);
                prop_assert!(reconstructed.is_err());
            } else {
                prop_assert!(gate);
                let out_with_garbage = reconstructed.unwrap();
                let out_valid_only = ceremony::try_reconstruct_with_rng(
                    &fixture.info,
                    &valid_only,
                    &mut ChaCha20Rng::from_seed(seed),
                )
                .unwrap();
                prop_assert_eq!(out_with_garbage.encode(), out_valid_only.encode());
            }
        }
    }
}
