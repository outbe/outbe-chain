use super::AsyncOutcomeProjectionV1;
use super::EmbeddedOcompExExV1;

use eyre::bail;
use eyre::Context as _;

use outbe_ocomp::embedded::EmbeddedJobActionV1;
use outbe_ocomp::embedded::EmbeddedJobEventV1;

use outbe_ocomp::embedded_runtime::EmbeddedVoteOutcomeV1;

use outbe_primitives::OutbeReceipt;

use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;

use tracing::info;

impl<P> EmbeddedOcompExExV1<P>
where
    P: BlockIdReader
        + BlockHashReader
        + BlockReader
        + ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    pub(super) fn handle_vote(&mut self, outcome: EmbeddedVoteOutcomeV1) -> eyre::Result<()> {
        let (job_id, event, detail) = match outcome {
            EmbeddedVoteOutcomeV1::Finalized {
                job_id,
                generation,
                success,
            } => (
                job_id,
                EmbeddedJobEventV1::VoteFinalized {
                    generation,
                    success,
                },
                None,
            ),
            EmbeddedVoteOutcomeV1::Unrecoverable {
                job_id,
                generation,
                detail,
            } => (
                job_id,
                EmbeddedJobEventV1::VoteFailed { generation },
                Some(detail),
            ),
        };
        let Some(projection) = self.async_outcome_projection(job_id)? else {
            return Ok(());
        };
        if projection == AsyncOutcomeProjectionV1::CheckpointPruned {
            info!(%job_id, "ignored checkpoint-pruned embedded OCOMP vote outcome");
            return Ok(());
        }
        match self
            .state
            .reduce(job_id, event)
            .wrap_err("reduce embedded OCOMP vote outcome")?
            .action
        {
            EmbeddedJobActionV1::VoteFinalized { success } => {
                info!(%job_id, success, "embedded OCOMP vote finalized");
            }
            EmbeddedJobActionV1::FatalVoteFailure => {
                self.latch_fatal(
                    job_id,
                    detail.unwrap_or_else(|| "embedded OCOMP vote failed".to_owned()),
                )?;
            }
            EmbeddedJobActionV1::ProtocolOwned => {
                info!(%job_id, "ignored late embedded OCOMP vote outcome");
            }
            _ => {
                bail!("unexpected embedded OCOMP vote action");
            }
        }
        Ok(())
    }
}
