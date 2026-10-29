use super::{
    ack_envelope, bootstrap_evidence_kind, new_delivery_tracker, BootstrapEvidenceKind,
    DeliveryTracker, DELIVERY_ACK, DELIVERY_DATA, DELIVERY_ID_LEN,
};
use alloy_primitives::B256;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_p2p::Recipients;
use outbe_primitives::tee_attestation_v1::AttestationMode;

/// Only transport ordering and time are controlled. Both exchanges and
/// delivery acknowledgements use the production implementation.
mod identity_phase_regression {
    use super::*;
    use crate::tee_bootstrap::{
        CeremonyBinding, CommonwareDkgGossip, IdentityExchange, LocalIdentityAnnouncement,
        StartupGossipTransport, DELIVERY_ACK, DKG_ENV_IDENTITY,
    };
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
                StartupGossipTransport {
                    sender: RecordingSender { peers: peers[1..].to_vec(), sent: sent.clone() },
                    receiver: OrderedReceiver(inbox.clone()),
                    clock: context,
                    allowed_remote_peers: peers[1..].iter().cloned().collect(),
                },
                scope,
            );
            let first = gossip.exchange_identities(IdentityExchange {
                local: LocalIdentityAnnouncement { bls: bls[0].clone(), enc: [0; 32], signature: Vec::new() },
                binding: CeremonyBinding { ceremony_id: B256::ZERO, round: 0, participant_set_hash: B256::ZERO },
                participant_count: 4,
            }).await.unwrap();
            assert_eq!(first.len(), 4, "preliminary exchange must complete");

            if early_signed {
                assert_early_delivery_stopped(&mut trackers[0], &peers, &signed[0], &sent);
            }
            for i in 1..4 {
                if i != 1 || !early_signed {
                    inbox.lock().unwrap().push_back((peers[i].clone(), signed[i - 1].clone().into()));
                }
            }
            let result = gossip.exchange_identities(IdentityExchange {
                local: LocalIdentityAnnouncement { bls: bls[0].clone(), enc: [0x22; 32], signature: vec![0x55; 96] },
                binding: CeremonyBinding { ceremony_id: B256::repeat_byte(0x41), round: 0, participant_set_hash: B256::repeat_byte(0x42) },
                participant_count: 4,
            }).await;
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

    #[test]
    fn identity_exchange_enforces_bijection_ignores_local_bls_and_maps_recv_sender_to_tee_bls() {
        use crate::tee_bootstrap::DKG_ENV_CEREMONY;
        use outbe_tee::tee_dkg::{DkgGossip as _, DkgWireMessage, FinalizedLog};

        deterministic::Runner::timed(Duration::from_secs(100)).start(|context| async move {
            let peers: Vec<_> = (201..205)
                .map(|seed| bls12381::PrivateKey::from_seed(seed).public_key())
                .collect();
            let bls: Vec<_> = (301..305)
                .map(|seed| {
                    bls12381::PrivateKey::from_seed(seed)
                        .public_key()
                        .encode()
                        .to_vec()
                })
                .collect();
            let scope = B256::repeat_byte(0x39);
            let mut trackers = peer_delivery_trackers(&peers, scope);

            let inbox = Arc::new(Mutex::new(VecDeque::new()));
            // Peer 1 attempts to announce local node 0's `bls[0]` (must be ignored)
            // and also unsigned `bls[2]` during Phase 1 (must not lock routing for peer 2).
            let capture_local_unsigned = announcement(&mut trackers[0], &bls[0], false);
            let spoof_peer2_unsigned = announcement(&mut trackers[0], &bls[2], false);
            let preliminary: Vec<_> = (1..4)
                .map(|i| announcement(&mut trackers[i - 1], &bls[i], false))
                .collect();
            inbox
                .lock()
                .unwrap()
                .push_back((peers[1].clone(), capture_local_unsigned.into()));
            inbox
                .lock()
                .unwrap()
                .push_back((peers[1].clone(), spoof_peer2_unsigned.into()));
            enqueue_preliminary(&inbox, &peers, &preliminary, None);

            let sent = Sent::default();
            let mut gossip = CommonwareDkgGossip::new(
                StartupGossipTransport {
                    sender: RecordingSender {
                        peers: peers[1..].to_vec(),
                        sent,
                    },
                    receiver: OrderedReceiver(inbox.clone()),
                    clock: context,
                    allowed_remote_peers: peers[1..].iter().cloned().collect(),
                },
                scope,
            );
            let first = gossip
                .exchange_identities(IdentityExchange {
                    local: LocalIdentityAnnouncement {
                        bls: bls[0].clone(),
                        enc: [0; 32],
                        signature: Vec::new(),
                    },
                    binding: CeremonyBinding {
                        ceremony_id: B256::ZERO,
                        round: 0,
                        participant_set_hash: B256::ZERO,
                    },
                    participant_count: 4,
                })
                .await
                .unwrap();
            assert_eq!(first.len(), 4);
            assert!(
                gossip.routing.is_empty() && gossip.sender_to_tee_bls.is_empty(),
                "unsigned preliminary phase must not lock routing or sender_to_tee_bls"
            );

            // Phase 2:
            // - Peer 1 sends a buffered ceremony message before its signed identity.
            // - Peer 1 attempts to claim local `bls[0]` (signed) -> ignored.
            // - Peer 1 registers its legitimate signed `bls[1]`.
            // - Peer 1 attempts to register a second identity `bls[2]` -> rejected (1-to-1 bijection).
            // - Peer 2 attempts to overwrite `bls[1]` -> rejected (1-to-1 bijection).
            // - Peers 2 and 3 register their legitimate signed `bls[2]` and `bls[3]`.
            let early_ceremony_msg = DkgWireMessage::FinalizedLog(FinalizedLog {
                dealer_bls: bls[1].clone(),
                signed_log: vec![0xaa],
            });
            let mut early_ceremony_payload = vec![DKG_ENV_CEREMONY];
            early_ceremony_payload.extend_from_slice(&early_ceremony_msg.to_bytes());
            let early_ceremony_env = trackers[0]
                .envelope(Recipients::All, early_ceremony_payload)
                .unwrap();

            let capture_local_signed = announcement(&mut trackers[0], &bls[0], true);
            let peer1_valid_signed = announcement(&mut trackers[0], &bls[1], true);
            let peer1_second_bls_signed = announcement(&mut trackers[0], &bls[2], true);
            let peer2_hijack_peer1_signed = announcement(&mut trackers[1], &bls[1], true);
            let peer2_valid_signed = announcement(&mut trackers[1], &bls[2], true);
            let peer3_valid_signed = announcement(&mut trackers[2], &bls[3], true);

            {
                let mut q = inbox.lock().unwrap();
                q.push_back((peers[1].clone(), early_ceremony_env.into()));
                q.push_back((peers[1].clone(), capture_local_signed.into()));
                q.push_back((peers[1].clone(), peer1_valid_signed.into()));
                q.push_back((peers[1].clone(), peer1_second_bls_signed.into()));
                q.push_back((peers[2].clone(), peer2_hijack_peer1_signed.into()));
                q.push_back((peers[2].clone(), peer2_valid_signed.into()));
                q.push_back((peers[3].clone(), peer3_valid_signed.into()));
            }

            let identities = gossip
                .exchange_identities(IdentityExchange {
                    local: LocalIdentityAnnouncement {
                        bls: bls[0].clone(),
                        enc: [0x11; 32],
                        signature: vec![0x44; 96],
                    },
                    binding: CeremonyBinding {
                        ceremony_id: B256::repeat_byte(0x41),
                        round: 0,
                        participant_set_hash: B256::repeat_byte(0x42),
                    },
                    participant_count: 4,
                })
                .await
                .unwrap();
            assert_eq!(identities.len(), 4);
            assert_eq!(gossip.routing.get(&bls[1]), Some(&peers[1]));
            assert_eq!(gossip.routing.get(&bls[2]), Some(&peers[2]));
            assert_eq!(gossip.routing.get(&bls[3]), Some(&peers[3]));
            assert!(!gossip.routing.contains_key(&bls[0]));

            // Buffered ceremony message from peers[1] resolves to bls[1], not peers[1].encode()!
            let (from_buffered, msg_buffered) = gossip.recv().await.unwrap();
            assert_eq!(from_buffered, bls[1]);
            assert_eq!(msg_buffered, early_ceremony_msg);
        });
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

fn valid_delivery_fixture() -> (DeliveryTracker, Vec<u8>, B256) {
    let (_, _, mut tracker) = known_peer_delivery(0x44, 106, 107);
    let payload = b"delivery-parser";
    let envelope = tracker.envelope(Recipients::All, payload.to_vec()).unwrap();
    let (tag, id, decoded) = tracker.decode(&envelope).unwrap();
    assert_eq!(tag, DELIVERY_DATA);
    assert_eq!(decoded, payload);
    (tracker, envelope, id)
}

#[test]
fn delivery_decode_rejects_a_short_input() {
    let (tracker, envelope, _) = valid_delivery_fixture();
    assert!(envelope.len() > DELIVERY_ID_LEN);
    assert!(tracker.decode(&envelope[..DELIVERY_ID_LEN]).is_none());
}

#[test]
fn delivery_decode_rejects_a_bad_tag() {
    let (tracker, mut envelope, _) = valid_delivery_fixture();
    envelope[0] = 0x00;
    assert!(tracker.decode(&envelope).is_none());
}

#[test]
fn delivery_decode_rejects_a_changed_message_id() {
    let (tracker, envelope, _) = valid_delivery_fixture();
    let mut changed = envelope.clone();
    changed[1] ^= 0x01;
    let id_end = 1 + DELIVERY_ID_LEN;
    assert_eq!(&changed[id_end..], &envelope[id_end..]);
    assert!(tracker.decode(&changed).is_none());
}

#[test]
fn delivery_decode_rejects_a_nonempty_ack() {
    let (tracker, _, id) = valid_delivery_fixture();
    let ack = ack_envelope(id);
    let (tag, ack_id, payload) = tracker.decode(&ack).unwrap();
    assert_eq!(tag, DELIVERY_ACK);
    assert_eq!(ack_id, id);
    assert!(payload.is_empty());
    let mut nonempty = ack;
    nonempty.push(0x01);
    assert!(tracker.decode(&nonempty).is_none());
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
