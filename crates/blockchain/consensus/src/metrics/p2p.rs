//! Consensus peer-to-peer telemetry.

use alloy_primitives::Bytes;
use commonware_p2p::{CheckedSender as _, LimitedSender, Recipients};
use metrics::{counter, gauge};

/// Consensus channels with stable, bounded telemetry labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum P2pChannel {
    Dkg,
    TeeDkg,
    TeeBootstrap,
}

impl P2pChannel {
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Dkg => "dkg",
            Self::TeeDkg => "tee_dkg",
            Self::TeeBootstrap => "tee_bootstrap",
        }
    }
}

/// One unchanged P2P submission and its observation context.
pub struct ObservedSend<P: commonware_cryptography::PublicKey, B> {
    pub channel: P2pChannel,
    pub recipients: Recipients<P>,
    pub expected_peers: Option<usize>,
    pub bytes: B,
    pub priority: bool,
}

/// Observe local submission without changing Commonware's accepted recipient vector.
/// Acceptance does not imply remote delivery.
pub fn send_observed<S: LimitedSender, B: Into<Bytes>>(
    sender: &mut S,
    request: ObservedSend<S::PublicKey, B>,
) -> Vec<S::PublicKey> {
    let ObservedSend {
        channel,
        recipients,
        expected_peers,
        bytes,
        priority,
    } = request;
    let Ok(checked) = sender.check(recipients) else {
        counter!("outbe_p2p_send_rate_limited_total", "channel" => channel.as_label()).increment(1);
        return Vec::new();
    };
    let retained = checked.recipients();
    if let Some(expected) = expected_peers {
        if expected > retained.len() {
            counter!("outbe_p2p_send_skipped_peers_total", "channel" => channel.as_label())
                .increment((expected - retained.len()) as u64);
        }
    }
    let bytes: Bytes = bytes.into();
    if checked.send(bytes.0, priority).accepted() {
        retained
    } else {
        counter!("outbe_p2p_send_rejected_total", "channel" => channel.as_label()).increment(1);
        Vec::new()
    }
}

/// Current process-local TEE bootstrap delivery backlog.
pub fn record_tee_bootstrap_pending(messages: usize, bytes: usize) {
    gauge!("outbe_p2p_tee_bootstrap_pending_messages").set(messages as f64);
    gauge!("outbe_p2p_tee_bootstrap_pending_bytes").set(bytes as f64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_actor::{Feedback, Unreliable};
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_p2p::{CheckedSender, Sender};
    use commonware_runtime::IoBufs;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use std::{
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    type Sent = Arc<Mutex<Vec<(Vec<u8>, bool)>>>;

    #[derive(Clone)]
    struct MockSender {
        retained: Vec<bls12381::PublicKey>,
        feedback: Option<Unreliable<Feedback>>,
        sent: Sent,
    }

    impl LimitedSender for MockSender {
        type PublicKey = bls12381::PublicKey;
        type Checked<'a> = Self;

        fn check(&mut self, recipients: Recipients<Self::PublicKey>) -> Result<Self, SystemTime> {
            assert!(matches!(recipients, Recipients::All));
            self.feedback.ok_or(SystemTime::UNIX_EPOCH)?;
            Ok(self.clone())
        }
    }

    impl CheckedSender for MockSender {
        type PublicKey = bls12381::PublicKey;

        fn recipients(&self) -> Vec<Self::PublicKey> {
            self.retained.clone()
        }

        fn send(self, message: impl Into<IoBufs> + Send, priority: bool) -> Unreliable<Feedback> {
            self.sent
                .lock()
                .unwrap()
                .push((message.into().coalesce().as_ref().to_vec(), priority));
            self.feedback.unwrap()
        }
    }

    const CHANNELS: [P2pChannel; 3] = [
        P2pChannel::Dkg,
        P2pChannel::TeeDkg,
        P2pChannel::TeeBootstrap,
    ];

    fn assert_send_case(
        channel: P2pChannel,
        feedback: Option<Unreliable<Feedback>>,
        expected: Option<usize>,
        counters: &[(&str, u64)],
    ) {
        let peers: Vec<_> = [602, 601, 602]
            .into_iter()
            .map(|seed| bls12381::PrivateKey::from_seed(seed).public_key())
            .collect();
        let mut observed = MockSender {
            retained: peers.clone(),
            feedback,
            sent: Sent::default(),
        };
        let mut baseline = MockSender {
            sent: Sent::default(),
            ..observed.clone()
        };
        let bytes = Bytes::from_static(b"p2p observation");
        let accepted = Sender::send(&mut baseline, Recipients::All, bytes.0.clone(), true);
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let actual = metrics::with_local_recorder(&recorder, || {
            send_observed(
                &mut observed,
                ObservedSend {
                    channel,
                    recipients: Recipients::All,
                    expected_peers: expected,
                    bytes: bytes.clone(),
                    priority: true,
                },
            )
        });
        assert_eq!(actual, accepted);
        let expected_return = if feedback.is_some_and(Unreliable::accepted) {
            peers
        } else {
            vec![]
        };
        assert_eq!(actual, expected_return);
        assert_eq!(
            *observed.sent.lock().unwrap(),
            *baseline.sent.lock().unwrap()
        );
        let expected_sends = if feedback.is_some() {
            vec![(bytes.to_vec(), true)]
        } else {
            vec![]
        };
        assert_eq!(*observed.sent.lock().unwrap(), expected_sends);
        let snapshot = snapshotter.snapshot().into_vec();
        assert_eq!(snapshot.len(), counters.len());
        for (name, count) in counters {
            let (key, _, _, value) = snapshot
                .iter()
                .find(|(key, _, _, _)| key.key().name() == *name)
                .expect("send counter emitted");
            assert_eq!(
                key.key().labels().collect::<Vec<_>>(),
                vec![&metrics::Label::new("channel", channel.as_label())]
            );
            assert_eq!(*value, DebugValue::Counter(*count));
        }
    }

    #[test]
    fn rate_limited_sends_match_sender_and_count_each_channel() {
        for channel in CHANNELS {
            assert_send_case(
                channel,
                None,
                Some(5),
                &[("outbe_p2p_send_rate_limited_total", 1)],
            );
        }
    }

    #[test]
    fn accepted_sends_preserve_retained_vector_and_count_skipped_peers() {
        for (channel, label) in CHANNELS
            .into_iter()
            .zip(["dkg", "tee_dkg", "tee_bootstrap"])
        {
            assert_eq!(channel.as_label(), label);
            for feedback in [Feedback::Ok, Feedback::Backoff] {
                let feedback = Some(Unreliable::Outcome(feedback));
                assert_send_case(
                    channel,
                    feedback,
                    Some(5),
                    &[("outbe_p2p_send_skipped_peers_total", 2)],
                );
                assert_send_case(channel, feedback, Some(2), &[]);
                assert_send_case(channel, feedback, None, &[]);
            }
        }
    }

    #[test]
    fn rejected_and_closed_sends_match_sender_and_count_each_channel() {
        for channel in CHANNELS {
            for feedback in [Unreliable::Rejected, Unreliable::Outcome(Feedback::Closed)] {
                assert_send_case(
                    channel,
                    Some(feedback),
                    Some(5),
                    &[
                        ("outbe_p2p_send_skipped_peers_total", 2),
                        ("outbe_p2p_send_rejected_total", 1),
                    ],
                );
            }
        }
    }
}
