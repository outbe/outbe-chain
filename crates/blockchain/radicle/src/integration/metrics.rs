//! Outbe metric taxonomy: see `crates/runtime/src/metrics_recorder.rs`.
//!
//! Rust names use `outbe_radicle_*`; Reth exports them as `reth_outbe_radicle_*`.
//! Counters end in `_total`; histograms end in their measurement unit (`_ms`
//! for millisecond latencies). Gauges never end in `_total` or `_count`; use
//! units, plural quantities, or boolean predicates such as `_known`/`_allowed`.
//! Boolean gauges emit only 0.0 or 1.0, never sentinel values.
//! Shared P2P counters use `outbe_p2p_*` with `channel="radicle"`.
//! Standard labels are `addr`, `signer`, `reason`, `kind`, `result`, and `channel`.

use super::{RadicleStatusSnapshot, RadicleVotingGate};
use commonware_p2p::{CheckedSender as _, LimitedSender, Recipients};

/// Records a send attempt denied by the P2P rate limiter.
pub fn record_p2p_send_rate_limited() {
    metrics::counter!("outbe_p2p_send_rate_limited_total", "channel" => "radicle").increment(1);
}

/// Records a checked send with no retained peer.
pub fn record_p2p_send_skipped_peer() {
    metrics::counter!("outbe_p2p_send_skipped_peers_total", "channel" => "radicle").increment(1);
}

/// Records a send whose local submission was not accepted.
pub fn record_p2p_send_rejected() {
    metrics::counter!("outbe_p2p_send_rejected_total", "channel" => "radicle").increment(1);
}

pub(super) fn send_p2p_observed<S: LimitedSender>(
    sender: &mut S,
    public_key: S::PublicKey,
    bytes: Vec<u8>,
) -> bool {
    let checked = match sender.check(Recipients::One(public_key)) {
        Ok(checked) => checked,
        Err(_) => {
            record_p2p_send_rate_limited();
            return false;
        }
    };
    if checked.recipients().is_empty() {
        record_p2p_send_skipped_peer();
    }
    let feedback = checked.send(bytes, false);
    if !feedback.accepted() {
        record_p2p_send_rejected();
    }
    feedback.accepted()
}

#[derive(Default)]
pub struct RadicleMetrics {
    previous: FailureTotals,
}

#[derive(Clone, Copy, Default)]
struct FailureTotals {
    finality_regressions: u64,
    finality_conflicts: u64,
    provider_failures: u64,
    endpoint_failures: u64,
    uds_failures: u64,
    tcp_status_failures: u64,
}

impl RadicleMetrics {
    pub fn record(&mut self, status: &RadicleStatusSnapshot) {
        let manager = &status.manager;
        let phase = match manager.phase {
            crate::manager::ManagerPhase::Disabled => 0.0,
            crate::manager::ManagerPhase::JoiningUnbound => 1.0,
            crate::manager::ManagerPhase::Ready => 2.0,
            crate::manager::ManagerPhase::RuntimeDegraded => 3.0,
        };
        metrics::gauge!("outbe_radicle_phase").set(phase);
        set_finality(
            "outbe_radicle_last_seen_finalized_known",
            "outbe_radicle_last_seen_finalized_height",
            manager.last_seen_finalized.map(|block| block.number),
        );
        set_finality(
            "outbe_radicle_last_converged_finalized_known",
            "outbe_radicle_last_converged_finalized_height",
            manager.last_converged_finalized.map(|block| block.number),
        );
        metrics::gauge!("outbe_radicle_desired_peers")
            .set((manager.resolved_peer_count + manager.unresolved_peer_count) as f64);
        metrics::gauge!("outbe_radicle_resolved_peers").set(manager.resolved_peer_count as f64);
        metrics::gauge!("outbe_radicle_unresolved_peers").set(manager.unresolved_peer_count as f64);
        metrics::gauge!("outbe_radicle_connected_peers").set(manager.connected_peer_count as f64);
        metrics::gauge!("outbe_radicle_desired_repositories")
            .set(manager.desired_repository_count as f64);
        // Observed/available/pending describe the current reconcile window, while
        // `RadicleStatusSnapshot::repositories` retains last-known per-repo states.
        metrics::gauge!("outbe_radicle_observed_repositories")
            .set(manager.observed_repository_count as f64);
        metrics::gauge!("outbe_radicle_available_repositories")
            .set(manager.available_repository_count as f64);
        metrics::gauge!("outbe_radicle_pending_repositories")
            .set(manager.pending_repository_count as f64);
        if let Some(node_id) = status.local_node_id {
            metrics::gauge!("outbe_radicle_identity", "node_id" => hex::encode(node_id)).set(1.0);
        }

        let current = FailureTotals {
            finality_regressions: manager.finality_regressions,
            finality_conflicts: manager.finality_conflicts,
            provider_failures: manager.provider_failures,
            endpoint_failures: manager.endpoint_failures,
            uds_failures: manager.uds_failures,
            tcp_status_failures: manager.tcp_status_failures,
        };
        increment(
            "outbe_radicle_finality_regressions_total",
            current.finality_regressions,
            self.previous.finality_regressions,
        );
        increment(
            "outbe_radicle_finality_conflicts_total",
            current.finality_conflicts,
            self.previous.finality_conflicts,
        );
        increment(
            "outbe_radicle_provider_failures_total",
            current.provider_failures,
            self.previous.provider_failures,
        );
        increment(
            "outbe_radicle_endpoint_failures_total",
            current.endpoint_failures,
            self.previous.endpoint_failures,
        );
        increment(
            "outbe_radicle_uds_failures_total",
            current.uds_failures,
            self.previous.uds_failures,
        );
        increment(
            "outbe_radicle_tcp_status_failures_total",
            current.tcp_status_failures,
            self.previous.tcp_status_failures,
        );
        self.previous = current;

        let signer_allowed = match status.voting_gate {
            RadicleVotingGate::SignerAllowed => 1.0,
            RadicleVotingGate::Verifier | RadicleVotingGate::Fatal(_) => 0.0,
        };
        metrics::gauge!("outbe_radicle_voting_gate_signer_allowed").set(signer_allowed);
    }
}

fn set_finality(known: &'static str, height: &'static str, value: Option<u64>) {
    metrics::gauge!(known).set(f64::from(value.is_some()));
    metrics::gauge!(height).set(value.unwrap_or_default() as f64);
}

fn increment(name: &'static str, current: u64, previous: u64) {
    if current >= previous {
        metrics::counter!(name).increment(current - previous);
    }
}

#[cfg(test)]
mod send_tests {
    use super::*;
    use crate::integration::network::shutdown_tests::NoSend;
    use commonware_actor::{Feedback, Unreliable};
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_p2p::CheckedSender;
    use commonware_runtime::IoBufs;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use std::{collections::BTreeMap, time::SystemTime};

    #[derive(Clone)]
    struct TestSender {
        peer: Option<bls12381::PublicKey>,
        feedback: Unreliable<Feedback>,
        sends: usize,
    }

    impl LimitedSender for TestSender {
        type PublicKey = bls12381::PublicKey;
        type Checked<'a> = &'a mut Self;

        fn check(
            &mut self,
            recipients: Recipients<Self::PublicKey>,
        ) -> Result<Self::Checked<'_>, SystemTime> {
            assert!(matches!(recipients, Recipients::One(_)));
            Ok(self)
        }
    }

    impl CheckedSender for &mut TestSender {
        type PublicKey = bls12381::PublicKey;

        fn recipients(&self) -> Vec<Self::PublicKey> {
            self.peer.iter().cloned().collect()
        }

        fn send(self, _: impl Into<IoBufs> + Send, priority: bool) -> Unreliable<Feedback> {
            assert!(!priority);
            self.sends += 1;
            self.feedback
        }
    }

    fn assert_metrics(action: impl FnOnce(), counts: [u64; 3]) {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, action);
        let actual: BTreeMap<_, _> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(key, _, _, value)| {
                let labels: Vec<_> = key.key().labels().collect();
                assert_eq!(labels, vec![&metrics::Label::new("channel", "radicle")]);
                (key.key().name().to_owned(), value)
            })
            .collect();
        let expected = [
            "outbe_p2p_send_rate_limited_total",
            "outbe_p2p_send_skipped_peers_total",
            "outbe_p2p_send_rejected_total",
        ]
        .into_iter()
        .zip(counts)
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| (name.to_owned(), DebugValue::Counter(count)))
        .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn check_rate_limited_records_failure_without_sending() {
        let key = bls12381::PrivateKey::from_seed(1).public_key();
        assert_metrics(
            || assert!(!send_p2p_observed(&mut NoSend, key, vec![1])),
            [1, 0, 0],
        );
    }

    #[test]
    fn checked_send_records_skips_and_rejections_without_changing_feedback() {
        let key = bls12381::PrivateKey::from_seed(1).public_key();
        for (empty, feedback, counts) in [
            (true, Unreliable::Rejected, [0, 1, 1]),
            (true, Unreliable::Outcome(Feedback::Ok), [0, 1, 0]),
            (false, Unreliable::Rejected, [0, 0, 1]),
            (false, Unreliable::Outcome(Feedback::Closed), [0, 0, 1]),
            (false, Unreliable::Outcome(Feedback::Ok), [0, 0, 0]),
            (false, Unreliable::Outcome(Feedback::Backoff), [0, 0, 0]),
        ] {
            let mut sender = TestSender {
                peer: (!empty).then(|| key.clone()),
                feedback,
                sends: 0,
            };
            assert_metrics(
                || {
                    assert_eq!(
                        send_p2p_observed(&mut sender, key.clone(), vec![1]),
                        feedback.accepted()
                    )
                },
                counts,
            );
            assert_eq!(sender.sends, 1);
        }
    }
}
