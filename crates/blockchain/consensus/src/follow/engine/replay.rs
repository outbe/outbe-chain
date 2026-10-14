//! Authentication and ordered reconciliation of a retained follower suffix.
use super::*;
use crate::block::ConsensusBlock;
use crate::follow::upstream::{AncestorFinalityProof, CertifiedFinalizedBlock};

/// Inclusive replay interval and its trusted anchor epoch.
/// Validation occurs when replay starts, before any upstream or archive I/O.
pub struct ReplayWindow {
    pub anchor_epoch: Epoch,
    pub lower: Height,
    pub upper: Height,
}
/// Borrowed authentication context shared by every record in a replay interval.
pub struct ReplayAuthority<'a, F> {
    pub chain: &'a SharedCommitteeChain,
    pub source: &'a F,
    pub epocher: &'a FollowerEpocher,
}
/// Owned archives handed to replay and returned separately on success.
/// The input keeps certificates-before-blocks drop order until replay is polled.
pub struct ReplayArchives<FC, FB> {
    certificates: FC,
    blocks: FB,
}
impl<FC, FB> ReplayArchives<FC, FB> {
    /// Transfer archive ownership without reading, writing or syncing either archive.
    pub const fn new(certificates: FC, blocks: FB) -> Self {
        Self {
            certificates,
            blocks,
        }
    }
}

struct ReplayProgress<FC, FB> {
    // Retain the original locals' blocks-before-certificates drop order.
    blocks: FB,
    certificates: FC,
    wrote_certificates: bool,
    wrote_blocks: bool,
}
impl<FC, FB> ReplayProgress<FC, FB> {
    fn new(certificates: FC, blocks: FB) -> Self {
        Self {
            blocks,
            certificates,
            wrote_certificates: false,
            wrote_blocks: false,
        }
    }
}
impl<F: FinalizedSource> ReplayAuthority<'_, F> {
    pub(super) async fn run<FC, FB>(
        self,
        window: ReplayWindow,
        archives: ReplayArchives<FC, FB>,
    ) -> Result<(Epoch, FC, FB)>
    where
        FC: Certificates<BlockDigest = Digest, Commitment = Digest, Scheme = HybridScheme<MinSig>>,
        FB: Blocks<Block = ConsensusBlock>,
    {
        let mut archives = ReplayProgress::new(archives.certificates, archives.blocks);
        let ReplayWindow {
            anchor_epoch,
            lower,
            upper,
        } = window;
        if lower > upper {
            return Err(FollowError::ReplayLowerExceedsUpper {
                lower: lower.get(),
                upper: upper.get(),
            }
            .into());
        }
        let lower_epoch =
            prepare_committee_chain(self.chain, self.source, self.epocher, anchor_epoch, lower)
                .await?;
        self.recover_pending_successor(lower_epoch, lower, &archives.blocks)
            .await?;
        for raw_height in lower.get()..=upper.get() {
            if raw_height == 0 {
                continue;
            }
            let height = Height::new(raw_height);
            let proof = self
                .source
                .get_finality_proof(height)
                .await
                .ok_or_else(|| FollowError::MissingReplaySuffixHeight {
                    height: height.get(),
                })?;
            authenticate_ancestor_proof(self.chain, self.epocher, height, &proof)?;
            archives = archives
                .reconcile_certificate(self.chain, &proof.certified)
                .await?;
            for block in proof
                .ancestors
                .iter()
                .chain(std::iter::once(&proof.certified.block))
            {
                archives = archives.reconcile_block(self.chain, block).await?;
            }
        }
        let archives = archives.sync().await?;
        Ok((
            self.chain
                .lock()
                .highest_registered()
                .unwrap_or(anchor_epoch),
            archives.certificates,
            archives.blocks,
        ))
    }
    async fn recover_pending_successor<FB>(
        &self,
        active_epoch: Epoch,
        lower: Height,
        blocks: &FB,
    ) -> Result<()>
    where
        FB: Blocks<Block = ConsensusBlock>,
    {
        let epocher = self.epocher;
        use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact as CHA;

        let Some(activation) = epocher.activation_height(active_epoch) else {
            return Ok(());
        };
        if lower <= activation {
            return Ok(());
        }
        let successor = active_epoch.get().saturating_add(1);
        let mut found_successor = false;

        for raw_height in (activation.get()..lower.get()).rev() {
            let height = Height::new(raw_height);
            let Some(retained) = self.retained_block(blocks, height, found_successor).await? else {
                continue;
            };
            let inspected_block = retained.block();
            let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
                inspected_block.header().extra_data().as_ref(),
            )
            .map_err(|error| FollowError::decode_retained_block_artifacts(height.get(), error))?;
            if !matches!(
                artifacts.consensus_header_artifact,
                Some(CHA::CommitteePreAnnounce { epoch, .. }) if epoch == successor
            ) {
                continue;
            }

            self.authenticate_preannounce(retained, height).await?;
            found_successor = true;
        }
        Ok(())
    }
    async fn retained_block<FB>(
        &self,
        blocks: &FB,
        height: Height,
        found_successor: bool,
    ) -> Result<Option<RetainedReplayBlock>>
    where
        FB: Blocks<Block = ConsensusBlock>,
    {
        let local = blocks
            .get(Identifier::Index(height.get()))
            .await
            .map_err(|error| FollowError::inspect_replay_block(height.get(), error))?;
        if let Some(local) = local {
            return Ok(Some(RetainedReplayBlock::Local(local)));
        }
        if found_successor {
            return Ok(None);
        }
        let fetched = self
            .source
            .get_finality_proof(height)
            .await
            .ok_or_else(|| FollowError::MissingRetainedHistoryHeight {
                height: height.get(),
            })?;
        Ok(Some(RetainedReplayBlock::Fetched(Box::new(fetched))))
    }
    async fn authenticate_preannounce(
        &self,
        retained: RetainedReplayBlock,
        height: Height,
    ) -> Result<()> {
        let certified = match retained {
            RetainedReplayBlock::Fetched(proof) => *proof,
            RetainedReplayBlock::Local(block) => {
                let proof = self
                    .source
                    .get_finality_proof(height)
                    .await
                    .ok_or_else(|| FollowError::MissingRetainedPreannounceHeight {
                        height: height.get(),
                    })?;
                if block.encode() != proof.target().encode() {
                    return Err(FollowError::RetainedPreannounceDiffers {
                        height: height.get(),
                    }
                    .into());
                }
                proof
            }
        };
        authenticate_ancestor_proof(self.chain, self.epocher, height, &certified)
    }
}
enum RetainedReplayBlock {
    Local(ConsensusBlock),
    Fetched(Box<AncestorFinalityProof>),
}
impl RetainedReplayBlock {
    fn block(&self) -> &ConsensusBlock {
        match self {
            Self::Local(block) => block,
            Self::Fetched(proof) => proof.target(),
        }
    }
}
impl<FC, FB> ReplayProgress<FC, FB>
where
    FC: Certificates<BlockDigest = Digest, Commitment = Digest, Scheme = HybridScheme<MinSig>>,
    FB: Blocks<Block = ConsensusBlock>,
{
    async fn reconcile_certificate(
        mut self,
        chain: &SharedCommitteeChain,
        certified: &CertifiedFinalizedBlock,
    ) -> Result<Self> {
        let height = Height::new(certified.block.number());
        let digest = certified.block.digest();
        match self
            .certificates
            .get(Identifier::Index(height.get()))
            .await
            .map_err(|error| FollowError::read_replay_finalization(height.get(), error))?
        {
            Some(local) => {
                if local.proposal != certified.finalization.proposal {
                    return Err(FollowError::ReplayFinalizationProposalDiffers {
                        height: height.get(),
                    }
                    .into());
                }
                let epoch = local.proposal.round.epoch();
                chain
                    .lock()
                    .verify_finalization(epoch, &local)
                    .map_err(|error| {
                        FollowError::replay_finalization_verification_failed(height.get(), error)
                    })?;
            }
            None => {
                self.certificates = self
                    .certificates
                    .put(height, digest, certified.finalization.clone())
                    .await
                    .map_err(|error| {
                        FollowError::repair_replay_finalization(height.get(), error)
                    })?;
                self.wrote_certificates = true;
            }
        }

        Ok(self)
    }
    async fn reconcile_block(
        mut self,
        chain: &SharedCommitteeChain,
        block: &ConsensusBlock,
    ) -> Result<Self> {
        let height = Height::new(block.number());
        if let Some(local_certificate) = self
            .certificates
            .get(Identifier::Index(height.get()))
            .await
            .map_err(FollowError::read_replay_ancestor_certificate)?
        {
            if local_certificate.proposal.payload != block.digest() {
                return Err(FollowError::ReplayAncestorCertificatePayloadMismatch.into());
            }
            chain.lock().verify_finalization(
                local_certificate.proposal.round.epoch(),
                &local_certificate,
            )?;
        }
        match self
            .blocks
            .get(Identifier::Index(height.get()))
            .await
            .map_err(FollowError::read_replay_block)?
        {
            Some(local) => {
                if local.encode() != block.encode() {
                    return Err(FollowError::ReplayBlockDiffers {
                        height: height.get(),
                    }
                    .into());
                }
            }
            None => {
                self.blocks = self
                    .blocks
                    .put(block.clone())
                    .await
                    .map_err(FollowError::repair_replay_block)?;
                self.wrote_blocks = true;
            }
        }
        Ok(self)
    }
    async fn sync(mut self) -> Result<Self> {
        if self.wrote_certificates {
            self.certificates = self
                .certificates
                .sync()
                .await
                .map_err(FollowError::sync_replay_finalizations)?;
        }
        if self.wrote_blocks {
            self.blocks = self
                .blocks
                .sync()
                .await
                .map_err(FollowError::sync_replay_blocks)?;
        }

        Ok(self)
    }
}
