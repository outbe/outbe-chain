use super::*;

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[test]
fn test_initial_dkg_3_nodes() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            // Generate 3 validator keys, sorted by public key (same as bootstrap).
            let mut keys: Vec<bls12381::PrivateKey> = (0..3)
                .map(|_| {
                    bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                        rand_commonware::rngs::SysRng,
                    ))
                })
                .collect();
            keys.sort_by_key(|a| a.public_key().encode());

            let participants: Set<bls12381::PublicKey> = keys
                .iter()
                .map(|k| k.public_key())
                .try_collect::<Set<bls12381::PublicKey>>()
                .unwrap();

            let (senders, receivers) = build_mock_network(&keys);

            // Spawn all 3 DKG ceremonies concurrently.
            let mut handles = Vec::new();
            for (i, ((key, sender), receiver)) in
                keys.iter().cloned().zip(senders).zip(receivers).enumerate()
            {
                let p = participants.clone();
                // `Context` is not `Clone` on commonware 2026.5.0; obtain a
                // fresh owned clock for the spawned ceremony via `Supervisor::child`.
                handles.push(
                    context
                        .child("dkg_ceremony")
                        .spawn(move |clock| async move {
                            let result = run_initial_dkg(
                                &clock, key, p, None, None, 0, None, None, sender, receiver,
                            )
                            .await;
                            (i, result)
                        }),
                );
            }

            // Collect results.
            let mut results = Vec::new();
            for handle in handles {
                let (i, result) = handle.await.unwrap();
                let complete = result.unwrap_or_else(|e| panic!("node {i} DKG failed: {e}"));
                results.push(complete);
            }

            // All nodes must get the same polynomial (Output).
            let poly_0 = results[0].output.public().encode();
            for (i, r) in results.iter().enumerate().skip(1) {
                assert_eq!(
                    poly_0,
                    r.output.public().encode(),
                    "node {i} polynomial differs from node 0"
                );
            }

            // All shares must be distinct.
            let share_bytes: Vec<Vec<u8>> =
                results.iter().map(|r| r.share.encode().to_vec()).collect();
            for i in 0..share_bytes.len() {
                for j in (i + 1)..share_bytes.len() {
                    assert_ne!(
                        share_bytes[i], share_bytes[j],
                        "shares {i} and {j} are identical"
                    );
                }
            }

            // Participant sets must be identical.
            for (i, r) in results.iter().enumerate().skip(1) {
                assert_eq!(
                    results[0].participants.len(),
                    r.participants.len(),
                    "node {i} participant count differs"
                );
            }
        },
    );
}

#[test]
fn test_initial_dkg_4_nodes() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let mut keys: Vec<bls12381::PrivateKey> = (0..4)
                .map(|_| {
                    bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                        rand_commonware::rngs::SysRng,
                    ))
                })
                .collect();
            keys.sort_by_key(|a| a.public_key().encode());

            let participants: Set<bls12381::PublicKey> = keys
                .iter()
                .map(|k| k.public_key())
                .try_collect::<Set<bls12381::PublicKey>>()
                .unwrap();

            let (senders, receivers) = build_mock_network(&keys);

            let mut handles = Vec::new();
            for (key, sender, receiver) in keys
                .iter()
                .cloned()
                .zip(senders)
                .zip(receivers)
                .map(|((k, s), r)| (k, s, r))
            {
                let p = participants.clone();
                handles.push(
                    context
                        .child("dkg_ceremony")
                        .spawn(move |clock| async move {
                            run_initial_dkg(
                                &clock, key, p, None, None, 0, None, None, sender, receiver,
                            )
                            .await
                        }),
                );
            }

            let mut outputs = Vec::new();
            for (i, handle) in handles.into_iter().enumerate() {
                let result = handle.await.unwrap();
                outputs.push(result.unwrap_or_else(|e| panic!("node {i} DKG failed: {e}")));
            }

            // All nodes share the same polynomial.
            let poly_0 = outputs[0].output.public().encode();
            for (i, o) in outputs.iter().enumerate().skip(1) {
                assert_eq!(poly_0, o.output.public().encode(), "node {i} poly differs");
            }

            // 4 distinct shares.
            let mut share_set = std::collections::BTreeSet::new();
            for o in &outputs {
                assert!(share_set.insert(o.share.encode()), "duplicate share");
            }
        },
    );
}

#[test]
fn test_bootstrap_dkg_recovers_dropped_finalized_log_with_retry() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600)).start(
        |context| async move {
            let mut keys: Vec<bls12381::PrivateKey> = (0..4)
                .map(|seed| bls12381::PrivateKey::from_seed(seed + 1))
                .collect();
            keys.sort_by_key(|key| key.public_key().encode());

            let participants: Set<bls12381::PublicKey> = keys
                .iter()
                .map(|key| key.public_key())
                .try_collect::<Set<bls12381::PublicKey>>()
                .unwrap();

            let drop_rule = Arc::new(Mutex::new(DropMessages {
                from: Some(keys[0].public_key()),
                to: keys[1].public_key(),
                tag: 0x02,
                remaining: 1,
                dropped: 0,
            }));
            let (senders, receivers) = build_mock_network_with_drop(&keys, Some(drop_rule.clone()));

            let mut handles = Vec::new();
            for (i, ((key, sender), receiver)) in
                keys.iter().cloned().zip(senders).zip(receivers).enumerate()
            {
                let p = participants.clone();
                handles.push(
                    context
                        .child("dkg_ceremony")
                        .spawn(move |clock| async move {
                            let result = run_initial_dkg(
                                &clock, key, p, None, None, 0, None, None, sender, receiver,
                            )
                            .await;
                            (i, result)
                        }),
                );
            }

            let mut results = Vec::new();
            for handle in handles {
                let (i, result) = handle.await.unwrap();
                results.push(result.unwrap_or_else(|error| {
                    panic!("node {i} DKG failed after finalized-log retry: {error}")
                }));
            }

            assert!(
                drop_rule.lock().expect("drop rule lock").dropped == 1,
                "test must drop the first finalized log to exercise retry gossip"
            );

            let public_0 = results[0].output.public().encode();
            for (i, result) in results.iter().enumerate().skip(1) {
                assert_eq!(
                    public_0,
                    result.output.public().encode(),
                    "node {i} polynomial differs after finalized-log retry"
                );
            }
        },
    );
}

/// 4 validators, 1 offline. Threshold = 3 (N3f1: f=1, quorum=3).
/// Interactive bootstrap must wait instead of finalizing from a threshold
/// P2P subset that could diverge across validators or mismatch their boundary artifacts.
#[test]
fn test_bootstrap_dkg_waits_for_all_genesis_nodes_one_offline() {
    use commonware_runtime::{Clock as _, Runner as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60)).start(
        |context| async move {
            // Non-completion within a bounded virtual-time window (matches the
            // original outer 5s timeout): a non-canonical 3/4 subset must keep
            // waiting, never finalize. `select!` is biased; the sleep arm winning
            // is the pass condition.
            commonware_macros::select! {
                _ = run_partial_dkg(&context, 4, 3) => {
                    panic!("bootstrap DKG must not complete from a non-canonical 3/4 P2P subset");
                },
                _ = context.sleep(std::time::Duration::from_secs(5)) => {},
            }
        },
    );
}

/// 7 validators, 2 offline (maximum tolerable under N3f1).
/// Threshold = 5 (f=2, quorum=5). Bootstrap still must wait for the full
/// genesis dealer-log set; threshold liveness belongs to chain-finalized
/// reshare after blocks exist.
#[test]
fn test_bootstrap_dkg_waits_for_all_genesis_nodes_max_offline() {
    use commonware_runtime::{Clock as _, Runner as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60)).start(
        |context| async move {
            commonware_macros::select! {
                _ = run_partial_dkg(&context, 7, 5) => {
                    panic!("bootstrap DKG must not complete from a non-canonical 5/7 P2P subset");
                },
                _ = context.sleep(std::time::Duration::from_secs(5)) => {},
            }
        },
    );
}

/// 4 validators, only 2 online - below threshold (3).
/// The ceremony must time out, not hang forever.
#[test]
fn test_dkg_fails_below_threshold() {
    use commonware_runtime::{Clock as _, Runner as _};
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60))
            .start(|context| async move {
            // Use a short timeout to avoid slow test.
            // The DKG has DKG_TIMEOUT=120s, but we wrap with a shorter outer timeout.
            // The ceremony should not complete - it will hit DKG_TIMEOUT internally,
            // but we can't wait 120s in a test. Instead, verify it doesn't complete
            // within a reasonable window.
            // Should not complete - 2 nodes can't reach threshold=3.
            commonware_macros::select! {
                _ = run_partial_dkg(&context, 4, 2) => {
                    panic!("DKG should NOT complete with only 2/4 nodes online (below threshold=3)");
                },
                _ = context.sleep(std::time::Duration::from_secs(5)) => {},
            }
        });
}
