use super::*;

#[test]
fn reshare_allows_new_player_without_previous_share() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let mut old_keys: Vec<bls12381::PrivateKey> =
                (1..=4).map(bls12381::PrivateKey::from_seed).collect();
            old_keys.sort_by_key(|key| key.public_key().encode());
            let (old_participants, previous_output, previous_shares) =
                run_direct_initial_round(&old_keys);

            let new_key = bls12381::PrivateKey::from_seed(100);
            let new_pk = new_key.public_key();
            let mut target_keys = old_keys.clone();
            target_keys.push(new_key);
            target_keys.sort_by_key(|key| key.public_key().encode());
            let target_participants: Set<bls12381::PublicKey> = target_keys
                .iter()
                .map(|key| key.public_key())
                .try_collect()
                .unwrap();

            let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
                &crate::config::outbe_app_namespace(),
                1,
                Some(previous_output.clone()),
                Mode::NonZeroCounter,
                commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
                old_participants.clone(),
                target_participants.clone(),
            )
            .unwrap();
            let max_players = NonZeroU32::new(target_participants.len() as u32).unwrap();

            let (senders, receivers) = build_mock_network(&target_keys);
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
            let mut finalized_log_txs = Vec::new();
            let mut handles = Vec::new();

            for ((key, sender), receiver) in target_keys.iter().cloned().zip(senders).zip(receivers)
            {
                let participants = target_participants.clone();
                let prev_output = previous_output.clone();
                let prev_share = old_keys
                    .iter()
                    .position(|old_key| old_key.public_key() == key.public_key())
                    .map(|idx| previous_shares[idx].clone());
                let progress_tx = progress_tx.clone();
                let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
                finalized_log_txs.push(finalized_log_tx);

                handles.push(
                    context
                        .child("dkg_ceremony")
                        .spawn(move |clock| async move {
                            run_initial_dkg(
                                &clock,
                                key,
                                participants,
                                Some(prev_output),
                                prev_share,
                                1,
                                Some(progress_tx),
                                Some(finalized_log_rx),
                                sender,
                                receiver,
                            )
                            .await
                        }),
                );
            }
            drop(progress_tx);

            let log_threshold = old_participants.quorum::<N3f1>();
            let mut chain_logs = BTreeMap::new();
            while chain_logs.len() < log_threshold as usize {
                let progress = progress_rx
                    .recv()
                    .await
                    .expect("progress channel should remain open");
                if let DkgProgress::LocalDealerLog(bytes) = progress {
                    let mut reader = bytes.as_ref();
                    let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                        &mut reader,
                        &max_players,
                    )
                    .unwrap();
                    let (dealer, _log) = signed_log.check(&info).unwrap();
                    assert_ne!(dealer, new_pk, "new player must not act as reshare dealer");
                    chain_logs.entry(dealer).or_insert(bytes);
                }
            }

            for bytes in chain_logs.values() {
                for tx in &finalized_log_txs {
                    tx.send(bytes.clone()).unwrap();
                }
            }

            let mut results = Vec::new();
            for handle in handles {
                results.push(handle.await.unwrap().unwrap());
            }

            let expected_public = results[0].output.public().encode();
            for result in &results {
                assert_eq!(result.output.public().encode(), expected_public);
                assert_eq!(result.participants, target_participants);
                assert!(
                    result.output.revealed().is_empty(),
                    "an all-online reshare must not publicly reveal any player's share"
                );
            }
            assert!(
                results
                    .iter()
                    .any(|result| result.participants.position(&new_pk).is_some()),
                "new player must be part of the reshared participant set"
            );
        },
    );
}

#[test]
fn reshare_retry_gives_online_player_time_to_ack_before_dealer_finalizes() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let mut keys: Vec<bls12381::PrivateKey> =
                (1..=4).map(bls12381::PrivateKey::from_seed).collect();
            keys.sort_by_key(|key| key.public_key().encode());
            let (participants, previous_output, previous_shares) = run_direct_initial_round(&keys);
            let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
                &crate::config::outbe_app_namespace(),
                1,
                Some(previous_output.clone()),
                Mode::NonZeroCounter,
                commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
                participants.clone(),
                participants.clone(),
            )
            .unwrap();
            let max_players = NonZeroU32::new(participants.len() as u32).unwrap();

            // Model a healthy player that disappears long enough for a real
            // node+enclave restart, then reconnects while the ceremony is still
            // recoverable.  Three complete retry rounds are dropped; the next
            // retry must still arrive before each dealer seals its log, otherwise
            // Feldman-Desmedt permanently publishes that player's share.
            let drop_rule = Arc::new(Mutex::new(DropMessages {
                from: None,
                to: keys[1].public_key(),
                tag: 0x00,
                remaining: 12,
                dropped: 0,
            }));
            let (senders, receivers) = build_mock_network_with_drop(&keys, Some(drop_rule.clone()));
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
            let mut finalized_log_txs = Vec::new();
            let mut handles = Vec::new();

            for (idx, ((key, sender), receiver)) in
                keys.iter().cloned().zip(senders).zip(receivers).enumerate()
            {
                let participant_set = participants.clone();
                let prev_output = previous_output.clone();
                let prev_share = previous_shares[idx].clone();
                let progress_tx = progress_tx.clone();
                let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
                finalized_log_txs.push(finalized_log_tx);
                handles.push(
                    context
                        .child("dkg_ceremony")
                        .spawn(move |clock| async move {
                            run_initial_dkg(
                                &clock,
                                key,
                                participant_set,
                                Some(prev_output),
                                Some(prev_share),
                                1,
                                Some(progress_tx),
                                Some(finalized_log_rx),
                                sender,
                                receiver,
                            )
                            .await
                        }),
                );
            }
            drop(progress_tx);

            let mut chain_logs = BTreeMap::new();
            while chain_logs.len() < participants.len() {
                let progress = progress_rx
                    .recv()
                    .await
                    .expect("progress channel should remain open");
                if let DkgProgress::LocalDealerLog(bytes) = progress {
                    let mut reader = bytes.as_ref();
                    let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                        &mut reader,
                        &max_players,
                    )
                    .unwrap();
                    let (dealer, _log) = signed_log.check(&info).unwrap();
                    chain_logs.entry(dealer).or_insert(bytes);
                }
            }
            for bytes in chain_logs.values() {
                for tx in &finalized_log_txs {
                    tx.send(bytes.clone()).unwrap();
                }
            }

            for handle in handles {
                let result = handle.await.unwrap().unwrap();
                assert!(
                    result.output.revealed().is_empty(),
                    "an online player that ACKs the retry must not have its share revealed"
                );
            }
            assert_eq!(
                drop_rule.lock().expect("drop rule lock").dropped,
                12,
                "test must delay every remote dealer through three retries"
            );
        },
    );
}

/// C1 regression: a byzantine ex-validator broadcasts a dealer log with a
/// VALID signature but GARBAGE content (dealt from a wrong previous share). It
/// passes the actor's signature-only acceptance check and lands in the raw
/// first-2f+1, but `observe`/`select` drop it. The old code broke on the raw
/// 2f+1 count and one-shot `finalize` then failed `DkgFailed` identically on
/// every node -> chain halt. With the observe-gated trigger the actors keep
/// collecting, complete on 2f+1 CONTENT-VALID logs, and every honest node
/// recovers a share (signer quorum preserved).
#[test]
fn reshare_survives_signed_but_garbage_dealer_log() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            // Old committee of 4 (f=1, log_threshold = 2f+1 = 3), resharing to the
            // same 4 so all are dealers AND players.
            let mut keys: Vec<bls12381::PrivateKey> =
                (1..=4).map(bls12381::PrivateKey::from_seed).collect();
            keys.sort_by_key(|key| key.public_key().encode());
            let (participants, previous_output, previous_shares) = run_direct_initial_round(&keys);

            let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
                &crate::config::outbe_app_namespace(),
                1,
                Some(previous_output.clone()),
                Mode::NonZeroCounter,
                commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
                participants.clone(),
                participants.clone(),
            )
            .unwrap();
            let max_players = NonZeroU32::new(participants.len() as u32).unwrap();

            // Byzantine dealer log from dealer 0, dealt with dealer 1's previous
            // share (wrong): dealer 0 signs it (-> passes `check`), but its content
            // is inconsistent with the previous output (-> dropped by `select`).
            let byzantine_pk = keys[0].public_key();
            let (byz_dealer, _pub, _priv) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                info.clone(),
                keys[0].clone(),
                Some(previous_shares[1].clone()),
            )
            .unwrap();
            let garbage_signed = byz_dealer.finalize::<N3f1>();
            assert!(
                garbage_signed.clone().check(&info).is_some(),
                "garbage dealer log must pass the signature-only acceptance check (that is C1)"
            );
            let garbage_bytes = Bytes::from(garbage_signed.encode());

            let (senders, receivers) = build_mock_network(&keys);
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
            let mut finalized_log_txs = Vec::new();
            let mut handles = Vec::new();

            for ((key, sender), receiver) in keys.iter().cloned().zip(senders).zip(receivers) {
                let participants = participants.clone();
                let prev_output = previous_output.clone();
                let prev_share = keys
                    .iter()
                    .position(|k| k.public_key() == key.public_key())
                    .map(|idx| previous_shares[idx].clone());
                let progress_tx = progress_tx.clone();
                let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
                finalized_log_txs.push(finalized_log_tx);
                handles.push(
                    context
                        .child("dkg_ceremony")
                        .spawn(move |clock| async move {
                            run_initial_dkg(
                                &clock,
                                key,
                                participants,
                                Some(prev_output),
                                prev_share,
                                1,
                                Some(progress_tx),
                                Some(finalized_log_rx),
                                sender,
                                receiver,
                            )
                            .await
                        }),
                );
            }
            drop(progress_tx);

            // Collect VALID dealer logs from dealers 1..=3 (skip dealer 0 - its slot
            // is taken by the garbage log). We need 2f+1 = 3 valid logs.
            let mut valid_logs: BTreeMap<bls12381::PublicKey, Bytes> = BTreeMap::new();
            while valid_logs.len() < 3 {
                let progress = progress_rx
                    .recv()
                    .await
                    .expect("progress channel should remain open");
                if let DkgProgress::LocalDealerLog(bytes) = progress {
                    let mut reader = bytes.as_ref();
                    let signed = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                        &mut reader,
                        &max_players,
                    )
                    .unwrap();
                    let (dealer, _log) = signed.check(&info).unwrap();
                    if dealer != byzantine_pk {
                        valid_logs.entry(dealer).or_insert(bytes);
                    }
                }
            }

            // Feed the GARBAGE log FIRST (so it lands in the raw first-2f+1), then
            // the 3 valid logs, to every actor.
            for tx in &finalized_log_txs {
                tx.send(garbage_bytes.clone()).unwrap();
                for bytes in valid_logs.values() {
                    tx.send(bytes.clone()).unwrap();
                }
            }

            let mut results = Vec::new();
            for handle in handles {
                // No halt: the old code returned Err("player finalize failed") here.
                results.push(handle.await.unwrap().unwrap());
            }

            // Canonical output agreed by all, and every node recovered a share
            // (quorum preserved) - the lone garbage log was filtered, not fatal.
            let expected_public = results[0].output.public().encode();
            for result in &results {
                assert_eq!(
                    result.output.public().encode(),
                    expected_public,
                    "all nodes must agree on the canonical output despite the garbage log"
                );
            }
        },
    );
}

#[test]
fn removed_old_validator_can_deal_without_being_target_player() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let mut old_keys: Vec<bls12381::PrivateKey> =
                (1..=4).map(bls12381::PrivateKey::from_seed).collect();
            old_keys.sort_by_key(|key| key.public_key().encode());
            let (old_participants, previous_output, previous_shares) =
                run_direct_initial_round(&old_keys);

            let removed_key = old_keys[0].clone();
            let removed_pk = removed_key.public_key();
            let target_keys: Vec<bls12381::PrivateKey> = old_keys
                .iter()
                .filter(|key| key.public_key() != removed_pk)
                .cloned()
                .collect();
            let target_participants: Set<bls12381::PublicKey> = target_keys
                .iter()
                .map(|key| key.public_key())
                .try_collect()
                .unwrap();
            assert!(target_participants.position(&removed_pk).is_none());

            let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
                &crate::config::outbe_app_namespace(),
                1,
                Some(previous_output.clone()),
                Mode::NonZeroCounter,
                commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
                old_participants.clone(),
                target_participants.clone(),
            )
            .unwrap();
            let max_players = NonZeroU32::new(target_participants.len() as u32).unwrap();

            let (senders, receivers) = build_mock_network(&old_keys);
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
            let mut finalized_log_txs = Vec::new();
            let mut player_handles = Vec::new();
            let mut dealer_only_handle = None;

            for ((key, sender), receiver) in old_keys.iter().cloned().zip(senders).zip(receivers) {
                let participants = target_participants.clone();
                let prev_output = previous_output.clone();
                let progress_tx = progress_tx.clone();
                let idx = old_keys
                    .iter()
                    .position(|old_key| old_key.public_key() == key.public_key())
                    .unwrap();
                let prev_share = previous_shares[idx].clone();

                if key.public_key() == removed_pk {
                    dealer_only_handle = Some(context.child("dkg_ceremony").spawn(
                        move |clock| async move {
                            run_reshare_dealer_only(
                                &clock,
                                key,
                                participants,
                                prev_output,
                                prev_share,
                                1,
                                progress_tx,
                                sender,
                                receiver,
                            )
                            .await
                        },
                    ));
                } else {
                    let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
                    finalized_log_txs.push(finalized_log_tx);
                    player_handles.push(context.child("dkg_ceremony").spawn(
                        move |clock| async move {
                            run_initial_dkg(
                                &clock,
                                key,
                                participants,
                                Some(prev_output),
                                Some(prev_share),
                                1,
                                Some(progress_tx),
                                Some(finalized_log_rx),
                                sender,
                                receiver,
                            )
                            .await
                        },
                    ));
                }
            }
            drop(progress_tx);

            let log_threshold = old_participants.quorum::<N3f1>();
            let mut chain_logs = BTreeMap::new();
            while chain_logs.len() < log_threshold as usize || !chain_logs.contains_key(&removed_pk)
            {
                let progress = progress_rx
                    .recv()
                    .await
                    .expect("progress channel should remain open");
                let DkgProgress::LocalDealerLog(bytes) = progress else {
                    continue;
                };
                let mut reader = bytes.as_ref();
                let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                    &mut reader,
                    &max_players,
                )
                .unwrap();
                let (dealer, _log) = signed_log.check(&info).unwrap();
                chain_logs.entry(dealer).or_insert(bytes);
            }

            assert!(
                chain_logs.contains_key(&removed_pk),
                "removed old validator must publish a valid dealer log"
            );
            let mut selected_logs = Vec::with_capacity(log_threshold as usize);
            selected_logs.push(chain_logs.get(&removed_pk).unwrap().clone());
            for (dealer, bytes) in &chain_logs {
                if dealer == &removed_pk {
                    continue;
                }
                selected_logs.push(bytes.clone());
                if selected_logs.len() == log_threshold as usize {
                    break;
                }
            }
            assert_eq!(
                selected_logs.len(),
                log_threshold as usize,
                "test must feed a threshold set including the removed dealer"
            );
            for bytes in selected_logs {
                for tx in &finalized_log_txs {
                    tx.send(bytes.clone()).unwrap();
                }
            }

            let dealer_only_result = dealer_only_handle
                .expect("dealer-only task must be spawned for removed validator")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(dealer_only_result.participants, target_participants);

            let mut results = Vec::new();
            for handle in player_handles {
                results.push(handle.await.unwrap().unwrap());
            }

            let expected_public = results[0].output.public().encode();
            for result in &results {
                assert_eq!(result.output.public().encode(), expected_public);
                assert_eq!(result.participants, target_participants);
            }
        },
    );
}
