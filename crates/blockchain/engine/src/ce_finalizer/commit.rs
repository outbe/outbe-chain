//! Finalized delivery stages run under the coordinator's serialization guard.
use super::{
    validate_durable_header_evidence, DurableCePersistence, DurableCeProbe, FinalizedCeTree,
};
use crate::ce_recovery::CanonicalCeReplayBlock;
use alloy_primitives::B256;
use outbe_compressed_entities::{FinalizedMarker, StagedTreeBatch, ACTIVE_COMMITMENT_SCHEME};
use outbe_consensus::executor::actor::FinalizedCeBlock;

/// One target bound to the previously committed marker and the DB-proven root.
pub(super) struct FinalizedCommit {
    pub(super) block: FinalizedCeBlock,
    pub(super) current: FinalizedMarker,
    pub(super) authoritative_root: B256,
}

/// Borrowed proof and tree dependencies for a single serialized delivery.
pub(super) struct FinalizedDelivery<'a> {
    pub(super) persistence: &'a DurableCePersistence,
    pub(super) tree: &'a dyn FinalizedCeTree,
}

impl FinalizedDelivery<'_> {
    pub(super) async fn validate_redelivery(
        &self,
        block: FinalizedCeBlock,
        current: FinalizedMarker,
    ) -> eyre::Result<()> {
        validate_redelivery_identity(block, current)?;
        let authoritative_root = self.redelivery_root(block).await?;
        if current.new_root != authoritative_root {
            eyre::bail!(
                "redelivered finalized CE block root conflicts with the durable marker at {}/{}: Reth={}, marker={}",
                block.height,
                block.block_hash,
                authoritative_root,
                current.new_root
            );
        }
        self.validate_redelivery_parent(block, current)
    }

    async fn redelivery_root(&self, block: FinalizedCeBlock) -> eyre::Result<B256> {
        let authoritative_root = match self.persistence.verify_durable(block)? {
            Some(root) => root,
            None => {
                self.persistence.wait_for_exact_persistence(block).await?;
                self.persistence.verify_durable(block)?.ok_or_else(|| {
                    eyre::eyre!(
                        "Reth emitted persistence for redelivered block {}/{}, but DB-only state is absent",
                        block.height,
                        block.block_hash
                    )
                })?
            }
        };
        Ok(authoritative_root)
    }

    fn validate_redelivery_parent(
        &self,
        block: FinalizedCeBlock,
        current: FinalizedMarker,
    ) -> eyre::Result<()> {
        let (durable_parent, durable_parent_root) = self.read_durable_parent(block, || {
            format!(
                "durable parent is absent while validating redelivered finalized CE block {}/{}",
                block.height, block.block_hash
            )
        })?;
        if current.parent_block_hash != durable_parent.block_hash
            || current.parent_root != durable_parent_root
        {
            eyre::bail!(
                "redelivered finalized CE block parent identity conflicts with the durable marker at {}/{}: Reth=({}, {}), marker=({}, {})",
                block.height,
                block.block_hash,
                durable_parent.block_hash,
                durable_parent_root,
                current.parent_block_hash,
                current.parent_root
            );
        }
        Ok(())
    }

    pub(super) async fn new_delivery_root(&self, block: FinalizedCeBlock) -> eyre::Result<B256> {
        let authoritative_root = match self.persistence.probe_durable(block)? {
            DurableCeProbe::Exact(root) => root,
            DurableCeProbe::Absent | DurableCeProbe::DifferentHash(_) => {
                self.persistence.wait_for_exact_persistence(block).await?;
                self.persistence.verify_durable(block)?.ok_or_else(|| {
                    eyre::eyre!(
                        "Reth emitted persistence for {}/{}, but DB-only state is absent",
                        block.height,
                        block.block_hash
                    )
                })?
            }
        };
        Ok(authoritative_root)
    }

    pub(super) fn validate_new_parent(
        &self,
        block: FinalizedCeBlock,
        current: FinalizedMarker,
    ) -> eyre::Result<()> {
        let (durable_parent, durable_parent_root) = self.read_durable_parent(block, || {
            format!(
                "durable parent is absent for finalized CE block {}/{}",
                block.height, block.block_hash
            )
        })?;
        if durable_parent.block_hash != block.parent_block_hash
            || durable_parent.block_hash != current.block_hash
            || durable_parent_root != current.new_root
        {
            eyre::bail!(
                "durable parent/header conflicts with finalized CE parent at {}/{}: actor=({}, {}), marker=({}, {})",
                block.height,
                block.block_hash,
                block.parent_block_hash,
                durable_parent_root,
                current.block_hash,
                current.new_root
            );
        }

        Ok(())
    }

    fn read_durable_parent(
        &self,
        block: FinalizedCeBlock,
        missing_message: impl FnOnce() -> String,
    ) -> eyre::Result<(super::DurableCeEvidence, B256)> {
        let parent_height = block.height.saturating_sub(1);
        let parent = self
            .persistence
            .state
            .block_and_root(parent_height)?
            .ok_or_else(|| eyre::eyre!(missing_message()))?;
        let root = validate_durable_header_evidence(parent_height, parent)?;
        Ok((parent, root))
    }

    pub(super) fn apply_commit(&self, commit: &FinalizedCommit) -> eyre::Result<FinalizedMarker> {
        let FinalizedCommit {
            block,
            current: _,
            authoritative_root,
        } = *commit;
        let marker = if let Some(candidate) = self.tree.candidate(block.height, block.block_hash)? {
            commit.validate_candidate(&candidate)?;
            self.tree
                .apply_finalized(block.height, block.block_hash, authoritative_root)?
        } else {
            // Validator/import execution must not publish before Reth's receipt
            // and state-root checks. Once the block is durable and exact, rebuild
            // the same batch from its canonical receipts instead of trusting a
            // speculative executor artifact.
            let replay = self
                .persistence
                .state
                .replay_block(block.height)?
                .ok_or_else(|| {
                    eyre::eyre!(
                        "durable canonical CE replay missing for finalized block {}/{}",
                        block.height,
                        block.block_hash
                    )
                })?;
            commit.validate_replay(&replay)?;
            self.tree.apply_replayed(&replay)?
        };
        Ok(marker)
    }
}

fn validate_redelivery_identity(
    block: FinalizedCeBlock,
    current: FinalizedMarker,
) -> eyre::Result<()> {
    if current.commitment_scheme_version != ACTIVE_COMMITMENT_SCHEME
        || current.block_hash != block.block_hash
        || current.parent_block_hash != block.parent_block_hash
    {
        eyre::bail!(
                "redelivered finalized CE block conflicts with the durable marker at {}/{}: {current:?}",
                block.height,
                block.block_hash
            );
    }
    Ok(())
}

impl FinalizedCommit {
    fn validate_candidate(&self, candidate: &StagedTreeBatch) -> eyre::Result<()> {
        if candidate.parent_block_hash() != self.block.parent_block_hash {
            eyre::bail!(
                "finalized CE candidate parent conflict at {}/{}: actor={}, candidate={}",
                self.block.height,
                self.block.block_hash,
                self.block.parent_block_hash,
                candidate.parent_block_hash()
            );
        }
        if candidate.new_root() != self.authoritative_root {
            eyre::bail!(
                "durable EVM/CE candidate root conflict at {}/{}: evm={}, candidate={}",
                self.block.height,
                self.block.block_hash,
                self.authoritative_root,
                candidate.new_root()
            );
        }
        Ok(())
    }

    fn validate_replay(&self, replay: &CanonicalCeReplayBlock) -> eyre::Result<()> {
        if !self.replay_matches_target(replay) || !self.replay_extends_current(replay) {
            eyre::bail!(
                "durable canonical CE replay identity/root conflict for finalized block {}/{}: current={:?}, replay={replay:?}, authoritative_root={}",
                self.block.height,
                self.block.block_hash,
                self.current,
                self.authoritative_root
            );
        }
        Ok(())
    }

    pub(super) fn validate_marker(&self, marker: FinalizedMarker) -> eyre::Result<()> {
        if !self.marker_matches_target(&marker) || !self.marker_matches_roots(&marker) {
            eyre::bail!(
                "CE MDBX returned conflicting finalized marker for {}/{}: {marker:?}",
                self.block.height,
                self.block.block_hash
            );
        }
        Ok(())
    }

    fn replay_matches_target(&self, replay: &CanonicalCeReplayBlock) -> bool {
        replay.number == self.block.height
            && replay.hash == self.block.block_hash
            && replay.parent_hash == self.block.parent_block_hash
            && replay.new_root == self.authoritative_root
    }

    fn replay_extends_current(&self, replay: &CanonicalCeReplayBlock) -> bool {
        replay.parent_hash == self.current.block_hash && replay.parent_root == self.current.new_root
    }

    fn marker_matches_target(&self, marker: &FinalizedMarker) -> bool {
        marker.commitment_scheme_version == ACTIVE_COMMITMENT_SCHEME
            && marker.height == self.block.height
            && marker.block_hash == self.block.block_hash
            && marker.parent_block_hash == self.block.parent_block_hash
    }

    fn marker_matches_roots(&self, marker: &FinalizedMarker) -> bool {
        marker.parent_root == self.current.new_root && marker.new_root == self.authoritative_root
    }
}
