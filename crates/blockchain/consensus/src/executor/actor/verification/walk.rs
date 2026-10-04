use super::*;
use commonware_consensus::marshal::ancestry::BlockProvider;

pub(super) enum Step {
    Ready,
    InFlight,
    Fetching(BoxFuture<'static, Option<Arc<ConsensusBlock>>>),
    Projection(BoxFuture<'static, outbe_primitives::projection::WaitOutcome>),
    Retry(BoxFuture<'static, ()>),
    Stopped,
}

pub(super) struct Walk {
    pub(super) id: u64,
    pub(super) round: Round,
    pub(super) target: Arc<ConsensusBlock>,
    pub(super) cursor: Arc<ConsensusBlock>,
    pub(super) parent: Option<Arc<ConsensusBlock>>,
    pub(super) epoch_fence: ApplicationEpochFence,
    pub(super) budget: ExecutionReadBudget,
    pub(super) step: Step,
    pub(super) anchored: Option<Digest>,
}

impl Walk {
    pub(super) fn new(id: u64, request: VerificationRequest) -> Self {
        Self {
            id,
            round: request.round,
            cursor: request.block.clone(),
            target: request.block,
            parent: request.parent,
            epoch_fence: request.epoch_fence,
            budget: request.execution_read_budget,
            step: Step::Ready,
            anchored: None,
        }
    }

    pub(super) fn follows(&self, parent: &PendingParent, previous_round: Option<Round>) -> bool {
        self.target.digest() == parent.digest
            && self.round.epoch() == parent.round.epoch()
            && !(matches!(self.step, Step::Stopped)
                && previous_round.is_some_and(|round| parent.round > round))
    }

    pub(super) fn current(&self) -> bool {
        self.epoch_fence
            .check(self.round, self.target.number())
            .is_ok()
    }

    pub(super) fn reprobe(&mut self) {
        self.cursor = self.target.clone();
        self.step = Step::Ready;
    }

    pub(super) fn retry(&mut self, clock: &impl Clock) {
        self.cursor = self.target.clone();
        self.step = Step::Retry(
            clock
                .sleep(crate::application::handler::VERIFY_SYNCING_RETRY_DELAY)
                .boxed(),
        );
    }

    pub(super) fn fetch_parent(&mut self, marshal: Option<&MarshalMailbox>) {
        if let Some(parent) = &self.parent {
            if parent.block_hash() == self.cursor.parent_hash()
                && parent.number().checked_add(1) == Some(self.cursor.number())
            {
                self.cursor = parent.clone();
                self.step = Step::Ready;
                return;
            }
        }
        let Some(marshal) = marshal else {
            self.step = Step::Stopped;
            return;
        };
        self.step = Step::Fetching(marshal.subscribe_parent(self.cursor.as_ref()).boxed());
    }

    pub(super) fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<eyre::Result<()>> {
        use outbe_primitives::projection::WaitOutcome;
        match &mut self.step {
            Step::Projection(wait) => {
                let outcome = futures::ready!(wait.as_mut().poll(cx));
                self.step = Step::Stopped;
                match outcome {
                    WaitOutcome::Ready => self.step = Step::Ready,
                    WaitOutcome::Fatal(failure) => {
                        return Poll::Ready(Err(eyre::eyre!("projection failed: {failure:?}")))
                    }
                    _ => {}
                }
            }
            Step::Fetching(fetch) => {
                let parent = futures::ready!(fetch.as_mut().poll(cx));
                self.step = Step::Stopped;
                if let Some(parent) = parent.filter(|parent| {
                    parent.block_hash() == self.cursor.parent_hash()
                        && parent.number().checked_add(1) == Some(self.cursor.number())
                }) {
                    self.cursor = parent;
                    self.step = Step::Ready;
                }
            }
            Step::Retry(retry) => {
                futures::ready!(retry.as_mut().poll(cx));
                self.step = Step::Ready;
            }
            _ => return Poll::Pending,
        }
        Poll::Ready(Ok(()))
    }

    pub(super) fn conflicts_with(&self, finalized: (Height, Digest)) -> bool {
        let (height, digest) = finalized;
        self.target.number() < height.get()
            || (self.cursor.number() == height.get() && self.cursor.digest() != digest)
            || (self.cursor.number().checked_sub(1) == Some(height.get())
                && self.cursor.parent_hash() != digest.0)
    }

    pub(super) fn reaches(&self, finalized: (Height, Digest)) -> bool {
        let (height, digest) = finalized;
        (self.cursor.number() == height.get() && self.cursor.digest() == digest)
            || (self.cursor.number().checked_sub(1) == Some(height.get())
                && self.cursor.parent_hash() == digest.0)
    }
}
