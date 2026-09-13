use super::ExecutorActor;
use commonware_consensus::types::Height;
use commonware_runtime::Clock;
use commonware_runtime::Metrics;
use commonware_runtime::Spawner;
use futures::future::BoxFuture;
use outbe_primitives::projection::ProjectionCheckpoint;
use reth_ethereum::node::api::BeaconForkChoiceUpdateError;

/// Result of one startup-only replay of a recovered finalized forkchoice.
///
/// The caller owns timeout, retry, and durable provider readback. Keeping those
/// concerns out of the actor preserves a single owner for Engine FCU types
/// without turning startup recovery into a second actor lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveredForkchoiceAttempt {
    Valid,
    Syncing,
    Invalid(String),
    Retryable(String),
    Fatal(String),
}

impl<E> ExecutorActor<E>
where
    E: Clock + Metrics + Spawner + Send + Sync + 'static,
{
    /// Replay the exact recovered forkchoice once before this actor is started.
    ///
    /// This method neither mutates actor state nor starts mailbox or heartbeat
    /// processing. A caller must confirm the durable provider identity before
    /// allowing any downstream startup side effect.
    pub fn replay_recovered_forkchoice_once(
        &self,
        expected: ProjectionCheckpoint,
    ) -> BoxFuture<'static, RecoveredForkchoiceAttempt> {
        let expected_height = Height::new(expected.block_number);
        let forkchoice = self.state.forkchoice;
        if self.state.head_height != expected_height
            || self.state.finalized_height != expected_height
            || forkchoice.head_block_hash != expected.block_hash
            || forkchoice.safe_block_hash != expected.block_hash
            || forkchoice.finalized_block_hash != expected.block_hash
        {
            return Box::pin(async move {
                RecoveredForkchoiceAttempt::Fatal(format!(
                    "executor recovered state does not match startup anchor {}:{}",
                    expected.block_number, expected.block_hash
                ))
            });
        }

        let engine = self.engine.clone();
        Box::pin(async move {
            match engine.fork_choice_updated(forkchoice, None).await {
                Ok(response) if response.is_valid() => RecoveredForkchoiceAttempt::Valid,
                Ok(response) if response.is_syncing() => RecoveredForkchoiceAttempt::Syncing,
                Ok(response) if response.is_invalid() => {
                    RecoveredForkchoiceAttempt::Invalid(format!("{response:?}"))
                }
                Ok(response) => RecoveredForkchoiceAttempt::Fatal(format!(
                    "unexpected recovered forkchoice response: {response:?}"
                )),
                Err(BeaconForkChoiceUpdateError::EngineUnavailable) => {
                    RecoveredForkchoiceAttempt::Retryable(
                        "beacon consensus engine task stopped before its FCU response".to_string(),
                    )
                }
                Err(error @ BeaconForkChoiceUpdateError::ForkchoiceUpdateError(_))
                | Err(error @ BeaconForkChoiceUpdateError::Internal(_)) => {
                    RecoveredForkchoiceAttempt::Fatal(error.to_string())
                }
            }
        })
    }
}
