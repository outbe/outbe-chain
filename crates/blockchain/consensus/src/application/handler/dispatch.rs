use super::timing::ProposalPacing;
use super::*;
use crate::application::handler::verification::VerifyTask;
use crate::metrics::propose::{record_proposal_outcome, ProposalOutcomeLabel};
use commonware_utils::channel::oneshot;

struct ProposalReply {
    response: oneshot::Sender<Digest>,
    propose_start: std::time::SystemTime,
    outcome: eyre::Result<ProposeOutcome>,
}
impl ApplicationHandler {
    pub(super) fn dispatch_message<E>(&self, context: &E, message: Message)
    where
        E: commonware_runtime::Metrics
            + commonware_runtime::Spawner
            + commonware_runtime::Clock
            + Send
            + Sync
            + 'static,
    {
        match message {
            Message::Genesis(genesis) => {
                context.child("genesis").spawn({
                    let shared = self.shared.clone();
                    move |ctx| async move {
                        shared.handle_genesis(&ctx, genesis).await;
                    }
                });
            }
            Message::Propose(propose) => {
                context.child("propose").spawn({
                    let shared = self.shared.clone();
                    move |ctx| async move {
                        shared.run_propose_message(&ctx, *propose).await;
                    }
                });
            }
            Message::Verify(verify) => {
                context.child("verify").spawn({
                    let shared = self.shared.clone();
                    move |ctx| async move {
                        shared.run_verify_message(&ctx, *verify).await;
                    }
                });
            }
        }
    }
}
impl ApplicationShared {
    async fn run_propose_message(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        propose: ingress::Propose,
    ) {
        let mut response = propose.response;
        let execution_read_budget = ExecutionReadBudget::new();
        let payload_trace = ProposalPayloadTrace::default();
        // Task start covers the whole build and marshal path for proposer pacing.
        let propose_start = clock.current();
        let handle = Box::pin(self.handle_propose(
            clock,
            ProposalRequest {
                context: propose.context,
                propose_start,
                execution_read_budget: execution_read_budget.clone(),
                payload_trace: payload_trace.clone(),
            },
        ));
        let cancelled = Box::pin(response.closed());
        let outcome = match futures::future::select(handle, cancelled).await {
            futures::future::Either::Left((outcome, _)) => outcome,
            futures::future::Either::Right(((), _)) => {
                record_proposal_cancellation(&execution_read_budget, &payload_trace);
                return;
            }
        };
        self.complete_propose_message(
            clock,
            ProposalReply {
                response,
                propose_start,
                outcome,
            },
        )
        .await;
    }
    async fn complete_propose_message(
        &self,
        clock: &impl commonware_runtime::Clock,
        reply: ProposalReply,
    ) {
        if let Err(error) = complete_proposal(clock, reply, self.min_block_time).await {
            self.report_proposal_failure(error);
        }
    }

    fn report_proposal_failure(&self, error: eyre::Report) {
        if let Some(suppressed_since_last) = self.proposal_failure_log_limiter.check() {
            tracing::error!(
                %error,
                suppressed_since_last,
                "critical proposal failure; stopping proposal task"
            );
        }
    }
    async fn run_verify_message(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        verify: ingress::Verify,
    ) {
        let response = verify.response;
        let execution_read_budget = ExecutionReadBudget::new();
        self.handle_verify(
            clock,
            VerifyTask {
                context: verify.context,
                payload_digest: verify.payload,
                response,
                execution_read_budget,
            },
        )
        .await;
    }
}
async fn complete_proposal(
    clock: &impl commonware_runtime::Clock,
    reply: ProposalReply,
    min_block_time: std::time::Duration,
) -> eyre::Result<()> {
    let ProposalReply {
        response,
        propose_start,
        outcome,
    } = reply;
    match outcome {
        Ok(ProposeOutcome::Proposed {
            digest,
            resolve_pad,
        }) => {
            // Proposer-side liveness pacing only. Hold the already-sealed
            // digest until the min-block-time floor elapses. Then give it
            // to Simplex, or abort if the view is cancelled first. This
            // never touches block bytes/hash/validation.
            let delivered = pace_and_send(
                clock,
                response,
                digest,
                ProposalPacing {
                    min_block_time,
                    propose_start,
                    resolve_pad,
                },
            )
            .await;
            record_proposal_outcome(if delivered {
                ProposalOutcomeLabel::Proposed
            } else {
                ProposalOutcomeLabel::PaceCancelled
            });
        }
        Ok(outcome) => {
            record_proposal_outcome(outcome.outcome_label());
            debug!("{}", outcome.completion_message());
        }
        Err(error) => {
            record_proposal_outcome(ProposalOutcomeLabel::Failed);
            return Err(error);
        }
    }
    Ok(())
}

fn record_proposal_cancellation(
    execution_read_budget: &ExecutionReadBudget,
    payload_trace: &ProposalPayloadTrace,
) {
    record_proposal_outcome(ProposalOutcomeLabel::Cancelled);
    execution_read_budget.cancel();
    if let Some(payload_id) = payload_trace.payload_id() {
        debug!(
            audit_schema = SCHEMA_VERSION,
            audit_event = %PROPOSAL_VIEW_CANCELLED,
            process_instance = %process_instance_id(),
            %payload_id,
            "view cancelled during proposal execution"
        );
    } else {
        debug!(
            payload_id = "unassigned",
            "view cancelled during proposal execution"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    #[test]
    fn completion_records_the_dispatched_outcome_and_actual_delivery() {
        use commonware_runtime::{Clock as _, Runner as _};
        use std::time::Duration;

        for (outcome, cancelled, label) in [
            (
                Ok(ProposeOutcome::Proposed {
                    digest: Digest::ZERO,
                    resolve_pad: Duration::ZERO,
                }),
                false,
                "proposed",
            ),
            (
                Ok(ProposeOutcome::Proposed {
                    digest: Digest::ZERO,
                    resolve_pad: Duration::ZERO,
                }),
                true,
                "pace_cancelled",
            ),
            (Ok(ProposeOutcome::EpochStale), false, "epoch_stale"),
            (Err(eyre::eyre!("proposal failed")), false, "failed"),
        ] {
            let recorder = DebuggingRecorder::new();
            metrics::with_local_recorder(&recorder, || {
                commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
                    |clock| async move {
                        let (response, receiver) = oneshot::channel();
                        let receiver = if cancelled {
                            drop(receiver);
                            None
                        } else {
                            Some(receiver)
                        };
                        let start = clock.current();
                        let result = complete_proposal(
                            &clock,
                            ProposalReply {
                                response,
                                propose_start: start,
                                outcome,
                            },
                            Duration::from_millis(100),
                        )
                        .await;
                        assert_eq!(result.is_err(), label == "failed");
                        if let Some(receiver) = receiver {
                            if label == "proposed" {
                                assert_eq!(receiver.await.unwrap(), Digest::ZERO);
                                assert_eq!(
                                    clock.current().duration_since(start).unwrap(),
                                    Duration::from_millis(100)
                                );
                            } else {
                                assert!(receiver.await.is_err());
                            }
                        } else {
                            assert_eq!(
                                clock.current().duration_since(start).unwrap(),
                                Duration::ZERO
                            );
                        }
                    },
                );
            });
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            let outcomes: Vec<_> = snapshot
                .iter()
                .filter(|(key, _, _, _)| {
                    key.key().name() == "outbe_consensus_proposal_outcomes_total"
                })
                .collect();
            assert_eq!(outcomes.len(), 1);
            let labels: Vec<_> = outcomes[0]
                .0
                .key()
                .labels()
                .map(|l| (l.key(), l.value()))
                .collect();
            assert_eq!(labels, [("result", label)]);
            assert_eq!(outcomes[0].3, DebugValue::Counter(1));
        }
    }

    #[test]
    fn cancellation_before_build_completion_records_one_outcome_and_cancels_execution() {
        let recorder = DebuggingRecorder::new();
        let budget = ExecutionReadBudget::new();
        metrics::with_local_recorder(&recorder, || {
            record_proposal_cancellation(&budget, &ProposalPayloadTrace::default());
        });
        assert!(budget.is_cancelled());
        let snapshot = recorder.snapshotter().snapshot().into_vec();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].0.key().name(),
            "outbe_consensus_proposal_outcomes_total"
        );
        let labels: Vec<_> = snapshot[0]
            .0
            .key()
            .labels()
            .map(|l| (l.key(), l.value()))
            .collect();
        assert_eq!(labels, [("result", "cancelled")]);
        assert_eq!(snapshot[0].3, DebugValue::Counter(1));
    }

    #[test]
    fn non_proposed_outcomes_map_exhaustively_to_their_metric_labels() {
        for (outcome, label) in [
            (
                ProposeOutcome::ParentProofUnavailable,
                ProposalOutcomeLabel::ParentProofUnavailable,
            ),
            (ProposeOutcome::EpochStale, ProposalOutcomeLabel::EpochStale),
            (
                ProposeOutcome::BoundaryUnavailable,
                ProposalOutcomeLabel::BoundaryUnavailable,
            ),
            (
                ProposeOutcome::ExecutionUnavailable,
                ProposalOutcomeLabel::ExecutionUnavailable,
            ),
            (
                ProposeOutcome::RoundAlreadyProposed,
                ProposalOutcomeLabel::RoundAlreadyProposed,
            ),
        ] {
            assert_eq!(outcome.outcome_label(), label);
        }
    }
}
