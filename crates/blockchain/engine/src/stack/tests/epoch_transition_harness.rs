//! Epoch-transition invariants on `ordered::Set` and the multi-node simplex
//! deterministic harness.

use super::*;

// =============================================================================
// T3 - ordered::Set index shift on prefix-sort join (must pass).
//
// Prepending a BLS pubkey that sorts before all existing keys to an ordered::Set
// shifts the indices of every original key by +1. Production code that builds
// `participants` from a live 4-key set after a 3-key DKG would therefore observe
// participant indices that no longer match the share.index baked into the
// saved DKG output (hybrid.rs:472-481, invariant).
//
// This is a structural assertion about ordered::Set, not a probabilistic one.
// =============================================================================
#[test]
fn ordered_set_index_shift_on_prefix_join() {
    use commonware_utils::ordered::Set;

    // Generate 3 BLS pubkeys deterministically.
    let mut keys: Vec<bls12381::PrivateKey> =
        (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|k| commonware_codec::Encode::encode(&k.public_key()));

    let participants_3: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();

    // Capture each original participant's index in the 3-key set.
    let original_indices: Vec<(bls12381::PublicKey, commonware_utils::Participant)> = keys
        .iter()
        .map(|k| {
            let pk = k.public_key();
            let idx = participants_3.index(&pk).unwrap();
            (pk, idx)
        })
        .collect();

    // Find a 4th BLS pubkey whose encoding sorts before all 3 originals.
    // BLS pubkeys are compressed G1 elements with byte values uniformly
    // distributed enough that a sort-before key is found within a small
    // seed window in practice.
    let smallest = commonware_codec::Encode::encode(&keys[0].public_key());
    let new_key = (4u64..1_000_000)
        .find_map(|seed| {
            let candidate = bls12381::PrivateKey::from_seed(seed);
            let bytes = commonware_codec::Encode::encode(&candidate.public_key());
            if bytes < smallest {
                Some(candidate)
            } else {
                None
            }
        })
        .expect("could not find a sort-before BLS pubkey within seed window");

    // Build a 4-key participants set including the new key + the 3 originals.
    let mut all_4: Vec<bls12381::PublicKey> = keys.iter().map(|k| k.public_key()).collect();
    all_4.push(new_key.public_key());
    let participants_4: Set<bls12381::PublicKey> = all_4.into_iter().try_collect().unwrap();

    // The new key sits at position 0; every original key shifts by +1.
    let mut shifted_count = 0usize;
    for (pk, original_idx) in &original_indices {
        let new_idx = participants_4.index(pk).unwrap();
        if new_idx != *original_idx {
            shifted_count += 1;
        }
    }

    assert_eq!(
        shifted_count,
        original_indices.len(),
        "expected every original participant's index to shift by +1 after prepending a sort-before key, but {} of {} shifted",
        shifted_count,
        original_indices.len()
    );

    // The new key occupies sorted position 0 in the 4-key set.
    let new_pk = new_key.public_key();
    assert_eq!(
        participants_4.index(&new_pk).unwrap().get(),
        0,
        "newly prepended key must occupy sorted position 0"
    );
}

// =============================================================================
// T1 / T2a / T2b / T5 - multi-node simplex deterministic harness.
//
// These tests run the actual `simplex::Engine` over a deterministic
// simulated network with outbe-chain's `HybridScheme<MinSig>` and the
// shared `crate::epoch_subchannels::register_epoch_subchannels` /
// `take_or_register_current` helper that production also uses in
// `stack.rs`. Toggling `use_pre_registration` in the harness switches
// between the pre-fix lazy path and the post-fix pre-register path.
//
// Foundation tests T0 (`muxer_contract::*`) and T3
// (`ordered_set_index_shift_on_prefix_join`) above pin the underlying
// commonware-p2p Muxer contract and `ordered::Set` ordering invariant
// respectively.
// =============================================================================

#[test]
fn epoch_transition_finalizes_view_one() {
    use commonware_consensus::types::{Epoch, View};
    use commonware_runtime::{deterministic, Runner};
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(30));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        // Epoch::new(2) -> RoundRobin leader = (2+1) % 3 = 0; arbitrary
        // baseline cycle.
        let outcome = harness
            .run_cycle(
                Epoch::new(2),
                outbe_consensus::test_harness::CycleOptions {
                    use_pre_registration: true,
                    leader_timeout: Duration::from_millis(500),
                    run_for: Duration::from_millis(2_000),
                    ..Default::default()
                },
            )
            .await;
        assert!(
            outcome.all_finalized_view_one(),
            "T1 baseline: every node must finalize view 1; got {:?}",
            outcome.view_finalized_per_node
        );
        let _ = View::new(1);
    });
}

#[test]
fn cross_node_race_stalls_under_lazy_registration() {
    use commonware_consensus::types::Epoch;
    use commonware_runtime::{deterministic, Runner};
    use std::collections::HashMap;
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(30));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        let epoch = Epoch::new(2); // leader index = (2+1) % 3 = 0
        let leader = harness.leader_for_view_one(epoch);

        // Identical timing to T2b. Only `use_pre_registration: false`
        // differs. In the lazy path, `dkg_completion_delay` is ignored
        // (no pre-register) so followers' Mux registers the new epoch
        // only at `activation_delay = 500ms`. Leader fires at 150ms;
        // 150-500ms window has no follower route -> Mux drop -> stall.
        let mut dkg_completion = HashMap::new();
        let mut activation = HashMap::new();
        for i in 0..3 {
            if i == leader {
                dkg_completion.insert(i, Duration::from_millis(0));
                activation.insert(i, Duration::from_millis(150));
            } else {
                dkg_completion.insert(i, Duration::from_millis(100));
                activation.insert(i, Duration::from_millis(500));
            }
        }

        let outcome = harness
            .run_cycle(
                epoch,
                outbe_consensus::test_harness::CycleOptions {
                    use_pre_registration: false,
                    dkg_completion_delay_per_node: dkg_completion,
                    activation_delay_per_node: activation,
                    leader_timeout: Duration::from_millis(500),
                    // Discriminating window: just past followers'
                    // activation, before view-1 nullification could
                    // recover into view 2.
                    run_for: Duration::from_millis(750),
                },
            )
            .await;

        // At least one follower's view-1 must NOT have finalized.
        let any_follower_stalled = outcome
            .followers()
            .any(|i| !outcome.view_finalized_per_node[i]);
        assert!(
            any_follower_stalled,
            "T2a: at least one follower must fail to finalize view 1 under lazy \
             registration; outcome={:?}",
            outcome.view_finalized_per_node
        );
    });
}

#[test]
fn pre_register_helper_avoids_cross_node_race() {
    use commonware_consensus::types::Epoch;
    use commonware_runtime::{deterministic, Runner};
    use std::collections::HashMap;
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(30));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        let epoch = Epoch::new(2);
        let leader = harness.leader_for_view_one(epoch);

        // Same timing scenario as T2a: leader activates fast,
        // followers slow. The only difference is `use_pre_registration:
        // true`, which in the harness invokes
        // `register_epoch_subchannels` at modeled DKG completion -
        // exactly the function the production fix calls in
        // stack.rs:1124-1190.
        let mut dkg_completion = HashMap::new();
        let mut activation = HashMap::new();
        for i in 0..3 {
            if i == leader {
                dkg_completion.insert(i, Duration::from_millis(0));
                activation.insert(i, Duration::from_millis(150));
            } else {
                // Followers' DKG completion fires BEFORE the leader's
                // activation (modeling the production fix's
                // pre-register-at-DKG-completion guarantee). Their
                // activation lags.
                dkg_completion.insert(i, Duration::from_millis(100));
                activation.insert(i, Duration::from_millis(500));
            }
        }

        let outcome = harness
            .run_cycle(
                epoch,
                outbe_consensus::test_harness::CycleOptions {
                    use_pre_registration: true,
                    dkg_completion_delay_per_node: dkg_completion,
                    activation_delay_per_node: activation,
                    leader_timeout: Duration::from_millis(500),
                    run_for: Duration::from_millis(2_000),
                },
            )
            .await;

        assert!(
            outcome.all_finalized_view_one(),
            "T2b: every node must finalize view 1 once next-epoch \
             sub-channels are pre-registered; outcome={:?}",
            outcome.view_finalized_per_node
        );
    });
}

#[test]
fn repeated_dkg_cycles_no_stall() {
    use commonware_consensus::types::{Epoch, View};
    use commonware_runtime::{deterministic, Runner};
    use std::collections::HashMap;
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(60));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        for raw_epoch in 2u64..=6 {
            let epoch = Epoch::new(raw_epoch);
            let leader = harness.leader_for_view_one(epoch);
            let mut dkg_completion = HashMap::new();
            let mut activation = HashMap::new();
            for i in 0..3 {
                if i == leader {
                    dkg_completion.insert(i, Duration::from_millis(0));
                    activation.insert(i, Duration::from_millis(80));
                } else {
                    dkg_completion.insert(i, Duration::from_millis(30));
                    activation.insert(i, Duration::from_millis(100));
                }
            }
            let outcome = harness
                .run_cycle(
                    epoch,
                    outbe_consensus::test_harness::CycleOptions {
                        use_pre_registration: true,
                        dkg_completion_delay_per_node: dkg_completion,
                        activation_delay_per_node: activation,
                        leader_timeout: Duration::from_millis(500),
                        run_for: Duration::from_millis(3_000),
                    },
                )
                .await;
            let finalized_view_three = outcome
                .finalized_view_per_node
                .iter()
                .all(|view| *view >= View::new(3));
            assert!(
                finalized_view_three,
                "T5 cycle {raw_epoch}: every node must finalize at least view 3; \
                 outcome={:?}",
                outcome.finalized_view_per_node
            );
        }
    });
}
