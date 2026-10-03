use super::{
    bootstrap_evidence_kind, new_delivery_tracker, BootstrapEvidenceKind, DeliveryTracker,
};
use alloy_primitives::B256;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_p2p::Recipients;
use outbe_primitives::tee_attestation_v1::AttestationMode;

/// Only transport ordering and time are controlled. Both exchanges and
/// delivery acknowledgements use the production implementation.
mod identity_phase_regression {
    use super::*;
    use crate::tee_bootstrap::{CommonwareDkgGossip, DELIVERY_ACK, DKG_ENV_IDENTITY};
    use commonware_actor::{Feedback, Unreliable};
    use commonware_codec::Encode as _;
    use commonware_p2p::{CheckedSender, LimitedSender, Message, Receiver};
    use commonware_runtime::{deterministic, IoBufs, Runner as _};
    use std::{
        collections::VecDeque,
        convert::Infallible,
        sync::{Arc, Mutex},
        time::{Duration, SystemTime},
    };

    type Sent = Arc<Mutex<Vec<(Vec<bls12381::PublicKey>, Vec<u8>)>>>;
    type Inbox = Arc<Mutex<VecDeque<Message<bls12381::PublicKey>>>>;

    #[derive(Clone)]
    struct RecordingSender {
        peers: Vec<bls12381::PublicKey>,
        sent: Sent,
    }

    struct RecordingCheckedSender {
        peers: Vec<bls12381::PublicKey>,
        sent: Sent,
    }

    impl CheckedSender for RecordingCheckedSender {
        type PublicKey = bls12381::PublicKey;

        fn recipients(&self) -> Vec<Self::PublicKey> {
            self.peers.clone()
        }

        fn send(self, message: impl Into<IoBufs> + Send, _: bool) -> Unreliable<Feedback> {
            self.sent
                .lock()
                .unwrap()
                .push((self.peers, message.into().coalesce().as_ref().to_vec()));
            Unreliable::Outcome(Feedback::Ok)
        }
    }

    impl LimitedSender for RecordingSender {
        type PublicKey = bls12381::PublicKey;
        type Checked<'a> = RecordingCheckedSender;

        fn check(
            &mut self,
            recipients: Recipients<Self::PublicKey>,
        ) -> Result<Self::Checked<'_>, SystemTime> {
            let peers = match recipients {
                Recipients::All => self.peers.clone(),
                Recipients::Some(peers) => peers,
                Recipients::One(peer) => vec![peer],
            };
            Ok(RecordingCheckedSender {
                peers,
                sent: self.sent.clone(),
            })
        }
    }

    #[derive(Debug)]
    struct OrderedReceiver(Inbox);

    impl Receiver for OrderedReceiver {
        type Error = Infallible;
        type PublicKey = bls12381::PublicKey;

        async fn recv(&mut self) -> Result<Message<Self::PublicKey>, Self::Error> {
            let message = self.0.lock().unwrap().pop_front();
            match message {
                Some(message) => Ok(message),
                // Keep the channel open: the production identity timeout,
                // not a closed test channel, must expose missing delivery.
                None => std::future::pending().await,
            }
        }
    }

    fn announcement(tracker: &mut DeliveryTracker, bls: &[u8], signed: bool) -> Vec<u8> {
        // Signature contents are opaque at this transport seam. Enclave
        // signature verification is deliberately not claimed by this test.
        let sig = if signed { vec![0x55; 96] } else { Vec::new() };
        let mut payload = vec![DKG_ENV_IDENTITY];
        payload.extend_from_slice(&(bls.len() as u32).to_be_bytes());
        payload.extend_from_slice(bls);
        payload.extend_from_slice(&[if signed { 0x22 } else { 0 }; 32]);
        payload.extend_from_slice(&(sig.len() as u32).to_be_bytes());
        payload.extend_from_slice(&sig);
        tracker.envelope(Recipients::All, payload).unwrap()
    }

    fn peer_delivery_trackers(peers: &[bls12381::PublicKey], scope: B256) -> Vec<DeliveryTracker> {
        (1..4)
            .map(|sender| {
                new_delivery_tracker(
                    scope,
                    peers
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != sender)
                        .map(|(_, peer)| peer.clone())
                        .collect(),
                )
            })
            .collect()
    }

    fn assert_early_delivery_stopped(
        tracker: &mut DeliveryTracker,
        peers: &[bls12381::PublicKey],
        signed: &[u8],
        sent: &Sent,
    ) {
        let (_, signed_id, _) = tracker.decode(signed).unwrap();
        let ack = sent
            .lock()
            .unwrap()
            .iter()
            .find_map(|(to, bytes)| {
                let (tag, id, _) = tracker.decode(bytes)?;
                (to == &vec![peers[1].clone()] && tag == DELIVERY_ACK && id == signed_id)
                    .then_some(id)
            })
            .expect("A must have ACKed B's signed announcement during preliminary exchange");
        // The other recipients have also acknowledged B. Feeding
        // A's actual ACK makes production delivery stop retrying B.
        for peer in [&peers[2], &peers[3], &peers[0]] {
            assert!(tracker.observe_peer(peer.clone()));
            tracker.acknowledge(peer, ack);
        }
        for _ in 0..=120 {
            assert!(
                tracker
                    .retry_batch()
                    .iter()
                    .all(|(_, bytes)| bytes.as_slice() != signed),
                "an ACKed announcement must not be retransmitted"
            );
        }
    }

    fn exchange_with_order(early_signed: bool) {
        deterministic::Runner::timed(Duration::from_secs(100)).start(|context| async move {
            let peers: Vec<_> = (201..205).map(|seed| bls12381::PrivateKey::from_seed(seed).public_key()).collect();
            let bls: Vec<_> = (301..305).map(|seed| bls12381::PrivateKey::from_seed(seed).public_key().encode().to_vec()).collect();
            let scope = B256::repeat_byte(0x31);
            let mut trackers = peer_delivery_trackers(&peers, scope);
            let preliminary: Vec<_> = (1..4).map(|i| announcement(&mut trackers[i - 1], &bls[i], false)).collect();
            let signed: Vec<_> = (1..4).map(|i| announcement(&mut trackers[i - 1], &bls[i], true)).collect();
            let inbox = Arc::new(Mutex::new(VecDeque::new()));
            // A receives B's signed announcement while still waiting for
            // C and D's preliminary identities. No packet is dropped.
            enqueue_preliminary(&inbox, &peers, &preliminary, early_signed.then_some(&signed[0]));
            let sent = Sent::default();
            let mut gossip = CommonwareDkgGossip::new(
                RecordingSender { peers: peers[1..].to_vec(), sent: sent.clone() },
                OrderedReceiver(inbox.clone()), context, scope, peers[1..].iter().cloned().collect(),
            );
            let first = gossip.exchange_identities(bls[0].clone(), [0; 32], Vec::new(), B256::ZERO, 0, B256::ZERO, 4).await.unwrap();
            assert_eq!(first.len(), 4, "preliminary exchange must complete");

            if early_signed {
                assert_early_delivery_stopped(&mut trackers[0], &peers, &signed[0], &sent);
            }
            for i in 1..4 {
                if i != 1 || !early_signed {
                    inbox.lock().unwrap().push_back((peers[i].clone(), signed[i - 1].clone().into()));
                }
            }
            let result = gossip.exchange_identities(bls[0].clone(), [0x22; 32], vec![0x55; 96], B256::repeat_byte(0x41), 0, B256::repeat_byte(0x42), 4).await;
            let identities = result.expect("all four signed identities were delivered; an ACKed early announcement must survive the phase transition");
            assert_eq!(identities.len(), 4);
            for (identity, expected_bls) in identities.iter().zip(bls.iter().collect::<std::collections::BTreeSet<_>>()) {
                assert_eq!(&identity.bls_pub, expected_bls);
                assert_eq!(identity.enc_pub, [0x22; 32]);
                assert_eq!(identity.enc_sig, vec![0x55; 96]);
            }
        });
    }

    fn enqueue_preliminary(
        inbox: &Inbox,
        peers: &[bls12381::PublicKey],
        preliminary: &[Vec<u8>],
        early_signed: Option<&Vec<u8>>,
    ) {
        for i in 1..4 {
            inbox
                .lock()
                .unwrap()
                .push_back((peers[i].clone(), preliminary[i - 1].clone().into()));
            if let Some(signed) = early_signed.filter(|_| i == 1) {
                inbox
                    .lock()
                    .unwrap()
                    .push_back((peers[i].clone(), signed.clone().into()));
            }
        }
    }

    #[test]
    fn signed_identities_arriving_in_their_phase_complete() {
        exchange_with_order(false);
    }

    #[test]
    fn early_acked_signed_identity_survives_phase_transition() {
        exchange_with_order(true);
    }
}

#[test]
fn bootstrap_evidence_depends_on_policy_not_local_session_security() {
    assert_eq!(
        bootstrap_evidence_kind(AttestationMode::DcapRequired, true).unwrap(),
        BootstrapEvidenceKind::Dcap
    );
    assert_eq!(
        bootstrap_evidence_kind(AttestationMode::GramineDirectDev, true).unwrap(),
        BootstrapEvidenceKind::GramineDirectDev
    );
    assert_eq!(
        bootstrap_evidence_kind(AttestationMode::GramineDirectDev, false).unwrap(),
        BootstrapEvidenceKind::GramineDirectDev
    );
    assert!(bootstrap_evidence_kind(AttestationMode::DcapRequired, false).is_err());
}

#[test]
fn delivery_tracker_retries_only_unacknowledged_peers() {
    let (peer_a, peer_b, mut tracker) = known_peer_delivery(0x11, 101, 102);
    let envelope = tracker
        .envelope(Recipients::All, b"registration".to_vec())
        .unwrap();
    let (_, id, _) = tracker.decode(&envelope).unwrap();

    assert_eq!(tracker.retry_batch().len(), 2);
    tracker.acknowledge(&peer_a, id);
    assert!(tracker.retry_batch().is_empty());
    let retry = tracker.retry_batch();
    assert_eq!(retry.len(), 1);
    assert!(matches!(&retry[0].0, Recipients::One(peer) if peer == &peer_b));

    tracker.acknowledge(&peer_b, id);
    assert_delivery_drained(&mut tracker);
}

#[test]
fn delivery_tracker_uses_capped_exponential_backoff() {
    let peer = bls12381::PrivateKey::from_seed(103).public_key();
    let mut tracker = new_delivery_tracker(
        B256::repeat_byte(0x22),
        [peer.clone()].into_iter().collect(),
    );
    assert!(tracker.observe_peer(peer));
    tracker
        .envelope(Recipients::All, b"signature".to_vec())
        .unwrap();

    let mut sends = Vec::new();
    for tick in 1..=31 {
        if !tracker.retry_batch().is_empty() {
            sends.push(tick);
        }
    }
    assert_eq!(sends, vec![1, 3, 7, 15, 31]);
}

#[test]
fn delivery_tracker_merges_recipients_for_identical_payloads() {
    let (peer_a, peer_b, mut tracker) = known_peer_delivery(0x33, 104, 105);

    let first = tracker
        .envelope(Recipients::One(peer_a.clone()), b"same".to_vec())
        .unwrap();
    let second = tracker
        .envelope(Recipients::One(peer_b.clone()), b"same".to_vec())
        .unwrap();
    assert_eq!(first, second);

    let retry = tracker.retry_batch();
    assert_eq!(retry.len(), 2);
    let (_, id, _) = tracker.decode(&first).unwrap();
    tracker.acknowledge(&peer_a, id);
    assert!(!tracker.pending.is_empty());
    tracker.acknowledge(&peer_b, id);
    assert!(tracker.pending.is_empty());
}

fn known_peer_delivery(
    scope: u8,
    seed_a: u64,
    seed_b: u64,
) -> (bls12381::PublicKey, bls12381::PublicKey, DeliveryTracker) {
    let peer_a = bls12381::PrivateKey::from_seed(seed_a).public_key();
    let peer_b = bls12381::PrivateKey::from_seed(seed_b).public_key();
    let mut tracker = new_delivery_tracker(
        B256::repeat_byte(scope),
        [peer_a.clone(), peer_b.clone()].into_iter().collect(),
    );
    assert!(tracker.observe_peer(peer_a.clone()));
    assert!(tracker.observe_peer(peer_b.clone()));
    (peer_a, peer_b, tracker)
}

fn assert_delivery_drained(tracker: &mut DeliveryTracker) {
    assert!(tracker.pending.is_empty());
    assert!(tracker.retry_batch().is_empty());
}
