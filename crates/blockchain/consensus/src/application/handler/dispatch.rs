use super::*;
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
        let ProposalReply {
            response,
            propose_start,
            outcome,
        } = reply;
        match outcome {
            Ok(ProposeOutcome::Proposed(digest)) => {
                // Proposer-side liveness pacing only: hold the already-sealed
                // digest until the min-block-time floor elapses, then hand it
                // to Simplex (or abort if the view is cancelled first). Never
                // touches block bytes/hash/validation.
                pace_and_send(clock, response, digest, self.min_block_time, propose_start).await;
            }
            Ok(ProposeOutcome::ParentProofUnavailable) => {
                debug!("proposal task completed without response: exact parent proof unavailable");
            }
            Ok(ProposeOutcome::EpochStale) => {
                debug!("proposal task completed without response for stale epoch work");
            }
            Ok(ProposeOutcome::BoundaryUnavailable) => {
                debug!("proposal task completed without response: DKG boundary requirement unavailable");
            }
            Ok(ProposeOutcome::ProjectionUnavailable) => {
                debug!("proposal task completed without response: exact parent is not projected");
            }
            Ok(ProposeOutcome::ExecutionUnavailable) => {
                debug!(
                    "proposal task completed without response: candidate execution is not valid"
                );
            }
            Err(error) => self.report_proposal_failure(error),
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
        match self
            .handle_verify(
                clock,
                verify.context,
                verify.payload,
                response,
                execution_read_budget,
            )
            .await
        {
            Ok(()) => {}
            Err(error) => {
                info!(
                    %error,
                    "could not decide proposal validity; dropping verify response channel"
                );
            }
        }
    }
}
fn record_proposal_cancellation(
    execution_read_budget: &ExecutionReadBudget,
    payload_trace: &ProposalPayloadTrace,
) {
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
