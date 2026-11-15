//! Block proposal telemetry.
use metrics::{counter, histogram};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalOutcomeLabel {
    Proposed,
    Cancelled,
    PaceCancelled,
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
    ExecutionUnavailable,
    RoundAlreadyProposed,
    Failed,
}

impl ProposalOutcomeLabel {
    fn as_label(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Cancelled => "cancelled",
            Self::PaceCancelled => "pace_cancelled",
            Self::ParentProofUnavailable => "parent_proof_unavailable",
            Self::EpochStale => "epoch_stale",
            Self::BoundaryUnavailable => "boundary_unavailable",
            Self::ExecutionUnavailable => "execution_unavailable",
            Self::RoundAlreadyProposed => "round_already_proposed",
            Self::Failed => "failed",
        }
    }
}

pub fn record_proposal_outcome(outcome: ProposalOutcomeLabel) {
    counter!("outbe_consensus_proposal_outcomes_total", "result" => outcome.as_label())
        .increment(1);
}

pub fn record_block_build_resolve_pad_ms(duration: Duration) {
    histogram!("outbe_consensus_block_build_resolve_pad_ms")
        .record(duration.as_secs_f64() * 1000.0);
}

pub fn record_parent_ahead_of_local_clock() {
    counter!("outbe_consensus_parent_ahead_of_local_clock_total").increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    #[test]
    fn proposal_outcomes_have_exactly_the_closed_result_labels() {
        for (outcome, label) in [
            (ProposalOutcomeLabel::Proposed, "proposed"),
            (ProposalOutcomeLabel::Cancelled, "cancelled"),
            (ProposalOutcomeLabel::PaceCancelled, "pace_cancelled"),
            (
                ProposalOutcomeLabel::ParentProofUnavailable,
                "parent_proof_unavailable",
            ),
            (ProposalOutcomeLabel::EpochStale, "epoch_stale"),
            (
                ProposalOutcomeLabel::BoundaryUnavailable,
                "boundary_unavailable",
            ),
            (
                ProposalOutcomeLabel::ExecutionUnavailable,
                "execution_unavailable",
            ),
            (
                ProposalOutcomeLabel::RoundAlreadyProposed,
                "round_already_proposed",
            ),
            (ProposalOutcomeLabel::Failed, "failed"),
        ] {
            let recorder = DebuggingRecorder::new();
            metrics::with_local_recorder(&recorder, || record_proposal_outcome(outcome));
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            assert_eq!(snapshot.len(), 1);
            let (key, _, _, value) = &snapshot[0];
            assert_eq!(key.key().name(), "outbe_consensus_proposal_outcomes_total");
            let labels: Vec<_> = key.key().labels().map(|l| (l.key(), l.value())).collect();
            assert_eq!(labels, [("result", label)]);
            assert_eq!(*value, DebugValue::Counter(1));
        }
    }

    #[test]
    fn resolve_padding_and_parent_clock_signal_keep_their_units() {
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            record_block_build_resolve_pad_ms(Duration::ZERO);
            record_block_build_resolve_pad_ms(Duration::from_micros(1500));
            record_parent_ahead_of_local_clock();
        });
        let snapshot = recorder.snapshotter().snapshot().into_vec();
        assert_eq!(snapshot.len(), 2);
        for (key, _, _, value) in snapshot {
            assert_eq!(key.key().labels().count(), 0);
            match key.key().name() {
                "outbe_consensus_block_build_resolve_pad_ms" => {
                    assert_eq!(value, DebugValue::Histogram(vec![0.0.into(), 1.5.into()]));
                }
                "outbe_consensus_parent_ahead_of_local_clock_total" => {
                    assert_eq!(value, DebugValue::Counter(1));
                }
                name => panic!("unexpected metric: {name}"),
            }
        }
    }
}
