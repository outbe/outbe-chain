use super::*;

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

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
                                &clock,
                                InitialDkgFixture::bootstrap(key, p),
                                None,
                                None,
                                (sender, receiver),
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

            // All nodes share the same polynomial and participant set.
            let poly_0 = outputs[0].output.public().encode();
            for (i, o) in outputs.iter().enumerate().skip(1) {
                assert_eq!(poly_0, o.output.public().encode(), "node {i} poly differs");
                assert_eq!(
                    outputs[0].participants.len(),
                    o.participants.len(),
                    "node {i} participant count differs"
                );
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
                                &clock,
                                InitialDkgFixture::bootstrap(key, p),
                                None,
                                None,
                                (sender, receiver),
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

/// Interactive bootstrap must wait for all genesis nodes rather than finalizing
/// from a threshold P2P subset (3/4 or 5/7) or below-threshold subset (2/4).
#[test]
fn test_bootstrap_dkg_waits_for_all_genesis_nodes_when_any_offline() {
    use commonware_runtime::{Clock as _, Runner as _};
    for (total, online) in [(4usize, 3usize), (7, 5), (4, 2)] {
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60)).start(
            move |context| async move {
                commonware_macros::select! {
                    _ = run_partial_dkg(&context, total, online) => {
                        panic!("bootstrap DKG must not complete with {online}/{total} online");
                    },
                    _ = context.sleep(std::time::Duration::from_secs(5)) => {},
                }
            },
        );
    }
}
