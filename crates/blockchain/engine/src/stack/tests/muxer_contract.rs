// =============================================================================
// T0 - Commonware Muxer drop vs backup-capture contract.
//
// The Outbe consensus stack uses `Muxer::new(...)` (no backup) for vote / cert
// / resolver / dkg sub-channels and registers a fresh sub-channel for every
// new epoch (see stack.rs:513-549, 1009-1017). If a peer sends a message on
// epoch N's sub-channel before the receiver has registered that sub-channel
// on its end, the message is dropped - there is no replay path back into the
// late registrant.
//
// These two tests pin the Muxer contract for the pinned commonware-p2p tag
// (v2026.3.0) so that any future bump to a tag with different semantics fails
// loudly rather than silently changing the boundary-race surface.
// =============================================================================

use commonware_consensus::types::Epoch;
use commonware_cryptography::ed25519::{PrivateKey as Ed25519PrivateKey, PublicKey};
use commonware_cryptography::Signer as _;
use commonware_p2p::{
    simulated::{self, Link, Network, Oracle},
    utils::mux::{Builder as _, Muxer},
    Channel, Receiver as _, Recipients, Sender as _,
};
use commonware_runtime::{
    deterministic, Clock as _, IoBuf, Quota, Runner, Spawner as _, Supervisor as _,
};
use std::{num::NonZeroU32, time::Duration};

const LINK: Link = Link {
    latency: Duration::from_millis(0),
    jitter: Duration::from_millis(0),
    success_rate: commonware_utils::Probability::new(1, 1).unwrap(),
};
const CAPACITY: usize = 4;
const TEST_QUOTA: Quota = Quota::per_second(NonZeroU32::MAX);
/// p2p::Channel namespace for these tests. Type alias = `u64`.
const PHYSICAL_CHANNEL: Channel = 0;
/// Sub-channel id used in the test, modelling an epoch sub-channel id in
/// production code.
const EPOCH_SUBCHANNEL: Channel = 42;

fn pk(seed: u64) -> PublicKey {
    Ed25519PrivateKey::from_seed(seed).public_key()
}

fn start_network(context: deterministic::Context) -> Oracle<PublicKey, deterministic::Context> {
    let (network, oracle) = Network::new(
        context.child("network"),
        simulated::Config {
            max_size: 1024 * 1024,
            disconnect_on_block: true,
            tracked_peer_sets: commonware_utils::NZUsize!(4),
            max_peers_per_set: commonware_utils::NZUsize!(32),
        },
    );
    network.start();
    oracle
}

async fn link_bidirectional(
    oracle: &mut Oracle<PublicKey, deterministic::Context>,
    a: PublicKey,
    b: PublicKey,
) {
    oracle.add_link(a.clone(), b.clone(), LINK).await.unwrap();
    oracle.add_link(b, a, LINK).await.unwrap();
}

#[test]
fn same_epoch_routes_are_reacquired_only_after_old_receivers_drop() {
    let executor = deterministic::Runner::timed(Duration::from_secs(10));
    executor.start(|context| async move {
        let oracle = start_network(context.child("network_owner"));
        let peer = pk(0);
        let control = oracle.control(peer);

        let (vote_sender, vote_receiver) = control
            .register(PHYSICAL_CHANNEL, TEST_QUOTA)
            .await
            .unwrap();
        let (vote_muxer, mut vote_mux) = Muxer::new(
            context.child("vote_mux"),
            vote_sender,
            vote_receiver,
            CAPACITY,
        );
        vote_muxer.start();

        let (cert_sender, cert_receiver) = control
            .register(PHYSICAL_CHANNEL + 1, TEST_QUOTA)
            .await
            .unwrap();
        let (cert_muxer, mut cert_mux) = Muxer::new(
            context.child("cert_mux"),
            cert_sender,
            cert_receiver,
            CAPACITY,
        );
        cert_muxer.start();

        let (res_sender, res_receiver) = control
            .register(PHYSICAL_CHANNEL + 2, TEST_QUOTA)
            .await
            .unwrap();
        let (res_muxer, mut res_mux) =
            Muxer::new(context.child("res_mux"), res_sender, res_receiver, CAPACITY);
        res_muxer.start();

        let epoch = Epoch::new(EPOCH_SUBCHANNEL);
        let old = outbe_consensus::epoch_subchannels::register_epoch_subchannels(
            epoch,
            &mut vote_mux,
            &mut cert_mux,
            &mut res_mux,
        )
        .await
        .unwrap();
        context
            .child("drop_old_epoch_receivers")
            .spawn(move |drop_context| async move {
                drop_context.sleep(Duration::from_millis(50)).await;
                drop(old);
            });

        let replacement = outbe_consensus::epoch_subchannels::reacquire_epoch_subchannels(
            epoch,
            &context,
            Duration::from_secs(1),
            Duration::from_millis(10),
            &mut vote_mux,
            &mut cert_mux,
            &mut res_mux,
        )
        .await
        .expect("same-epoch routes must become available after old receivers drop");
        assert_eq!(replacement.epoch, epoch);
    });
}

/// Without `.with_backup()`, a message sent to a sub-channel that the
/// receiver has not yet registered is dropped. Even if the receiver
/// registers later, it never observes the early message.
#[test]
fn mux_drops_messages_to_unregistered_subchannel() {
    let executor = deterministic::Runner::timed(Duration::from_secs(10));
    executor.start(|context| async move {
        // 2026.5.0: `deterministic::Context` is no longer `Clone`; pass a
        // child context to the network and keep `context` for the test body
        // (labels via `child` need `Supervisor` in scope).
        let mut oracle = start_network(context.child("network_owner"));

        let pk_sender = pk(0);
        let pk_receiver = pk(1);

        // Sender peer: register the physical channel + the epoch sub-channel.
        let (s_sender, s_receiver) = oracle
            .control(pk_sender.clone())
            .register(PHYSICAL_CHANNEL, TEST_QUOTA)
            .await
            .unwrap();
        let (s_mux, mut s_handle) =
            Muxer::new(context.child("sender_mux"), s_sender, s_receiver, CAPACITY);
        s_mux.start();

        // Receiver peer: register the physical channel only - sub-channel
        // is *not* registered yet.
        let (r_sender, r_receiver) = oracle
            .control(pk_receiver.clone())
            .register(PHYSICAL_CHANNEL, TEST_QUOTA)
            .await
            .unwrap();
        let (r_mux, mut r_handle) = Muxer::new(
            context.child("receiver_mux"),
            r_sender,
            r_receiver,
            CAPACITY,
        );
        r_mux.start();

        link_bidirectional(&mut oracle, pk_sender.clone(), pk_receiver.clone()).await;

        // Sender registers and sends a message on the epoch sub-channel
        // *before* the receiver has registered it.
        let (mut tx, _) = s_handle.register(EPOCH_SUBCHANNEL).await.unwrap();
        let payload = IoBuf::copy_from_slice(b"early-vote");
        // 2026.5.0: `Sender::send` is SYNC and returns `Vec<PublicKey>` (the
        // recipients we attempted to deliver to), not a future/Result.
        let _ = tx.send(Recipients::One(pk_receiver.clone()), payload.clone(), false);

        // Wait for the simulated network to drain the message into the
        // receiver muxer (which will drop it, since the sub-channel is
        // not registered there).
        context.sleep(Duration::from_millis(100)).await;

        // Now the receiver registers the sub-channel - too late.
        let (_, mut rx) = r_handle.register(EPOCH_SUBCHANNEL).await.unwrap();

        // Bound the wait. With LINK latency = 0 and SubReceiver mailbox
        // empty, recv() will block forever on the contract this test
        // pins; we treat any receipt within the bound as a contract break.
        let timed = context.sleep(Duration::from_millis(500));
        tokio::pin!(timed);
        tokio::select! {
            received = rx.recv() => {
                let _ = received;
                panic!(
                    "muxer contract violation: late registrant received a message that was \
                     sent before its sub-channel was registered"
                );
            }
            _ = &mut timed => {
                // Expected: timed out without receiving - message was dropped.
            }
        }
    });
}

/// With `.with_backup()`, the same early message is captured into the
/// backup receiver as `(subchannel, (peer_pk, payload))`. The late-
/// registrant of the sub-channel still does **not** see it - backup is
/// a capture surface, not an auto-replay mechanism.
#[test]
fn mux_with_backup_captures_unrouted_message_but_does_not_replay() {
    let executor = deterministic::Runner::timed(Duration::from_secs(10));
    executor.start(|context| async move {
        // 2026.5.0: `deterministic::Context` is no longer `Clone`; pass a
        // child context to the network and keep `context` for the test body.
        let mut oracle = start_network(context.child("network_owner"));

        let pk_sender = pk(0);
        let pk_receiver = pk(1);

        let (s_sender, s_receiver) = oracle
            .control(pk_sender.clone())
            .register(PHYSICAL_CHANNEL, TEST_QUOTA)
            .await
            .unwrap();
        let (s_mux, mut s_handle) =
            Muxer::new(context.child("sender_mux"), s_sender, s_receiver, CAPACITY);
        s_mux.start();

        let (r_sender, r_receiver) = oracle
            .control(pk_receiver.clone())
            .register(PHYSICAL_CHANNEL, TEST_QUOTA)
            .await
            .unwrap();
        let (r_mux, mut r_handle, mut backup_rx) = Muxer::builder(
            context.child("receiver_mux"),
            r_sender,
            r_receiver,
            CAPACITY,
        )
        .with_backup()
        .build();
        r_mux.start();

        link_bidirectional(&mut oracle, pk_sender.clone(), pk_receiver.clone()).await;

        // commonware 2026.4.0: routing requires a tracked peer set, not just
        // a link. Track both peers so the Recipients::One send resolves.
        {
            use commonware_p2p::Manager as _;
            let peers = commonware_utils::ordered::Set::from_iter_dedup([
                pk_sender.clone(),
                pk_receiver.clone(),
            ]);
            // 2026.5.0: `Manager::track` is SYNC and returns `Feedback`.
            let _ = oracle.manager().track(0, peers);
        }

        let (mut tx, _) = s_handle.register(EPOCH_SUBCHANNEL).await.unwrap();
        let payload = IoBuf::copy_from_slice(b"early-vote");
        // 2026.5.0: `Sender::send` is SYNC and returns `Vec<PublicKey>`.
        let _ = tx.send(Recipients::One(pk_receiver.clone()), payload.clone(), false);

        // Drain into backup channel.
        let timed = context.sleep(Duration::from_secs(2));
        tokio::pin!(timed);
        let captured = tokio::select! {
            msg = backup_rx.recv() => msg.expect("backup recv must produce a message"),
            _ = &mut timed => {
                panic!("muxer with backup did not capture the unrouted message");
            }
        };
        let (subchannel, (from, bytes)) = captured;
        assert_eq!(subchannel, EPOCH_SUBCHANNEL);
        assert_eq!(from, pk_sender);
        // The captured payload contains the muxer's framing prefix
        // (varint sub-channel id) followed by our raw payload. We assert
        // that our payload bytes appear at the tail so we don't depend on
        // the exact framing format.
        let captured_bytes: &[u8] = bytes.as_ref();
        let expected_bytes: &[u8] = payload.as_ref();
        assert!(
            captured_bytes.ends_with(expected_bytes),
            "backup-captured bytes did not contain the original payload as suffix"
        );

        // Now register the sub-channel on the receiver - assert that the
        // late registrant does **not** receive the message that was
        // already drained into backup.
        let (_, mut rx) = r_handle.register(EPOCH_SUBCHANNEL).await.unwrap();
        let timed_late = context.sleep(Duration::from_millis(500));
        tokio::pin!(timed_late);
        tokio::select! {
            received = rx.recv() => {
                let _ = received;
                panic!(
                    "muxer with backup auto-replayed into the late registrant; this is \
                     not the v2026.3.0 contract - production fix design must change"
                );
            }
            _ = &mut timed_late => {
                // Expected: backup captured, late registrant blank.
            }
        }
    });
}
