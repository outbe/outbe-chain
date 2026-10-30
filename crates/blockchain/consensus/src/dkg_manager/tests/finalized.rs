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

fn run_round_with_offline_player(
    keys: &[bls12381::PrivateKey],
    participants: Set<bls12381::PublicKey>,
    offline_player_idx: usize,
    round: u64,
) -> (
    Info<MinSig, bls12381::PublicKey>,
    Output<MinSig, bls12381::PublicKey>,
    Vec<Bytes>,
) {
    use rand_commonware::SeedableRng as _;

    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &config::outbe_app_namespace(),
        round,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .unwrap();

    let mut dealers = Vec::new();
    let mut pub_msgs = Vec::new();
    let mut all_priv_msgs = Vec::new();
    for (idx, key) in keys.iter().enumerate() {
        let (dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_commonware::rngs::ChaCha20Rng::from_seed([idx as u8 + 1; 32]),
            info.clone(),
            key.clone(),
            None,
        )
        .unwrap();
        dealers.push(dealer);
        pub_msgs.push(pub_msg);
        all_priv_msgs.push(priv_msgs);
    }

    let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|k| Player::new(info.clone(), k.clone()).unwrap())
        .collect();

    // Only online players (all except `offline_player_idx`) send ACKs, forcing
    // every dealer to reveal the offline player's share in its finalized log.
    for (dealer_idx, dealer_key) in keys.iter().enumerate() {
        let dealer_pk = dealer_key.public_key();
        let pub_msg = &pub_msgs[dealer_idx];
        for (player_pk, priv_msg) in &all_priv_msgs[dealer_idx] {
            let player_idx = keys
                .iter()
                .position(|k| &k.public_key() == player_pk)
                .unwrap();
            if player_idx == offline_player_idx {
                continue;
            }
            let ack = players[player_idx]
                .dealer_message::<N3f1>(dealer_pk.clone(), pub_msg.clone(), priv_msg.clone())
                .unwrap()
                .unwrap();
            dealers[dealer_idx]
                .receive_player_ack(player_pk.clone(), ack)
                .unwrap();
        }
    }

    let mut signed_logs = BTreeMap::new();
    let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
    for dealer in dealers {
        let signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey> = dealer.finalize::<N3f1>();
        let encoded = Bytes::from(signed_log.encode());
        let (pk, log) = signed_log.check(&info).unwrap();
        signed_logs.insert(pk.clone(), encoded);
        dkg_logs.record(pk, log);
    }

    let output =
        observe::<MinSig, bls12381::PublicKey, N3f1, commonware_cryptography::bls12381::Batch>(
            &mut rand_commonware::rngs::ChaCha20Rng::from_seed([0xA5; 32]),
            dkg_logs,
            &Sequential,
        )
        .unwrap();
    (info, output, signed_logs.into_values().collect())
}

#[test]
fn revealed_set_reconstructible_from_chain() {
    let mut keys: Vec<_> = (40..44).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let offline_pk = keys[3].public_key();
    let (_info, output, signed_logs) =
        run_round_with_offline_player(&keys, participants.clone(), 3, 21);
    assert_eq!(
        output.revealed().iter().cloned().collect::<Vec<_>>(),
        vec![offline_pk.clone()],
        "ceremony with 1 offline player must reveal that player's share"
    );

    let boundary = build_boundary_artifact(BoundaryArtifactInput {
        epoch: Epoch::new(4),
        validator_set: &validator_set_from_keys(&keys),
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 4,
        freeze_height: 239,
        planned_activation_height: 240,
        vrf_material_version: 4,
        is_validator_set_change: false,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();

    // Node A reconstructs the canonical output from chain-finalized DealerLog
    // headers alone (no local Player state) and commits the boundary via live
    // finalization delivery.
    let node_a = Mailbox::new();
    node_a
        .note_ceremony_started(Epoch::new(4), 21, None, participants.clone())
        .unwrap();
    for log_bytes in &signed_logs {
        node_a.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
            log_bytes.clone(),
        )));
    }
    let reconstructed_a = node_a
        .canonical_output(Epoch::new(4))
        .expect("canonical output reconstructed from finalized dealer logs");
    assert_eq!(
        reconstructed_a.revealed(),
        output.revealed(),
        "public chain-finalized dealer logs must reconstruct the exact revealed set"
    );
    node_a.note_ceremony_completed(boundary.clone());
    node_a.note_finalized_header_artifact_at(
        241,
        B256::repeat_byte(0x41),
        Some(&ConsensusHeaderArtifact::BoundaryOutcome(boundary.clone())),
    );
    let record_a = node_a
        .latest_revealed_set_record()
        .expect("commit_finalized_boundary must record RevealedSetRecord");

    // Node B never ran the ceremony; it adopts the finalized boundary carrier
    // from canonical chain state.
    let node_b = Mailbox::new();
    node_b.note_recovered_pending_boundary(boundary.clone());
    assert!(node_b
        .adopt_finalized_boundary(241, B256::repeat_byte(0x41), &boundary)
        .unwrap());
    let record_b = node_b
        .latest_revealed_set_record()
        .expect("adopt_finalized_boundary must record RevealedSetRecord");

    assert_eq!(record_a.epoch, 4);
    assert_eq!(record_a.boundary_height, 241);
    assert_eq!(record_a.revealed, vec![offline_pk]);
    assert_eq!(record_a, record_b);
    assert_eq!(
        record_a.encode(),
        record_b.encode(),
        "independent nodes must produce byte-identical RevealedSetRecord encodings"
    );

    // Mismatched boundary epoch vs ODKO epoch must return a typed error.
    let mut mismatched = boundary.clone();
    mismatched.epoch = 5;
    assert!(matches!(
        RevealedSetRecord::from_finalized_boundary(241, &mismatched),
        Err(crate::errors::DkgError::BoundaryOutcomeEpochMismatch {
            boundary_epoch: 5,
            outcome_epoch: 4,
        })
    ));

    // Genesis (block_number == 0) and group-key-only bootstrap outcomes must return Ok(None).
    assert_eq!(
        RevealedSetRecord::from_finalized_boundary(0, &boundary).unwrap(),
        None
    );
    let mut raw_group_key_boundary = boundary.clone();
    raw_group_key_boundary.outcome =
        Bytes::copy_from_slice(output.public().public().encode().as_ref());
    assert_eq!(
        RevealedSetRecord::from_finalized_boundary(241, &raw_group_key_boundary).unwrap(),
        None
    );
    let mut legacy_v1_boundary = boundary.clone();
    legacy_v1_boundary.outcome = legacy_group_key_only_outcome(Epoch::new(4), &output, true);
    assert_eq!(
        RevealedSetRecord::from_finalized_boundary(241, &legacy_v1_boundary).unwrap(),
        None
    );

    // Empty outcome at block_number > 0 is a malformed ODKO and must return a typed error.
    let mut empty_outcome_boundary = boundary;
    empty_outcome_boundary.outcome = Bytes::new();
    assert!(matches!(
        RevealedSetRecord::from_finalized_boundary(241, &empty_outcome_boundary),
        Err(crate::errors::DkgError::InvalidBoundaryOutcome(
            crate::dkg_manager::OdkoDecodeError::TooShort(0)
        ))
    ));
}

#[test]
fn revealed_share_triggers_degradation_and_record() {
    use crate::vrf_safety::VrfSafetyGate;
    use outbe_primitives::consensus::RandomnessStatus;

    let mut keys: Vec<_> = (50..54).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let offline_pk = keys[2].public_key();
    let (_info, revealed_output, _signed_logs) =
        run_round_with_offline_player(&keys, participants.clone(), 2, 31);
    assert_eq!(revealed_output.revealed().len(), 1);

    let gate = VrfSafetyGate::new(1, 100, 200, 20);
    assert_eq!(gate.snapshot().randomness_status, RandomnessStatus::Healthy);

    // 0. W-05.1(c): Transient `mark_degraded` on a VRF seed miss sets `Degraded`
    //    without setting `active_material_revealed`, so a subsequent rotation
    //    transition (`note_preparing`) clears `Degraded`.
    gate.mark_degraded();
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Degraded
    );
    gate.note_preparing(2, 180, 200, 20);
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Preparing,
        "transient mark_degraded must not set persistent active_material_revealed flag"
    );
    gate.note_activated(1, 100, 200, 20);
    assert_eq!(gate.snapshot().randomness_status, RandomnessStatus::Healthy);

    // 1. W-05.1(a)/(b) + D3-B: Immediate degradation upon ceremony completion
    //    (`apply_pending_ceremony_output`) and sticky preservation across rotation
    //    status updates (`note_preparing`, `note_pending_activation`, `note_grace`,
    //    `note_activated`).
    gate.apply_pending_ceremony_output(Some(&revealed_output));
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Degraded
    );

    gate.note_pending_activation(200, 20);
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Degraded
    );
    gate.note_grace(200, 20);
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Degraded
    );
    gate.note_activated(2, 200, 300, 20);
    gate.apply_revealed_output(&revealed_output);
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Degraded
    );
    gate.note_preparing(3, 280, 300, 20);
    assert_eq!(
        gate.snapshot().randomness_status,
        RandomnessStatus::Degraded,
        "prepare window must not overwrite Degraded while active material has revealed shares"
    );

    // 2. D2-A: Degraded is status-only (has_threshold_shares() == false,
    //    is_consensus_active() == true) and does not block block production inside expiry.
    let status = gate.snapshot().randomness_status;
    assert!(!status.has_threshold_shares());
    assert!(status.is_consensus_active());
    assert!(gate.ensure_block_allowed(320).is_ok());
    assert!(gate.ensure_verify_height_allowed(320).is_ok());

    // 3. Finalized boundary records the exact RevealedSetRecord.
    let boundary = build_boundary_artifact(BoundaryArtifactInput {
        epoch: Epoch::new(2),
        validator_set: &validator_set_from_keys(&keys),
        output: &revealed_output,
        is_full_dkg: true,
        dkg_cycle: 2,
        freeze_height: 199,
        planned_activation_height: 200,
        vrf_material_version: 2,
        is_validator_set_change: false,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let manager = Mailbox::new();
    manager.note_ceremony_completed(boundary.clone());
    manager.note_finalized_header_artifact_at(
        201,
        B256::repeat_byte(0x52),
        Some(&ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
    );
    let record = manager
        .latest_revealed_set_record()
        .expect("RevealedSetRecord must be recorded on boundary commit");
    assert_eq!(record.epoch, 2);
    assert_eq!(record.boundary_height, 201);
    assert_eq!(record.revealed, vec![offline_pk]);

    // 4. W-05.1(d): Activating a clean output with empty `revealed()` clears `Degraded` back to `Healthy`.
    let (_clean_info, clean_output, _shares, _logs, _clean_signed) =
        run_round(&keys, participants, None, None, 32);
    assert!(clean_output.revealed().is_empty());
    gate.note_activated(3, 300, 400, 20);
    gate.apply_revealed_output(&clean_output);
    assert_eq!(gate.snapshot().randomness_status, RandomnessStatus::Healthy);
}

#[test]
fn participant_count_at_max_validators_is_exact() {
    let max = crate::bls::MAX_VALIDATORS as usize;
    assert_eq!(max, 256);

    let mut keys: Vec<_> = (1..=max as u64)
        .map(bls12381::PrivateKey::from_seed)
        .collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    assert_eq!(participants.len(), max);
    assert_eq!(u32::try_from(participants.len()), Ok(256u32));

    let ceremony_id = crate::dkg_actor::wire::DkgCeremonyId::new(
        &config::outbe_app_namespace(),
        0,
        None,
        &participants,
    );
    assert_eq!(ceremony_id.round, 0);

    let _info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &config::outbe_app_namespace(),
        0,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .expect("Info::new must accept MAX_VALIDATORS = 256 participants");

    let bootstrap = crate::bls::bootstrap_dkg_for_participants(participants.clone())
        .expect("bootstrap_dkg_for_participants must accept MAX_VALIDATORS = 256 participants");
    assert_eq!(bootstrap.shares.len(), max);
    assert_eq!(bootstrap.output.players().len(), max);

    let validator_set = ValidatorSet {
        public_keys: keys.iter().map(bls12381::PrivateKey::public_key).collect(),
        addresses: (0..max)
            .map(|idx| {
                alloy_primitives::Address::from_word(B256::from(alloy_primitives::U256::from(
                    idx + 1,
                )))
            })
            .collect(),
        p2p_addresses: vec![crate::validators::ValidatorP2pAddress::Missing; max],
    };
    let boundary = build_boundary_artifact(BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &bootstrap.output,
        is_full_dkg: true,
        dkg_cycle: 1,
        freeze_height: 59,
        planned_activation_height: 60,
        vrf_material_version: 1,
        is_validator_set_change: false,
        tee_expired_target_exclusions: Vec::new(),
    })
    .expect("build_boundary_artifact must accept MAX_VALIDATORS = 256 output");
    assert_eq!(boundary.epoch, 1);
    let record_256 = RevealedSetRecord::from_finalized_boundary(61, &boundary)
        .expect("RevealedSetRecord::from_finalized_boundary must decode 256-validator boundary")
        .expect("non-genesis ODKO boundary must produce a RevealedSetRecord");
    assert!(record_256.revealed.is_empty());

    let manager = Mailbox::new();
    manager
        .note_ceremony_started(Epoch::new(1), 0, None, participants)
        .expect("Mailbox::note_ceremony_started must accept 256 participants");

    // Empty participant set must still return typed errors.
    let empty: Set<bls12381::PublicKey> = std::iter::empty().try_collect().unwrap();
    assert!(crate::bls::bootstrap_dkg_for_participants(empty.clone()).is_err());
    let empty_err = manager
        .note_ceremony_started(Epoch::new(2), 0, None, empty)
        .unwrap_err();
    assert!(matches!(
        empty_err.downcast_ref::<crate::errors::DkgError>(),
        Some(crate::errors::DkgError::NoParticipants)
    ));
}
