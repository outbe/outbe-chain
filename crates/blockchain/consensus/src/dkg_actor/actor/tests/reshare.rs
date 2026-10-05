use super::*;
mod fixtures;
use fixtures::*;

#[test]
fn reshare_allows_new_player_without_previous_share() {
    use commonware_runtime::Runner as _;
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let PreviousCommittee {
                keys: old_keys,
                participants: old_participants,
                output: previous_output,
                shares: previous_shares,
            } = previous_committee();

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

            let verification =
                LogVerification::new(&old_participants, &previous_output, &target_participants)
                    .expect("valid reshare verification fixture");

            let (senders, receivers) = build_mock_network(&target_keys);
            let Players {
                mut progress_rx,
                finalized_log_txs,
                handles,
            } = spawn_players(
                &context,
                PlayerCommittee {
                    keys: &target_keys,
                    participants: &target_participants,
                    previous: PreviousShares {
                        keys: &old_keys,
                        output: &previous_output,
                        shares: &previous_shares,
                    },
                },
                (senders, receivers),
            );

            let log_threshold = old_participants.quorum::<N3f1>();
            let chain_logs = verification
                .collect(
                    &mut progress_rx,
                    LogSelection {
                        threshold: log_threshold as usize,
                        required: None,
                    },
                    |dealer| {
                        assert_ne!(*dealer, new_pk, "new player must not act as reshare dealer");
                        true
                    },
                )
                .await
                .expect("collect signed dealer logs");

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
    use commonware_runtime::Runner as _;
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let PreviousCommittee {
                keys,
                participants,
                output: previous_output,
                shares: previous_shares,
            } = previous_committee();
            let verification = LogVerification::new(&participants, &previous_output, &participants)
                .expect("valid reshare verification fixture");

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
            let Players {
                mut progress_rx,
                finalized_log_txs,
                handles,
            } = spawn_players(
                &context,
                PlayerCommittee {
                    keys: &keys,
                    participants: &participants,
                    previous: PreviousShares {
                        keys: &keys,
                        output: &previous_output,
                        shares: &previous_shares,
                    },
                },
                (senders, receivers),
            );

            let chain_logs = verification
                .collect(
                    &mut progress_rx,
                    LogSelection {
                        threshold: participants.len(),
                        required: None,
                    },
                    |_| true,
                )
                .await
                .expect("collect signed dealer logs");
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
    use commonware_runtime::Runner as _;
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            // Old committee of 4 (f=1, log_threshold = 2f+1 = 3), resharing to the
            // same 4 so all are dealers AND players.
            let PreviousCommittee {
                keys,
                participants,
                output: previous_output,
                shares: previous_shares,
            } = previous_committee();

            let verification = LogVerification::new(&participants, &previous_output, &participants)
                .expect("valid reshare verification fixture");

            let info = &verification.info;

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
                garbage_signed.clone().check(info).is_some(),
                "garbage dealer log must pass the signature-only acceptance check (that is C1)"
            );
            let garbage_bytes = Bytes::from(garbage_signed.encode());

            let (senders, receivers) = build_mock_network(&keys);
            let Players {
                mut progress_rx,
                finalized_log_txs,
                handles,
            } = spawn_players(
                &context,
                PlayerCommittee {
                    keys: &keys,
                    participants: &participants,
                    previous: PreviousShares {
                        keys: &keys,
                        output: &previous_output,
                        shares: &previous_shares,
                    },
                },
                (senders, receivers),
            );

            // Collect VALID dealer logs from dealers 1..=3 (skip dealer 0 - its slot
            // is taken by the garbage log). We need 2f+1 = 3 valid logs.
            let valid_logs = verification
                .collect(
                    &mut progress_rx,
                    LogSelection {
                        threshold: 3,
                        required: None,
                    },
                    |dealer| *dealer != byzantine_pk,
                )
                .await
                .expect("collect signed dealer logs");

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
    use commonware_runtime::Runner as _;
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let PreviousCommittee {
                keys: old_keys,
                participants: old_participants,
                output: previous_output,
                shares: previous_shares,
            } = previous_committee();

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

            let verification =
                LogVerification::new(&old_participants, &previous_output, &target_participants)
                    .expect("valid reshare verification fixture");

            let (senders, receivers) = build_mock_network(&old_keys);
            let (
                Players {
                    mut progress_rx,
                    finalized_log_txs,
                    handles: player_handles,
                },
                dealer_only_handle,
            ) = spawn_with_removed_dealer(
                &context,
                PlayerCommittee {
                    keys: &old_keys,
                    participants: &target_participants,
                    previous: PreviousShares {
                        keys: &old_keys,
                        output: &previous_output,
                        shares: &previous_shares,
                    },
                },
                &removed_pk,
                (senders, receivers),
            );

            let log_threshold = old_participants.quorum::<N3f1>();
            let chain_logs = verification
                .collect(
                    &mut progress_rx,
                    LogSelection {
                        threshold: log_threshold as usize,
                        required: Some(&removed_pk),
                    },
                    |_| true,
                )
                .await
                .expect("collect signed dealer logs");

            let selected_logs = threshold_logs_with_dealer(&chain_logs, &removed_pk, log_threshold);
            broadcast_selected_logs(selected_logs, &finalized_log_txs);

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

fn threshold_logs_with_dealer(
    chain_logs: &BTreeMap<bls12381::PublicKey, Bytes>,
    removed_pk: &bls12381::PublicKey,
    log_threshold: u32,
) -> Vec<Bytes> {
    assert!(
        chain_logs.contains_key(removed_pk),
        "removed old validator must publish a valid dealer log"
    );
    let mut selected_logs = Vec::with_capacity(log_threshold as usize);
    selected_logs.push(chain_logs.get(removed_pk).unwrap().clone());
    for (dealer, bytes) in chain_logs {
        if dealer == removed_pk {
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
    selected_logs
}

fn broadcast_selected_logs(
    selected_logs: Vec<Bytes>,
    finalized_log_txs: &[mpsc::UnboundedSender<Bytes>],
) {
    for bytes in selected_logs {
        for tx in finalized_log_txs {
            tx.send(bytes.clone()).unwrap();
        }
    }
}
