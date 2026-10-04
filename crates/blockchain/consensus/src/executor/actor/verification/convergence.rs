use super::super::ExecutorActor;
use super::{Digest, Height};

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
        let response =
            response.map_err(|error| eyre::eyre!("consensus parent forkchoice failed: {error}"))?;
        if !response.is_valid() {
            return Err(eyre::eyre!(
                "consensus parent forkchoice must be VALID: {response:?}"
            ));
        }
        self.state = next;
        Ok(())
    }
}
