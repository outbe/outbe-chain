//! Authenticate the public finality wire with the same committee verifier as a
//! real follower. RPC delivery and decoding alone are never finality evidence.

use std::collections::BTreeMap;

use alloy_primitives::{Bytes, B256};
use commonware_codec::DecodeExt as _;
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381;
use commonware_utils::TryCollect as _;
use eyre::{ensure, eyre, Result};
use outbe_consensus::follow::{
    decode_public_finalized_block, CertifiedFinalizedBlock, CommitteeChain,
};
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use serde::Deserialize;

use crate::internal::eth;
use crate::world::rpc::{FinalizedCheckpoint, Rpc};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinalizationWire {
    finalization_hex: Bytes,
    block_hex: Bytes,
}

pub(crate) fn read_certified(
    rpc: &Rpc,
    port: u16,
    height: u64,
    committee_members: usize,
) -> Result<CertifiedFinalizedBlock> {
    let wire: FinalizationWire = serde_json::from_value(eth::raw_json_result(
        &rpc.url(port),
        "outbe_getFinalization",
        serde_json::json!([height]),
    )?)?;
    decode_public_finalized_block(&wire.finalization_hex, &wire.block_hex, committee_members)
        .map_err(|error| eyre!("decode local finalization on port {port} at h{height}: {error}"))
}

pub(crate) struct AuthenticatedHistory {
    chain: CommitteeChain,
    member_count: usize,
    height: u64,
    last_hash: B256,
    epoch: u64,
    epoch_boundaries: BTreeMap<u64, u64>,
}

impl std::fmt::Debug for AuthenticatedHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatedHistory")
            .field("height", &self.height)
            .field("last_hash", &self.last_hash)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedHistory {
    pub(crate) fn new(binding: &DcapSeededChainSpecBindingV1) -> Result<Self> {
        let participants = binding
            .genesis_consensus_keys
            .iter()
            .map(|key| {
                bls12381::PublicKey::decode(key.as_slice())
                    .map_err(|error| eyre!("decode genesis consensus key: {error}"))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .try_collect()
            .map_err(|error| eyre!("invalid genesis participant set: {error:?}"))?;
        ensure!(
            !binding.genesis_consensus_keys.is_empty(),
            "empty genesis committee"
        );
        Ok(Self {
            chain: CommitteeChain::new(Epoch::new(0), participants),
            member_count: binding.genesis_consensus_keys.len(),
            height: 0,
            last_hash: binding.genesis_hash,
            epoch: 0,
            epoch_boundaries: BTreeMap::new(),
        })
    }

    pub(crate) fn height(&self) -> u64 {
        self.height
    }
    pub(crate) fn member_count(&self) -> usize {
        self.member_count
    }
    pub(crate) fn highest_registered_epoch(&self) -> Result<u64> {
        self.chain
            .highest_registered()
            .map(|epoch| epoch.get())
            .ok_or_else(|| eyre!("genesis committee has not been authenticated"))
    }

    /// A failed authentication terminates the scenario; never reuse its partial
    /// observations as trusted state or skip a failed height.
    pub(crate) fn advance(
        &mut self,
        certified: &CertifiedFinalizedBlock,
        expected: FinalizedCheckpoint,
    ) -> Result<Option<ConsensusHeaderArtifact>> {
        ensure!(
            Some(expected.height) == self.height.checked_add(1),
            "non-sequential certified history"
        );
        ensure!(
            certified.block.parent_hash() == self.last_hash,
            "certified history parent mismatch"
        );
        validate_envelope(certified, expected)?;
        let artifact =
            decode_outbe_block_artifacts(certified.block.header().inner.extra_data.as_ref())
                .map_err(|error| eyre!("decode certified header artifacts: {error}"))?
                .consensus_header_artifact;
        let epoch = certified.finalization.proposal.round.epoch();
        if self.height == 0 {
            let Some(ConsensusHeaderArtifact::BoundaryOutcome(ref boundary)) = artifact else {
                return Err(eyre!("block1 must carry the genesis committee boundary"));
            };
            ensure!(
                epoch.get() == 0 && boundary.epoch == 0,
                "non-genesis anchor epoch"
            );
            self.chain
                .register_epoch_from_outcome(epoch, &boundary.outcome)?;
        }
        ensure!(
            epoch.get() == self.epoch || self.epoch.checked_add(1) == Some(epoch.get()),
            "certificate skipped a committee epoch"
        );
        if epoch.get() != self.epoch {
            ensure!(
                matches!(artifact, Some(ConsensusHeaderArtifact::BoundaryOutcome(ref boundary)) if boundary.epoch == epoch.get()),
                "epoch changed without a boundary"
            );
        }
        // Later self-finalized boundaries cannot install their own verifier.
        self.chain
            .verify_finalization(epoch, &certified.finalization)?;
        match &artifact {
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                epoch: successor,
                outcome,
            }) => {
                ensure!(
                    epoch.get().checked_add(1) == Some(*successor),
                    "preannounce is not the certificate epoch successor"
                );
                self.chain
                    .register_epoch_from_outcome(Epoch::new(*successor), outcome)?;
            }
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => {
                ensure!(
                    boundary.epoch == epoch.get(),
                    "boundary epoch differs from certificate"
                );
                self.chain
                    .register_epoch_from_outcome(epoch, &boundary.outcome)?;
            }
            _ => {}
        }
        if self.height == 0 || self.epoch != epoch.get() {
            self.epoch_boundaries.insert(expected.height, epoch.get());
        }
        self.height = expected.height;
        self.last_hash = expected.block_hash;
        self.epoch = epoch.get();
        Ok(artifact)
    }

    /// Recheck a pinned carrier served by a particular follower, with its exact
    /// expected canonical identity and an already authenticated historical key.
    pub(crate) fn verify_retained(
        &self,
        certified: &CertifiedFinalizedBlock,
        expected: FinalizedCheckpoint,
    ) -> Result<()> {
        validate_envelope(certified, expected)?;
        ensure!(
            expected.height <= self.height,
            "retained certificate is outside authenticated history"
        );
        let (_, epoch) = self
            .epoch_boundaries
            .range(..=expected.height)
            .next_back()
            .ok_or_else(|| eyre!("no authenticated committee for retained height"))?;
        ensure!(
            certified.finalization.proposal.round.epoch().get() == *epoch,
            "retained certificate uses the wrong committee for its height"
        );
        self.chain.verify_finalization(
            certified.finalization.proposal.round.epoch(),
            &certified.finalization,
        )
    }
}

fn validate_envelope(
    certified: &CertifiedFinalizedBlock,
    expected: FinalizedCheckpoint,
) -> Result<()> {
    ensure!(
        certified.block.number() == expected.height,
        "certified block height mismatch"
    );
    ensure!(
        certified.block.block_hash() == expected.block_hash,
        "certified block hash mismatch"
    );
    ensure!(
        certified.block.header().inner.state_root == expected.state_root,
        "certified state root mismatch"
    );
    ensure!(
        certified.finalization.proposal.payload == certified.block.digest(),
        "certificate payload differs from its block"
    );
    Ok(())
}

#[derive(Debug)]
pub(crate) struct PinnedHandoff {
    pub history: AuthenticatedHistory,
    pub epoch: u64,
    pub carrier: FinalizedCheckpoint,
    pub outcome: Bytes,
    pub boundary: Option<FinalizedCheckpoint>,
    pub follower_pids: [u32; 2],
    pub survivor_anchor_after_fault: Option<u64>,
    pub follower_watermarks: Option<[u64; 2]>,
}

impl PinnedHandoff {
    pub(crate) fn require_before_boundary(&self, canonical_extra: &[u8]) -> Result<()> {
        let artifact = decode_outbe_block_artifacts(canonical_extra)
            .map_err(|error| eyre!("decode live boundary guard: {error}"))?
            .consensus_header_artifact;
        ensure!(
            !matches!(artifact, Some(ConsensusHeaderArtifact::BoundaryOutcome(ref boundary)) if boundary.epoch >= self.epoch),
            "pinned handoff epoch {} crossed its boundary before both live follower witnesses",
            self.epoch
        );
        Ok(())
    }

    pub(crate) fn verify_carrier(&self, proof: &CertifiedFinalizedBlock) -> Result<()> {
        self.history.verify_retained(proof, self.carrier)?;
        let artifact = decode_outbe_block_artifacts(proof.block.header().inner.extra_data.as_ref())
            .map_err(|error| eyre!("decode pinned carrier: {error}"))?
            .consensus_header_artifact;
        ensure!(
            matches!(artifact, Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, ref outcome }) if epoch == self.epoch && outcome == &self.outcome),
            "pinned preannounce epoch/outcome changed"
        );
        Ok(())
    }

    pub(crate) fn verify_boundary(&self, proof: &CertifiedFinalizedBlock) -> Result<()> {
        let expected = self
            .boundary
            .ok_or_else(|| eyre!("matching boundary has not been observed"))?;
        self.history.verify_retained(proof, expected)?;
        let artifact = decode_outbe_block_artifacts(proof.block.header().inner.extra_data.as_ref())
            .map_err(|error| eyre!("decode pinned boundary: {error}"))?
            .consensus_header_artifact;
        ensure!(
            matches!(artifact, Some(ConsensusHeaderArtifact::BoundaryOutcome(ref boundary)) if boundary.epoch == self.epoch && boundary.outcome == self.outcome),
            "pinned successor boundary changed"
        );
        ensure!(
            proof.finalization.proposal.round.epoch().get() == self.epoch,
            "successor boundary certified by wrong epoch"
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "certified_handoff_tests.rs"]
mod tests;
