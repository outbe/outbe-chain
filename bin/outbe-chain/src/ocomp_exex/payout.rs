use super::EmbeddedOcompExExV1;
use super::PAYOUT_LOOKBACK_DAYS;

use outbe_ocomp::embedded_runtime::EmbeddedOcompRuntimeErrorV1;
use outbe_ocomp::embedded_runtime::EmbeddedPayoutOutcomeV1;

use outbe_ocomp::payout_submitter::PayoutTickOutcomeV1;

use outbe_primitives::OutbeReceipt;

use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;

use std::sync::atomic::AtomicBool;

use std::sync::Arc;

use tracing::info;
use tracing::warn;

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
    /// Ticks the payout sender owned by the embedded Supervisor.
    /// One tick at a time: the submitter journals its own progress, so a later
    /// block simply resumes where this one stopped.
    pub(super) fn drive_payout(&mut self, timestamp: u64) {
        if self.payout_active {
            return;
        }
        // The chain's own clock, not the host's: a localnet can run a shifted
        // genesis, and a day the host has not reached yet still owes its payout.
        let mut day = outbe_primitives::time::worldwide_day_from_timestamp(timestamp);
        let mut days = Vec::with_capacity(PAYOUT_LOOKBACK_DAYS as usize + 1);
        for _ in 0..=PAYOUT_LOOKBACK_DAYS {
            days.push(day);
            day = outbe_primitives::time::previous_date_key(day);
        }
        days.reverse();
        match self.domain.spawn_validator_payout(
            days,
            Arc::new(AtomicBool::new(false)),
            self.payout_tx.clone(),
        ) {
            Ok(()) => self.payout_active = true,
            Err(EmbeddedOcompRuntimeErrorV1::FullNodeVoteAuthority) => {}
            Err(error) => warn!(?error, "OCOMP payout tick could not start"),
        }
    }

    pub(super) fn handle_payout(&mut self, outcome: EmbeddedPayoutOutcomeV1) {
        self.payout_active = false;
        match outcome {
            EmbeddedPayoutOutcomeV1::Ticked(PayoutTickOutcomeV1::Idle) => {}
            EmbeddedPayoutOutcomeV1::Ticked(outcome) => {
                info!(?outcome, "OCOMP payout tick advanced");
            }
            EmbeddedPayoutOutcomeV1::Failed(detail) => {
                warn!(detail, "OCOMP payout tick failed");
            }
        }
    }
}
