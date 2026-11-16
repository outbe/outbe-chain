use super::super::ExecutorActor;
use super::{
    Digest, ExecutionReadCancelled, ExecutionStatus, Height, NodeLocalExecutionAbort, PayloadStatus,
};
use outbe_primitives::error::PrecompileError;

impl<E> ExecutorActor<E>
where
    E: commonware_runtime::Clock
        + commonware_runtime::Metrics
        + commonware_runtime::Spawner
        + Send
        + Sync
        + 'static,
{
    pub(in crate::executor::actor) async fn follow_consensus_parent(
        &mut self,
        parent: crate::executor::ingress::PendingParent,
    ) -> eyre::Result<()> {
        let convergence = self.verification.get_mut().record_parent(
            parent,
            (
                self.state.finalized_height,
                Digest(self.state.forkchoice.finalized_block_hash),
            ),
        );
        self.verification
            .get_mut()
            .record_progress(self.state.head_height, self.state.finalized_height);
        if let Some((height, digest)) = convergence {
            self.commit_convergence(height, digest).await?;
        }
        Ok(())
    }

    pub(in crate::executor::actor) async fn commit_convergence(
        &mut self,
        height: Height,
        digest: Digest,
    ) -> eyre::Result<()> {
        let next = self.state.update_head(height, digest);
        if next == self.state {
            return Ok(());
        }
        let response = self.engine.fork_choice_updated(next.forkchoice, None).await;
        self.reset_fcu_heartbeat_deadline();
        let response = response.map_err(|error| {
            crate::errors::ExecutorError::ParentForkchoiceFailed(format!("{error}"))
        })?;
        if !response.is_valid() {
            return Err(
                crate::errors::ExecutorError::ParentForkchoiceNotValid(format!("{response:?}"))
                    .into(),
            );
        }
        self.state = next;
        self.verification
            .get_mut()
            .record_progress(self.state.head_height, self.state.finalized_height);
        Ok(())
    }
}

impl ExecutionStatus {
    pub(super) fn from_engine(
        result: Result<PayloadStatus, reth_ethereum::node::api::BeaconOnNewPayloadError>,
        digest: Digest,
    ) -> eyre::Result<Self> {
        let error = match result {
            Ok(status) => return Ok(Self::Completed(status)),
            Err(error) => error,
        };
        if let reth_ethereum::node::api::BeaconOnNewPayloadError::Internal(source) = &error {
            if let Some(status) = Self::classify_internal_error(source.as_ref()) {
                return Ok(status);
            }
        }
        Err(eyre::Report::new(error).wrap_err(
            crate::errors::ExecutorError::VerificationNewPayloadFailed(digest),
        ))
    }

    fn classify_internal_error(source: &(dyn std::error::Error + 'static)) -> Option<Self> {
        if let Some(cancelled) = ExecutionReadCancelled::find(source) {
            return Some(Self::Cancelled(cancelled.clone()));
        }
        if let Some(abort) = NodeLocalExecutionAbort::find(source) {
            return match abort.witness_reason {
                Some(
                    outbe_primitives::projection::WitnessAbortReason::TapeInvalid
                    | outbe_primitives::projection::WitnessAbortReason::SemanticMismatch,
                ) => Some(Self::Completed(PayloadStatus::from_status(
                    alloy_rpc_types_engine::PayloadStatusEnum::Invalid {
                        validation_error: abort.reason.clone(),
                    },
                ))),
                Some(outbe_primitives::projection::WitnessAbortReason::LocalUnavailable) | None => {
                    Some(Self::NodeLocalAbort(abort.clone()))
                }
            };
        }
        if let Some(reason) = find_precompile_error(source, PrecompileError::is_node_local) {
            return Some(Self::NodeLocalAbort(NodeLocalExecutionAbort::new(reason)));
        }
        find_precompile_error(source, is_candidate_validation_precompile_error).map(
            |validation_error| {
                Self::Completed(PayloadStatus::from_status(
                    alloy_rpc_types_engine::PayloadStatusEnum::Invalid { validation_error },
                ))
            },
        )
    }
}

fn find_precompile_error(
    mut error: &(dyn std::error::Error + 'static),
    predicate: fn(&PrecompileError) -> bool,
) -> Option<String> {
    loop {
        if let Some(precompile_error) = error
            .downcast_ref::<PrecompileError>()
            .filter(|err| predicate(err))
        {
            return Some(precompile_error.to_string());
        }
        error = error.source()?;
    }
}

const fn is_candidate_validation_precompile_error(precompile_error: &PrecompileError) -> bool {
    matches!(
        precompile_error,
        PrecompileError::Revert(_)
            | PrecompileError::RevertBytes(_)
            | PrecompileError::Halt(_)
            | PrecompileError::OutOfGas
            | PrecompileError::WriteProtection
            | PrecompileError::BodyReadCorruption(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_rpc_types_engine::PayloadStatusEnum;
    use outbe_primitives::projection::WitnessAbortReason;
    use reth_ethereum::node::api::BeaconOnNewPayloadError;

    #[test]
    fn witness_tape_invalid_and_semantic_mismatch_classify_as_invalid_payload_without_aborting_node(
    ) {
        for reason in [
            WitnessAbortReason::TapeInvalid,
            WitnessAbortReason::SemanticMismatch,
        ] {
            let abort = NodeLocalExecutionAbort::witness(reason, "witness failure");
            let err = BeaconOnNewPayloadError::Internal(Box::new(abort));
            let status = ExecutionStatus::from_engine(Err(err), Digest(B256::ZERO))
                .expect("witness invalid/mismatch must classify cleanly");
            assert!(
                matches!(
                    status,
                    ExecutionStatus::Completed(PayloadStatus {
                        status: PayloadStatusEnum::Invalid { validation_error },
                        ..
                    }) if validation_error == "witness failure"
                ),
                "expected PayloadStatusEnum::Invalid for {reason:?}"
            );
        }
    }

    #[test]
    fn witness_local_unavailable_classifies_as_node_local_abort() {
        let abort = NodeLocalExecutionAbort::witness(
            WitnessAbortReason::LocalUnavailable,
            "local tee socket timeout",
        );
        let err = BeaconOnNewPayloadError::Internal(Box::new(abort));
        let status = ExecutionStatus::from_engine(Err(err), Digest(B256::ZERO))
            .expect("local unavailable must classify as NodeLocalAbort");
        assert!(matches!(
            status,
            ExecutionStatus::NodeLocalAbort(NodeLocalExecutionAbort {
                witness_reason: Some(WitnessAbortReason::LocalUnavailable),
                ..
            })
        ));
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::super::*;
    use commonware_consensus::types::{Epoch, View};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::{primitives::SealedBlock, Block};

    fn parent(block: Option<Arc<ConsensusBlock>>, height: u64, digest: Digest) -> PendingParent {
        PendingParent {
            round: Round::new(Epoch::new(0), View::new(4)),
            height: Height::new(height),
            digest,
            block,
            epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
        }
    }

    fn gauge(snapshotter: &Snapshotter, name: &str) -> Option<DebugValue> {
        snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .find(|(key, _, _, _)| key.key().name() == name)
            .map(|(_, _, _, value)| value)
    }

    #[test]
    fn convergence_requires_matching_parent_and_tracks_head_progress() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            let mut work = VerificationWork::default();
            let finalized = (Height::zero(), Digest::ZERO);
            let name = "outbe_consensus_executor_convergence_depth_blocks";
            let mut block = Block::default();
            block.header.number = 55;
            let block = Arc::new(ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                block.map_header(OutbeHeader::new),
            )));
            let mismatched = Digest(alloy_primitives::B256::repeat_byte(1));
            for pending in [
                parent(None, 0, Digest::ZERO),
                parent(Some(block.clone()), 55, mismatched),
                parent(Some(block.clone()), 54, block.digest()),
            ] {
                work.record_parent(pending, finalized);
                work.record_progress(Height::new(50), finalized.0);
                assert_eq!(work.convergence_target_height, None);
                assert_eq!(gauge(&snapshotter, name), None);
            }
            work.record_parent(parent(Some(block.clone()), 55, block.digest()), finalized);
            work.record_progress(Height::new(50), finalized.0);
            assert_eq!(
                gauge(&snapshotter, name),
                Some(DebugValue::Gauge(5.0.into()))
            );
            work.record_progress(Height::new(55), finalized.0);
            assert_eq!(
                gauge(&snapshotter, name),
                Some(DebugValue::Gauge(0.0.into()))
            );
            work.record_progress(Height::new(60), finalized.0);
            assert_eq!(
                gauge(&snapshotter, name),
                Some(DebugValue::Gauge((-5.0).into()))
            );
        });
    }

    #[test]
    fn finalization_lag_tracks_known_tip_and_saturates() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            let mut work = VerificationWork::default();
            let name = "outbe_consensus_executor_finalization_lag_blocks";
            work.observe_finalized_height(Height::new(55));
            work.record_progress(Height::new(50), Height::new(50));
            assert_eq!(
                gauge(&snapshotter, name),
                Some(DebugValue::Gauge(5.0.into()))
            );
            work.finalized(
                Round::new(Epoch::new(0), View::new(4)),
                Height::new(57),
                Digest::ZERO,
            );
            work.record_progress(Height::new(55), Height::new(55));
            assert_eq!(
                gauge(&snapshotter, name),
                Some(DebugValue::Gauge(2.0.into()))
            );
            work.record_progress(Height::new(60), Height::new(60));
            assert_eq!(
                gauge(&snapshotter, name),
                Some(DebugValue::Gauge(0.0.into()))
            );
        });
    }
}
