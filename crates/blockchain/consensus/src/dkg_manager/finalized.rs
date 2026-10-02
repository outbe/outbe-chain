//! Finalized boundary promotion, dealer-log effects and fenced ceremony replay.

use alloy_primitives::{Bytes, B256};
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self, dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use commonware_utils::ordered::Set;
use eyre::Result;
use outbe_primitives::{consensus::DkgBoundaryArtifact, reshare_artifact::ConsensusHeaderArtifact};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{
    ceremony::{FinalizedLogOutcome, ReconstructOutcome},
    dkg_output_hash, public_polynomial_hash, BoundaryStatus, CeremonyState, CommittedDkgBoundary,
    FinalizedReplayGuard, Mailbox, State,
};

impl Mailbox {
    pub(super) fn commit_finalized_boundary(
        state: &mut State,
        block_number: u64,
        block_hash: B256,
        boundary: &DkgBoundaryArtifact,
    ) {
        match Self::boundary_artifact_hash(boundary) {
            Ok(artifact_hash) => {
                let committed = CommittedDkgBoundary {
                    artifact: boundary.clone(),
                    artifact_hash,
                    block_number,
                    block_hash,
                };
                if state.pending_boundary.as_ref() == Some(boundary) {
                    state.committed_boundary = Some(committed.clone());
                }
                if block_hash != B256::ZERO {
                    Self::cache_boundary_status(
                        state,
                        block_hash,
                        artifact_hash,
                        BoundaryStatus::BoundaryCommitted(committed),
                    );
                }
            }
            Err(error) => {
                warn!(%error, "failed to record finalized DKG boundary status");
            }
        }
        if let Some(ceremony) = state.ceremony.as_mut() {
            ceremony.gossip.clear();
        }
    }
}

impl CeremonyState {
    /// Called under the manager's state lock and finalized replay fence.
    pub(super) fn note_finalized_dealer_log(&mut self, bytes: &Bytes) {
        let verified = match self.canonical.verify_dealer_log(bytes.as_ref()) {
            Ok(verified) => verified,
            Err(error) => {
                warn!(%error, "ignoring finalized DKG dealer log");
                return;
            }
        };
        // Stop re-gossiping a dealer log that is now chain-finalized.
        self.gossip.prune_finalized(&verified.dealer, bytes);

        let logs_len = match self.canonical.apply_finalized_dealer_log(verified) {
            FinalizedLogOutcome::DuplicateFinalized { dealer } => {
                debug!(dealer = ?dealer, "ignoring duplicate chain-finalized DKG dealer log");
                return;
            }
            FinalizedLogOutcome::Recorded { dealer, logs_len } => {
                debug!(dealer = ?dealer, logs = logs_len, "recorded finalized DKG dealer log");
                logs_len
            }
        };
        // Preserve insert -> actor notification -> canonical reconstruction.
        self.notify_finalized_dealer_log(bytes);
        self.reconstruct_from_finalized_logs(logs_len);
    }

    fn notify_finalized_dealer_log(&self, bytes: &Bytes) {
        if let Some(tx) = &self.finalized_dealer_log_tx {
            if tx.send(bytes.clone()).is_err() {
                debug!("active DKG actor is no longer accepting finalized dealer logs");
            }
        }
    }

    fn reconstruct_from_finalized_logs(&mut self, logs_len: usize) {
        match self.canonical.try_reconstruct_if_needed() {
            ReconstructOutcome::Reconstructed(output) => {
                info!(
                    output_hash = %dkg_output_hash(&output),
                    polynomial_hash = %public_polynomial_hash(output.public()),
                    logs = logs_len,
                    dealers = output.dealers().len(),
                    players = output.players().len(),
                    "canonical DKG output reconstructed from finalized dealer logs"
                );
            }
            ReconstructOutcome::Pending(error) => {
                debug!(
                    %error,
                    logs = logs_len,
                    "finalized DKG dealer logs do not yet produce an output"
                );
            }
            ReconstructOutcome::AlreadyReconstructed => {}
        }
    }
}

/// Existing ceremony inputs and the canonical finalized-log prefix to replay.
/// The caller supplies chain order; the manager does not reorder or prefetch it.
pub struct CeremonyReplayRequest<I> {
    pub epoch: Epoch,
    pub round: u64,
    pub previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    pub participants: Set<bls12381::PublicKey>,
    pub finalized_dealer_log_tx: Option<mpsc::UnboundedSender<Bytes>>,
    pub finalized_logs: I,
}

impl FinalizedReplayGuard<'_> {
    pub fn restart_ceremony_with_finalized_logs(
        &self,
        request: CeremonyReplayRequest<impl IntoIterator<Item = (u64, B256, Bytes)>>,
    ) -> Result<()> {
        self.mailbox.note_ceremony_started_with_finalized_log_tx(
            request.epoch,
            request.round,
            request.previous_output,
            request.participants,
            request.finalized_dealer_log_tx,
        )?;
        for (block_number, block_hash, bytes) in request.finalized_logs {
            let artifact = ConsensusHeaderArtifact::DealerLog(bytes);
            self.mailbox.note_finalized_header_artifact_at_inner(
                block_number,
                block_hash,
                Some(&artifact),
            );
        }
        Ok(())
    }
}
