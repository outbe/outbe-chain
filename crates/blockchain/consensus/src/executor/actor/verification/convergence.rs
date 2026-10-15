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
        if let Some((height, digest)) = self.verification.get_mut().record_parent(
            parent,
            (
                self.state.finalized_height,
                Digest(self.state.forkchoice.finalized_block_hash),
            ),
            self.projection_readiness.clone(),
        ) {
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
        Err(eyre::Report::new(error).wrap_err(format!(
            "new_payload failed during executor verification: target={digest}"
        )))
    }

    fn classify_internal_error(source: &(dyn std::error::Error + 'static)) -> Option<Self> {
        if let Some(cancelled) = ExecutionReadCancelled::find(source) {
            return Some(Self::Cancelled(cancelled.clone()));
        }
        if let Some(abort) = NodeLocalExecutionAbort::find(source) {
            return Some(Self::NodeLocalAbort(abort.clone()));
        }
        if let Some(reason) = find_precompile_error(source, PrecompileError::is_node_local) {
            return Some(Self::NodeLocalAbort(NodeLocalExecutionAbort { reason }));
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
        PrecompileError::TransactionCeWorkLimitExceeded
            | PrecompileError::BlockCeWorkCapacityExhausted
            | PrecompileError::Revert(_)
            | PrecompileError::RevertBytes(_)
            | PrecompileError::Halt(_)
            | PrecompileError::OutOfGas
            | PrecompileError::WriteProtection
            | PrecompileError::BodyReadCorruption(_)
    )
}
