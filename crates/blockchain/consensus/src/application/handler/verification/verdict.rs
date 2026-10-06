//! Publish only a terminal decision; storage and convergence have independent owners.
use super::{ApplicationShared, VerifyRequest};
use crate::executor::ingress::VerificationOutcome;
use commonware_utils::channel::oneshot;

impl ApplicationShared {
    pub(super) fn publish_verify_verdict(
        &self,
        request: &VerifyRequest,
        response: oneshot::Sender<bool>,
        outcome: VerificationOutcome,
    ) {
        match outcome {
            VerificationOutcome::Invalid => {
                let _ = response.send(false);
            }
            VerificationOutcome::Valid
                if self.epoch_fence.check(request.context.round, 0).is_ok() =>
            {
                let _ = response.send(true);
            }
            VerificationOutcome::Valid | VerificationOutcome::Unavailable => {}
        }
    }
}
