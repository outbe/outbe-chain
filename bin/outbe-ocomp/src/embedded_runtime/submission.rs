use super::*;

pub(super) struct VoteWork {
    pub request: EmbeddedVoteRequestV1,
    pub job_id: B256,
    pub preparer: Arc<LocalVoteTransactionPreparerV1>,
    pub submission_gate: Arc<ValidatorOcompSubmissionGateV1>,
    pub rpc_url: String,
    pub config: VoteSubmissionConfigV1,
    pub cancelled: Arc<AtomicBool>,
    pub sender: mpsc::Sender<EmbeddedVoteOutcomeV1>,
}

impl VoteWork {
    pub(super) fn run(self) {
        let VoteWork {
            request,
            job_id,
            preparer,
            submission_gate,
            rpc_url,
            config,
            cancelled,
            sender,
        } = self;
        let task = VoteDelivery {
            request,
            job_id,
            preparer,
            cancelled,
            sender,
        };
        if task.cancelled.load(Ordering::Acquire) {
            return;
        }
        let _submission_permit = match submission_gate.acquire() {
            Ok(permit) => permit,
            Err(error) => {
                task.report_unrecoverable(error.to_string());
                return;
            }
        };
        if task.cancelled.load(Ordering::Acquire) {
            return;
        }
        let rpc = match PublicVoteRpcClientV1::new(rpc_url, RPC_MAX_RESPONSE_BYTES) {
            Ok(rpc) => rpc,
            Err(error) => {
                task.report_unrecoverable(error.to_string());
                return;
            }
        };
        let mut submitter = match SupervisorVoteSubmitterV1::open(config, rpc) {
            Ok(submitter) => submitter,
            Err(error) => {
                task.report_unrecoverable(error.to_string());
                return;
            }
        };
        task.reconcile(&mut submitter);
    }
}

struct VoteDelivery {
    request: EmbeddedVoteRequestV1,
    job_id: B256,
    preparer: Arc<LocalVoteTransactionPreparerV1>,
    cancelled: Arc<AtomicBool>,
    sender: mpsc::Sender<EmbeddedVoteOutcomeV1>,
}

impl VoteDelivery {
    fn report_unrecoverable(&self, detail: String) {
        let _ = self.sender.send(EmbeddedVoteOutcomeV1::Unrecoverable {
            job_id: self.job_id,
            generation: self.request.generation,
            detail,
        });
    }

    fn reconcile(&self, submitter: &mut SupervisorVoteSubmitterV1<PublicVoteRpcClientV1>) {
        while !self.cancelled.load(Ordering::Acquire) {
            match submitter.reconcile(
                self.preparer.as_ref(),
                self.job_id,
                self.request.result_digest,
                &self.request.canonical_result,
                &self.request.record.spec,
            ) {
                Ok(VoteSubmissionOutcomeV1::Finalized(inclusion)) => {
                    let _ = self.sender.send(EmbeddedVoteOutcomeV1::Finalized {
                        job_id: self.job_id,
                        generation: self.request.generation,
                        success: inclusion.success,
                    });
                    return;
                }
                Ok(_) => thread::sleep(RETRY_INTERVAL),
                Err(error) if error.class() == VoteSubmissionFailureClassV1::Retryable => {
                    metrics::counter!(
                        "outbe_ocomp_vote_submission_failures_total",
                        "class" => "retryable"
                    )
                    .increment(1);
                    thread::sleep(RETRY_INTERVAL);
                }
                Err(error) => {
                    metrics::counter!(
                        "outbe_ocomp_vote_submission_failures_total",
                        "class" => "unrecoverable"
                    )
                    .increment(1);
                    let _ = self.sender.send(EmbeddedVoteOutcomeV1::Unrecoverable {
                        job_id: self.job_id,
                        generation: self.request.generation,
                        detail: error.to_string(),
                    });
                    return;
                }
            }
        }
    }
}
