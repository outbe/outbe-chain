use super::*;

struct ExecutionReply {
    owner: Owner,
    id: u64,
    digest: Digest,
    status: PayloadStatusEnum,
}

struct Progress<'a, C> {
    clock: &'a C,
    marshal: Option<&'a MarshalMailbox>,
    finalized: (Height, Digest),
}

impl VerificationWork {
    pub(in crate::executor::actor) fn handle_event(
        &mut self,
        event: Event,
        clock: &impl Clock,
        finalized: (Height, Digest),
    ) -> eyre::Result<Option<(Height, Digest)>> {
        let delivery = match event {
            Event::Delivered(delivery) => delivery,
            Event::Failed(error) => return Err(error),
            Event::Changed => return Ok(None),
        };
        // Only a positively identified abort may retire cancelled work. Genuine
        // engine failures remain fatal even when their requester has left.
        let status = match delivery.status? {
            ExecutionStatus::Completed(status) => status.status,
            ExecutionStatus::Cancelled(cancelled) => {
                self.retire_cancelled(delivery.request, cancelled)?;
                return Ok(None);
            }
        };
        let reply = ExecutionReply {
            owner: delivery.request.owner,
            id: delivery.request.id,
            digest: delivery.request.digest,
            status,
        };
        let finalized = self.finalized_boundary(finalized);
        match reply.owner {
            Owner::Verification(_) => {
                self.verification_delivered(reply, clock, finalized);
            }
            Owner::Convergence => {
                return Ok(self.convergence_delivered(reply, clock, finalized));
            }
        }
        Ok(None)
    }

    fn retire_cancelled(
        &mut self,
        request: ExecutionRequest,
        cancelled: ExecutionReadCancelled,
    ) -> eyre::Result<()> {
        if !cancelled.budget.same_request(&request.budget) || !request.budget.is_cancelled() {
            eyre::bail!("execution cancellation does not match its cancelled request");
        }
        let Owner::Verification(round) = request.owner else {
            eyre::bail!("unexpected cancellation of parent convergence execution");
        };
        if self.queued.get(&round).is_some_and(|queued| {
            queued.walk.id == request.id && queued.walk.cursor.digest() == request.digest
        }) {
            self.queued.remove(&round);
        }
        tracing::debug!(request_id = request.id, digest = %request.digest,
            "cancelled verification execution retired without a verdict");
        Ok(())
    }

    fn convergence_delivered(
        &mut self,
        reply: ExecutionReply,
        clock: &impl Clock,
        finalized: (Height, Digest),
    ) -> Option<(Height, Digest)> {
        let walk = self
            .convergence
            .as_mut()
            .filter(|walk| walk.id == reply.id && walk.cursor.digest() == reply.digest)?;
        if !walk.current() || walk.conflicts_with(finalized) {
            walk.step = Step::Stopped;
            return None;
        }
        if walk.cursor.number() < finalized.0.get() {
            walk.reprobe();
            return None;
        }
        let progress = Progress {
            clock,
            marshal: self.marshal.as_ref(),
            finalized,
        };
        walk.converge(reply.status, &progress)
    }
    fn verification_delivered(
        &mut self,
        reply: ExecutionReply,
        clock: &impl Clock,
        finalized: (Height, Digest),
    ) {
        let Owner::Verification(round) = reply.owner else {
            return;
        };
        let Some(request) = self.queued.get_mut(&round).filter(|request| {
            request.walk.id == reply.id && request.walk.cursor.digest() == reply.digest
        }) else {
            return;
        };
        if !request.active(finalized) {
            if let Some(request) = self.queued.remove(&round) {
                request.walk.budget.cancel();
            }
            return;
        }
        if request.walk.cursor.number() < finalized.0.get() {
            request.walk.reprobe();
            return;
        }
        let progress = Progress {
            clock,
            marshal: self.marshal.as_ref(),
            finalized,
        };
        let verdict = request.walk.verdict(reply.status, &progress);
        if let Some(verdict) = verdict {
            if let Some(request) = self.queued.remove(&round) {
                let _ = request.response.send(verdict);
            }
        }
    }
}

impl Walk {
    fn missing_parent(&mut self, progress: &Progress<'_, impl Clock>) {
        if self.cursor.number().saturating_sub(1) <= progress.finalized.0.get() {
            self.retry(progress.clock);
        } else {
            self.fetch_parent(progress.marshal);
        }
    }

    fn verdict(
        &mut self,
        status: PayloadStatusEnum,
        progress: &Progress<'_, impl Clock>,
    ) -> Option<VerificationOutcome> {
        match status {
            PayloadStatusEnum::Valid if self.cursor.digest() == self.target.digest() => {
                Some(VerificationOutcome::Valid)
            }
            PayloadStatusEnum::Valid => {
                self.reprobe();
                None
            }
            PayloadStatusEnum::Invalid { .. } => Some(VerificationOutcome::Invalid),
            PayloadStatusEnum::Syncing => {
                self.missing_parent(progress);
                None
            }
            PayloadStatusEnum::Accepted => Some(VerificationOutcome::Unavailable),
        }
    }

    fn converge(
        &mut self,
        status: PayloadStatusEnum,
        progress: &Progress<'_, impl Clock>,
    ) -> Option<(Height, Digest)> {
        match status {
            PayloadStatusEnum::Valid => return self.valid_parent(progress),
            PayloadStatusEnum::Syncing => self.missing_parent(progress),
            PayloadStatusEnum::Invalid { .. } | PayloadStatusEnum::Accepted => {
                self.step = Step::Stopped
            }
        }
        None
    }

    fn valid_parent(&mut self, progress: &Progress<'_, impl Clock>) -> Option<(Height, Digest)> {
        if self.anchored == Some(progress.finalized.1)
            && self.cursor.digest() == self.target.digest()
        {
            self.step = Step::Stopped;
            return Some((Height::new(self.target.number()), self.target.digest()));
        }
        if self.reaches(progress.finalized) {
            self.anchored = Some(progress.finalized.1);
            self.reprobe();
        } else {
            self.fetch_parent(progress.marshal);
        }
        None
    }
}
