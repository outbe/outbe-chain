use super::*;
use std::ops::ControlFlow;

impl ApplicationShared {
    /// Canonicalize the parent, start its payload build and resolve the sealed candidate.
    pub(in super::super) async fn build_block(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: BlockBuildRequest,
    ) -> eyre::Result<BuildBlockOutcome> {
        let round = request.round;
        let parent_height = request.parent.height;
        let parent_digest = request.parent.digest;
        let next_block_number = parent_height.get().saturating_add(1);
        if !self.build_epoch_current(&request, "dropping stale proposal before payload build") {
            return Ok(BuildBlockOutcome::EpochStale);
        }
        if crate::test_faults::should_drop_new_payload_for_test(Height::new(next_block_number)) {
            warn!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                "test-marshal-drop: skipping local proposal for dropped height"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        let attrs = match self.proposal_attributes(clock, &request).await? {
            ControlFlow::Continue(attrs) => attrs,
            ControlFlow::Break(outcome) => return Ok(outcome),
        };
        if !self.build_epoch_current(&request, "dropping stale proposal before FCU payload build") {
            return Ok(BuildBlockOutcome::EpochStale);
        }
        // FCU-based payload building: the executor actor canonicalizes the
        // parent and starts the build in one atomic operation.
        let payload_id = self
            .executor_mailbox
            .canonicalize_and_build(parent_height, parent_digest, attrs)
            .await
            .map_err(ApplicationError::CanonicalizeAndBuild)?;
        request.payload_trace.record(payload_id);

        debug!(%payload_id, "payload building started via FCU");

        let resolve_pad =
            Self::wait_for_payload_build(clock, request.propose_start, self.payload_resolve_time)
                .await;
        if !self.build_epoch_current(
            &request,
            "dropping stale proposal after payload build started",
        ) {
            return Ok(BuildBlockOutcome::EpochStale);
        }
        self.resolve_proposal_payload(payload_id, resolve_pad).await
    }
    fn build_epoch_current(&self, request: &BlockBuildRequest, message: &'static str) -> bool {
        let round = request.round;
        let parent_digest = request.parent.digest;
        let next_block_number = request.parent.height.get().saturating_add(1);
        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(%round, parent = %parent_digest.0, next_block_number, ?rejection, "{message}");
            return false;
        }
        true
    }
    async fn wait_for_payload_build(
        clock: &impl commonware_runtime::Clock,
        propose_start: std::time::SystemTime,
        payload_resolve_time: Duration,
    ) -> Duration {
        // Give the payload builder a bounded chance to execute transactions before
        // resolving. This code measures elapsed time against the runtime clock (same
        // source as the sleep below), so it is correct on the deterministic runtime too.
        let elapsed = clock
            .current()
            .duration_since(propose_start)
            .unwrap_or_default();
        let remaining_resolve = payload_resolve_time.saturating_sub(elapsed);

        clock.sleep(remaining_resolve).await;
        crate::metrics::propose::record_block_build_resolve_pad_ms(remaining_resolve);
        remaining_resolve
    }
    async fn resolve_proposal_payload(
        &self,
        payload_id: PayloadId,
        resolve_pad: Duration,
    ) -> eyre::Result<BuildBlockOutcome> {
        let payload = crate::execution_port::PayloadBuilderPort::resolve_kind(
            &self.payload_builder,
            payload_id,
            reth_payload_builder::PayloadKind::WaitForPending,
        )
        .await
        .ok_or(ApplicationError::PayloadResolutionNone)?
        .map_err(|e| ApplicationError::PayloadResolutionFailed(format!("{e}")))?;

        let sealed_block = payload.block().clone();

        let consensus_block = ConsensusBlock::from_sealed(sealed_block);
        let digest = consensus_block.digest();
        let block_number = consensus_block.number();
        debug!(%digest, number = block_number, "block built");

        crate::metrics::record_block_proposed(block_number);

        self.block_cache
            .insert_bounded(digest, consensus_block.clone());

        Ok(BuildBlockOutcome::Built(
            digest,
            consensus_block,
            resolve_pad,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_runtime::{Clock as _, Runner as _};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    #[test]
    fn resolve_sleep_is_observed_without_successful_publication() {
        for (elapsed_ms, pad_ms) in [(0, 200), (50, 150), (250, 0)] {
            let recorder = DebuggingRecorder::new();
            metrics::with_local_recorder(&recorder, || {
                commonware_runtime::deterministic::Runner::default().start(|clock| async move {
                    let start = clock.current();
                    clock.sleep(Duration::from_millis(elapsed_ms)).await;
                    let before = clock.current();
                    let pad = ApplicationShared::wait_for_payload_build(
                        &clock,
                        start,
                        Duration::from_millis(200),
                    )
                    .await;
                    assert_eq!(pad, Duration::from_millis(pad_ms));
                    assert_eq!(clock.current().duration_since(before).unwrap(), pad);
                    // No resolve, publication or pacing: later failure cannot erase the sample.
                });
            });
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            assert_eq!(snapshot.len(), 1);
            assert_eq!(
                snapshot[0].0.key().name(),
                "outbe_consensus_block_build_resolve_pad_ms"
            );
            assert_eq!(
                snapshot[0].3,
                DebugValue::Histogram(vec![(pad_ms as f64).into()])
            );
        }
    }
}
