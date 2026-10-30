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
    FinalizedReplayGuard, Mailbox, OdkoOutcome, State,
};
use crate::errors::DkgError;

/// Canonical chain-reconstructible record of the revealed validator set at a
/// finalized epoch boundary (`W-08`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevealedSetRecord {
    /// Epoch activated by this boundary (`boundary.epoch == OdkoOutcome.epoch.get()`).
    pub epoch: u64,
    /// Height of the finalized carrier block (`block_number`).
    pub boundary_height: u64,
    /// Canonically ordered public keys whose individual share evaluations were
    /// revealed during the DKG ceremony (`output.revealed()`).
    pub revealed: Vec<bls12381::PublicKey>,
}

impl RevealedSetRecord {
    /// Construct a [`RevealedSetRecord`] from an already-decoded DKG [`Output`].
    pub fn from_output(
        epoch: u64,
        boundary_height: u64,
        output: &Output<MinSig, bls12381::PublicKey>,
    ) -> Self {
        Self {
            epoch,
            boundary_height,
            revealed: output.revealed().iter().cloned().collect(),
        }
    }

    /// Construct a [`RevealedSetRecord`] from a finalized [`DkgBoundaryArtifact`]
    /// and its carrier `block_number`.
    ///
    /// Returns `Ok(None)` when `block_number == 0` or `boundary.outcome` is a
    /// group-key-only bootstrap artifact (raw 48/96-byte group key or legacy
    /// ODKO-v1 group-key-only envelope).
    pub fn from_finalized_boundary(
        block_number: u64,
        boundary: &DkgBoundaryArtifact,
    ) -> std::result::Result<Option<Self>, DkgError> {
        if block_number == 0 || is_group_key_only_outcome(boundary)? {
            return Ok(None);
        }
        let decoded = OdkoOutcome::decode(&boundary.outcome)?;
        if decoded.epoch.get() != boundary.epoch {
            return Err(DkgError::BoundaryOutcomeEpochMismatch {
                boundary_epoch: boundary.epoch,
                outcome_epoch: decoded.epoch.get(),
            });
        }
        let _ = u32::try_from(decoded.output.revealed().len()).map_err(|_| {
            DkgError::RevealedValidatorCountOverflow {
                count: decoded.output.revealed().len(),
            }
        })?;
        Ok(Some(Self::from_output(
            boundary.epoch,
            block_number,
            &decoded.output,
        )))
    }

    /// Canonical byte encoding (`epoch:u64 BE || boundary_height:u64 BE ||
    /// count:u32 BE || concat(Encode::encode(pk))`).
    pub fn encode(&self) -> Bytes {
        let count = u32::try_from(self.revealed.len())
            .expect("revealed validator count bounded by MAX_VALIDATORS");
        let mut buf = Vec::with_capacity(8 + 8 + 4 + self.revealed.len() * 48);
        buf.extend_from_slice(&self.epoch.to_be_bytes());
        buf.extend_from_slice(&self.boundary_height.to_be_bytes());
        buf.extend_from_slice(&count.to_be_bytes());
        for pk in &self.revealed {
            buf.extend_from_slice(commonware_codec::Encode::encode(pk).as_ref());
        }
        Bytes::from(buf)
    }
}

fn is_group_key_only_outcome(
    boundary: &DkgBoundaryArtifact,
) -> std::result::Result<bool, DkgError> {
    let bytes = boundary.outcome.as_ref();
    if matches!(bytes.len(), 48 | 96) && &bytes[..4] != b"ODKO" {
        return Ok(true);
    }
    if bytes.len() >= 18 && &bytes[0..4] == b"ODKO" && bytes[4] == 0x01 && bytes[13] <= 1 {
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&bytes[14..18]);
        let declared = usize::try_from(u32::from_be_bytes(len_bytes)).unwrap_or(usize::MAX);
        let actual = bytes.len() - 18;
        if declared == actual && matches!(actual, 48 | 96) {
            let mut epoch_bytes = [0u8; 8];
            epoch_bytes.copy_from_slice(&bytes[5..13]);
            let outcome_epoch = u64::from_be_bytes(epoch_bytes);
            if outcome_epoch != boundary.epoch {
                return Err(DkgError::BoundaryOutcomeEpochMismatch {
                    boundary_epoch: boundary.epoch,
                    outcome_epoch,
                });
            }
            return Ok(true);
        }
    }
    Ok(false)
}

impl Mailbox {
    pub(super) fn record_finalized_revealed_set(
        state: &mut State,
        block_number: u64,
        boundary: &DkgBoundaryArtifact,
    ) {
        match RevealedSetRecord::from_finalized_boundary(block_number, boundary) {
            Ok(Some(record)) => {
                state.latest_revealed_set = Some(record);
            }
            Ok(None) => {}
            Err(error) => {
                warn!(
                    %error,
                    epoch = boundary.epoch,
                    block_number,
                    "failed to decode revealed validator set from finalized DKG boundary"
                );
            }
        }
    }

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
                Self::record_finalized_revealed_set(state, block_number, boundary);
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
/// The caller supplies chain order. The manager does not reorder or prefetch it.
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
