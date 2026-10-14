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
            return Err(crate::errors::CeFinalizerError::RedeliveredRootConflict {
                height: block.height,
                block_hash: block.block_hash,
                authoritative_root,
                marker_root: current.new_root,
            }
            .into());
        }
        self.validate_redelivery_parent(block, current)
    }

    async fn redelivery_root(&self, block: FinalizedCeBlock) -> eyre::Result<B256> {
        let authoritative_root = match self.persistence.verify_durable(block)? {
            Some(root) => root,
            None => {
                self.persistence.wait_for_exact_persistence(block).await?;
                self.persistence.verify_durable(block)?.ok_or(
                    crate::errors::CeFinalizerError::RedeliveredDbStateAbsent {
                        height: block.height,
                        block_hash: block.block_hash,
                    },
                )?
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
            crate::errors::CeFinalizerError::DurableParentAbsentRedelivery {
                height: block.height,
                block_hash: block.block_hash,
            }
        })?;
        if current.parent_block_hash != durable_parent.block_hash
            || current.parent_root != durable_parent_root
        {
            return Err(crate::errors::CeFinalizerError::RedeliveredParentConflict {
                height: block.height,
                block_hash: block.block_hash,
                reth_hash: durable_parent.block_hash,
                reth_root: durable_parent_root,
                marker_hash: current.parent_block_hash,
                marker_root: current.parent_root,
            }
            .into());
        }
        Ok(())
    }

    pub(super) async fn new_delivery_root(&self, block: FinalizedCeBlock) -> eyre::Result<B256> {
        let authoritative_root = match self.persistence.probe_durable(block)? {
            DurableCeProbe::Exact(root) => root,
            DurableCeProbe::Absent | DurableCeProbe::DifferentHash(_) => {
                self.persistence.wait_for_exact_persistence(block).await?;
                self.persistence.verify_durable(block)?.ok_or(
                    crate::errors::CeFinalizerError::DbStateAbsent {
                        height: block.height,
                        block_hash: block.block_hash,
                    },
                )?
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
            crate::errors::CeFinalizerError::DurableParentAbsent {
                height: block.height,
                block_hash: block.block_hash,
            }
        })?;
        if durable_parent.block_hash != block.parent_block_hash
            || durable_parent.block_hash != current.block_hash
            || durable_parent_root != current.new_root
        {
            return Err(
                crate::errors::CeFinalizerError::DurableParentHeaderConflict {
                    height: block.height,
                    block_hash: block.block_hash,
                    actor_hash: block.parent_block_hash,
                    actor_root: durable_parent_root,
                    marker_hash: current.block_hash,
                    marker_root: current.new_root,
                }
                .into(),
            );
        }

        Ok(())
    }

    fn read_durable_parent(
        &self,
        block: FinalizedCeBlock,
        missing_error: impl FnOnce() -> crate::errors::CeFinalizerError,
    ) -> eyre::Result<(super::DurableCeEvidence, B256)> {
        let parent_height = block.height.saturating_sub(1);
        let parent = self
            .persistence
            .state
            .block_and_root(parent_height)?
            .ok_or_else(missing_error)?;
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
            let replay = self.persistence.state.replay_block(block.height)?.ok_or(
                crate::errors::CeFinalizerError::DurableCanonicalReplayMissing {
                    height: block.height,
                    block_hash: block.block_hash,
                },
            )?;
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
        return Err(crate::errors::CeFinalizerError::RedeliveredMarkerConflict {
            height: block.height,
            block_hash: block.block_hash,
            current,
        }
        .into());
    }
    Ok(())
}

impl FinalizedCommit {
    fn validate_candidate(&self, candidate: &StagedTreeBatch) -> eyre::Result<()> {
        if candidate.parent_block_hash() != self.block.parent_block_hash {
            return Err(crate::errors::CeFinalizerError::CandidateParentConflict {
                height: self.block.height,
                block_hash: self.block.block_hash,
                actor: self.block.parent_block_hash,
                candidate: candidate.parent_block_hash(),
            }
            .into());
        }
        if candidate.new_root() != self.authoritative_root {
            return Err(crate::errors::CeFinalizerError::CandidateRootConflict {
                height: self.block.height,
                block_hash: self.block.block_hash,
                evm: self.authoritative_root,
                candidate: candidate.new_root(),
            }
            .into());
        }
        Ok(())
    }

    fn validate_replay(&self, replay: &CanonicalCeReplayBlock) -> eyre::Result<()> {
        if !self.replay_matches_target(replay) || !self.replay_extends_current(replay) {
            return Err(
                crate::errors::CeFinalizerError::ReplayIdentityRootConflict {
                    height: self.block.height,
                    block_hash: self.block.block_hash,
                    current: self.current,
                    replay: format!("{replay:?}"),
                    authoritative_root: self.authoritative_root,
                }
                .into(),
            );
        }
        Ok(())
    }

    pub(super) fn validate_marker(&self, marker: FinalizedMarker) -> eyre::Result<()> {
        if !self.marker_matches_target(&marker) || !self.marker_matches_roots(&marker) {
            return Err(crate::errors::CeFinalizerError::MdbxConflictingMarker {
                height: self.block.height,
                block_hash: self.block.block_hash,
                marker,
            }
            .into());
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
