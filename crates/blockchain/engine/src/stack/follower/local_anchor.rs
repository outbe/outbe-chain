//! Restore a follower committee only from accepted local finalized history.
use alloy_consensus::BlockHeader as _;
use alloy_primitives::B256;
use commonware_consensus::{
    marshal::store::{Blocks, Certificates},
    types::{Epoch, Epocher, Height},
};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_storage::archive::Identifier;
use eyre::{ensure, eyre, Result};
use outbe_consensus::{
    block::ConsensusBlock,
    digest::Digest,
    follow::{CommitteeChain, FollowerEpocher},
    hybrid::HybridScheme,
};
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

pub(in crate::stack) struct RestoredCommittee {
    pub(in crate::stack) chain: CommitteeChain,
    pub(in crate::stack) epocher: FollowerEpocher,
}

pub(in crate::stack) struct LocalFinalizedHistory<'a, C, B, H> {
    pub(in crate::stack) certificates: &'a C,
    pub(in crate::stack) blocks: &'a B,
    pub(in crate::stack) canonical_hash: H,
    pub(in crate::stack) floor: u64,
    pub(in crate::stack) epoch_length: u64,
    pub(in crate::stack) activation_grace: u64,
}

impl<C, B, H> LocalFinalizedHistory<'_, C, B, H>
where
    C: Certificates<BlockDigest = Digest, Commitment = Digest, Scheme = HybridScheme<MinSig>>,
    B: Blocks<Block = ConsensusBlock>,
    H: Fn(u64) -> Result<Option<B256>>,
{
    pub(in crate::stack) async fn restore(self) -> Result<Option<RestoredCommittee>> {
        let Some(certificate) = self.read_floor().await? else {
            return Ok(None);
        };
        let epoch = certificate.proposal.round.epoch();
        if epoch == Epoch::new(0) {
            return Ok(None);
        }
        let Some(restored) = self.find_boundary(epoch).await? else {
            return Ok(None);
        };
        restored.chain.verify_finalization(epoch, &certificate)?;
        Ok(Some(restored))
    }

    async fn read_floor(&self) -> Result<Option<outbe_consensus::marshal_types::Finalization>> {
        if self.floor == 0 {
            return Ok(None);
        }
        let Some(certificate) = self
            .certificates
            .get(Identifier::Index(self.floor))
            .await
            .map_err(|error| {
                eyre!(
                    "failed to read local floor certificate {}: {error}",
                    self.floor
                )
            })?
        else {
            return Ok(None);
        };
        let Some(block) = self
            .blocks
            .get(Identifier::Index(self.floor))
            .await
            .map_err(|error| eyre!("failed to read local floor block {}: {error}", self.floor))?
        else {
            return Ok(None);
        };
        self.validate_canonical(&block, self.floor)?;
        ensure!(
            certificate.proposal.payload == block.digest(),
            "local finalized floor {} certificate disagrees with canonical execution state",
            self.floor
        );
        Ok(Some(certificate))
    }

    async fn find_boundary(&self, epoch: Epoch) -> Result<Option<RestoredCommittee>> {
        let first = self
            .floor
            .saturating_sub(self.epoch_length.saturating_add(self.activation_grace))
            .max(1);
        for height in (first..=self.floor).rev() {
            let Some(block) =
                self.blocks
                    .get(Identifier::Index(height))
                    .await
                    .map_err(|error| {
                        eyre!("failed to inspect local follower block {height}: {error}")
                    })?
            else {
                continue;
            };
            let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
                block.header().extra_data().as_ref(),
            )
            .map_err(|error| eyre!("invalid local boundary candidate {height}: {error:?}"))?;
            let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
                artifacts.consensus_header_artifact
            else {
                continue;
            };
            if boundary.epoch != epoch.get() {
                continue;
            }
            self.validate_canonical(&block, height)?;
            return self
                .restore_boundary(epoch, &block, boundary.outcome.as_ref())
                .await
                .map(Some);
        }
        Ok(None)
    }

    fn validate_canonical(&self, block: &ConsensusBlock, height: u64) -> Result<()> {
        let canonical = (self.canonical_hash)(height)?
            .ok_or_else(|| eyre!("missing canonical local block at height {height}"))?;
        ensure!(
            block.number() == height && block.block_hash() == canonical,
            "local finalized block {height} disagrees with canonical execution state"
        );
        Ok(())
    }

    async fn restore_boundary(
        &self,
        epoch: Epoch,
        block: &ConsensusBlock,
        outcome: &[u8],
    ) -> Result<RestoredCommittee> {
        let height = block.number();
        let chain = CommitteeChain::from_trusted_local_boundary(epoch, outcome)?;
        if let Some(certificate) = self
            .certificates
            .get(Identifier::Index(height))
            .await
            .map_err(|error| eyre!("failed to read local boundary certificate {height}: {error}"))?
        {
            ensure!(
                certificate.proposal.payload == block.digest()
                    && certificate.proposal.round.epoch() == epoch,
                "local boundary certificate conflicts with canonical block {height}"
            );
            chain.verify_finalization(epoch, &certificate)?;
        }
        let epocher = FollowerEpocher::from_anchor(
            self.epoch_length,
            self.activation_grace,
            epoch,
            Height::new(height),
        );
        ensure!(
            epocher
                .containing(Height::new(self.floor))
                .is_some_and(|value| value.epoch() == epoch),
            "local committee boundary {height} cannot contain floor {}",
            self.floor
        );
        Ok(RestoredCommittee { chain, epocher })
    }
}
