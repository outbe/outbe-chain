//! Production ownership and finalization boundary for the compressed-entity tree.
//!
//! The manager deliberately keeps speculative candidates in memory while the
//! sole finalized tree lives in the CE-owned MDBX environment. Opening a
//! parent view takes one immutable MDBX read transaction; finalization applies
//! one exact candidate atomically before advancing retention and removing
//! cache entries.

use std::sync::{Arc, Mutex};

use alloy_primitives::B256;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::{
    api::{AuthenticatedParentTree, AuthenticatedParentTreeFactory},
    persistence::{
        ApplyOutcome, CeMdbx, CeRetentionCursor, ExactParentIdentity, FinalizedMarker,
        PersistenceError,
    },
    staging::{
        CandidateCache, CandidateCacheLimits, ProvisionalTreeBatch, PublicationOutcome,
        StagedTreeBatch, StagingError,
    },
    MdbxAuthenticatedTree,
};

/// Result of applying an exact finalized block to the CE materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizedCandidateOutcome {
    Applied(FinalizedMarker),
    AlreadyApplied(FinalizedMarker),
}

impl FinalizedCandidateOutcome {
    #[must_use]
    pub const fn marker(self) -> FinalizedMarker {
        match self {
            Self::Applied(marker) | Self::AlreadyApplied(marker) => marker,
        }
    }
}

/// Explicitly-owned production tree service. There are no process globals and
/// no implicit cache limits: callers must supply benchmark-derived bounds.
#[derive(Debug)]
pub struct CompressedTreeService {
    db: Arc<CeMdbx>,
    candidates: Mutex<CandidateCache>,
    retention: CeRetentionCursor,
    finalization: Mutex<()>,
}

impl CompressedTreeService {
    /// Takes ownership of the CE MDBX environment and seeds retention from its
    /// already-verified finalized marker.
    pub fn new(db: CeMdbx, limits: CandidateCacheLimits) -> Result<Self, TreeServiceError> {
        let marker = db.marker()?;
        Ok(Self {
            db: Arc::new(db),
            candidates: Mutex::new(CandidateCache::new(limits)),
            retention: CeRetentionCursor::from_verified_marker(marker),
            finalization: Mutex::new(()),
        })
    }

    /// Opens one exact-parent tree session over one immutable MDBX snapshot.
    /// Every marker field is checked against `identity` before the session is
    /// returned.
    pub fn open_parent(
        &self,
        identity: ExactParentIdentity,
    ) -> Result<Arc<dyn AuthenticatedParentTree>, TreeServiceError> {
        let tree = MdbxAuthenticatedTree::open(Arc::clone(&self.db), identity)
            .map_err(TreeServiceError::ParentView)?;
        Ok(Arc::new(tree))
    }

    /// Freezes and publishes a provisional batch under the executor-assigned
    /// block hash. Repeating the identical publication is a successful no-op.
    pub fn publish_candidate(
        &self,
        block_hash: B256,
        provisional: ProvisionalTreeBatch,
    ) -> Result<PublicationOutcome, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;
        let current = self.db.marker()?;
        let batch = provisional.freeze(block_hash);
        if current.height == batch.block_number
            && current.block_hash == batch.block_hash
            && current.parent_block_hash == batch.parent_block_hash
            && current.parent_root == batch.parent_root()
            && current.new_root == batch.new_root()
        {
            return Ok(PublicationOutcome::AlreadyPublished);
        }
        if batch.block_number != current.height.saturating_add(1)
            || batch.parent_block_hash != current.block_hash
            || batch.parent_root() != current.new_root
        {
            return Err(TreeServiceError::NonContiguousPublication {
                current_marker: current,
                candidate_height: batch.block_number,
                block_hash,
                parent_block_hash: batch.parent_block_hash,
                parent_root: batch.parent_root(),
            });
        }
        self.candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .publish(batch)
            .map_err(Into::into)
    }

    /// Fetches an immutable exact candidate. A matching hash at another height
    /// is rejected instead of being silently reinterpreted.
    pub fn candidate(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<Option<Arc<StagedTreeBatch>>, TreeServiceError> {
        let candidate = self
            .candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .get(block_hash);
        match candidate {
            Some(candidate) if candidate.block_number != block_number => {
                Err(TreeServiceError::CandidateIdentityMismatch {
                    requested_height: block_number,
                    block_hash,
                    candidate_height: candidate.block_number,
                })
            }
            candidate => Ok(candidate),
        }
    }

    /// Drops one proposer candidate after a later payload guard rejects the
    /// assembled block. Finalization and removal share one serialization lock.
    pub fn discard_candidate(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<bool, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;
        let mut candidates = self
            .candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?;
        if let Some(candidate) = candidates.get(block_hash) {
            if candidate.block_number != block_number {
                return Err(TreeServiceError::CandidateIdentityMismatch {
                    requested_height: block_number,
                    block_hash,
                    candidate_height: candidate.block_number,
                });
            }
        }
        Ok(candidates.remove(block_hash).is_some())
    }

    /// Applies the exact candidate and only then advances retention and removes
    /// the winning/losing candidates at or below its height. Repeating a known
    /// completed finalization is idempotent even after cache removal.
    pub fn apply_finalized(
        &self,
        block_number: u64,
        block_hash: B256,
        authoritative_root: B256,
    ) -> Result<FinalizedCandidateOutcome, TreeServiceError> {
        let _guard = self
            .finalization
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("finalization boundary"))?;

        let Some(candidate) = self.candidate(block_number, block_hash)? else {
            let marker = self.db.marker()?;
            if marker.height == block_number
                && marker.block_hash == block_hash
                && marker.new_root == authoritative_root
            {
                self.retention
                    .advance_or_confirm_after_known_commit(marker)?;
                self.remove_finalized_candidates(block_number)?;
                return Ok(FinalizedCandidateOutcome::AlreadyApplied(marker));
            }
            return Err(TreeServiceError::CandidateMissing {
                block_number,
                block_hash,
                current_marker: marker,
            });
        };

        if candidate.new_root() != authoritative_root {
            return Err(TreeServiceError::AuthoritativeRootMismatch {
                block_number,
                block_hash,
                candidate_root: candidate.new_root(),
                authoritative_root,
            });
        }

        let outcome = self.db.apply_finalized(&candidate)?;
        let finalized = match outcome {
            ApplyOutcome::Applied(marker) => FinalizedCandidateOutcome::Applied(marker),
            ApplyOutcome::AlreadyApplied(marker) => {
                FinalizedCandidateOutcome::AlreadyApplied(marker)
            }
        };

        // The cursor cannot move until MDBX has returned a known-successful
        // outcome. Cache removal is deliberately last.
        self.retention
            .advance_or_confirm_after_known_commit(finalized.marker())?;
        self.remove_finalized_candidates(block_number)?;
        Ok(finalized)
    }

    /// Explicit, idempotent cache cleanup policy used after a successful
    /// finalized apply.
    pub fn remove_finalized_candidates(&self, height: u64) -> Result<(), TreeServiceError> {
        self.candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .remove_finalized(height);
        Ok(())
    }

    /// Restart never attempts to resurrect speculative state.
    pub fn discard_speculative_candidates(&self) -> Result<(), TreeServiceError> {
        self.candidates
            .lock()
            .map_err(|_| TreeServiceError::LockPoisoned("candidate cache"))?
            .discard_all_on_restart();
        Ok(())
    }

    pub fn finalized_marker(&self) -> Result<FinalizedMarker, TreeServiceError> {
        self.db.marker().map_err(Into::into)
    }

    #[must_use]
    pub fn retention_height(&self) -> u64 {
        self.retention.height()
    }
}

impl AuthenticatedParentTreeFactory for CompressedTreeService {
    fn open_parent(
        &self,
        identity: ExactParentIdentity,
    ) -> outbe_primitives::error::Result<Arc<dyn AuthenticatedParentTree>> {
        CompressedTreeService::open_parent(self, identity).map_err(|error| match error {
            TreeServiceError::ParentView(error) => error,
            other => PrecompileError::Fatal(other.to_string()),
        })
    }
}

#[derive(Debug, Error)]
pub enum TreeServiceError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Staging(#[from] StagingError),
    #[error("candidate shard count mismatch: expected {expected}, got {actual}")]
    ShardCountMismatch { expected: u32, actual: u32 },
    #[error("unable to open exact compressed-tree parent: {0}")]
    ParentView(PrecompileError),
    #[error("compressed-tree {0} lock is poisoned")]
    LockPoisoned(&'static str),
    #[error(
        "candidate {block_hash} requested at height {requested_height}, stored at {candidate_height}"
    )]
    CandidateIdentityMismatch {
        requested_height: u64,
        block_hash: B256,
        candidate_height: u64,
    },
    #[error(
        "candidate for finalized block {block_number}/{block_hash} is missing; current marker is {current_marker:?}"
    )]
    CandidateMissing {
        block_number: u64,
        block_hash: B256,
        current_marker: FinalizedMarker,
    },
    #[error(
        "candidate {candidate_height}/{block_hash} does not extend current marker {current_marker:?}: parent {parent_block_hash}/{parent_root}"
    )]
    NonContiguousPublication {
        current_marker: FinalizedMarker,
        candidate_height: u64,
        block_hash: B256,
        parent_block_hash: B256,
        parent_root: B256,
    },
    #[error(
        "candidate {block_number}/{block_hash} root {candidate_root} differs from authoritative EVM root {authoritative_root}"
    )]
    AuthoritativeRootMismatch {
        block_number: u64,
        block_hash: B256,
        candidate_root: B256,
        authoritative_root: B256,
    },
}

// ADR-009's flat-namespace fixtures are replaced by ADR-010 catalog fixtures below.
#[cfg(test)]
mod tests {
    use super::*;
    use outbe_common::WorldwideDay;

    use crate::{
        persistence::{EnvironmentIdentity, LOCAL_STORAGE_SCHEMA_VERSION},
        sealed_root, CeTopologyV1, Commitment, EntityId36, EntityRef, FinalLeafMutation,
        ACTIVE_COMMITMENT_SCHEME, K_PROVISIONAL,
    };

    fn b256(last: u8) -> B256 {
        let mut bytes = [0_u8; 32];
        bytes[31] = last;
        B256::from(bytes)
    }

    fn environment() -> EnvironmentIdentity {
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 8080,
            genesis_hash: b256(1),
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        }
    }

    fn genesis() -> FinalizedMarker {
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: environment().genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: sealed_root(B256::ZERO).unwrap(),
        }
    }

    fn service(directory: &std::path::Path) -> CompressedTreeService {
        let db = CeMdbx::open(directory, environment(), genesis()).unwrap();
        // These are test fixture bounds, not production defaults.
        CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 4,
                max_encoded_bytes: 1_000_000,
            },
        )
        .unwrap()
    }

    fn genesis_identity() -> ExactParentIdentity {
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis().block_hash,
            root: genesis().new_root,
        }
    }

    #[test]
    fn exact_parent_identity_is_checked_before_any_tree_read() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        assert!(service.open_parent(genesis_identity()).is_ok());

        for wrong in [
            ExactParentIdentity {
                block_hash: b256(99),
                ..genesis_identity()
            },
            ExactParentIdentity {
                root: b256(99),
                ..genesis_identity()
            },
            ExactParentIdentity {
                block_number: 9,
                ..genesis_identity()
            },
        ] {
            assert!(matches!(
                service.open_parent(wrong),
                Err(TreeServiceError::ParentView(_))
            ));
        }
    }

    #[test]
    fn exact_parent_identity_and_read_root_mismatches_are_corruption_not_readiness() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let wrong = ExactParentIdentity {
            block_hash: b256(99),
            ..genesis_identity()
        };

        let factory: &dyn AuthenticatedParentTreeFactory = &service;
        assert!(matches!(
            factory.open_parent(wrong),
            Err(PrecompileError::Fatal(_))
        ));

        let parent = factory.open_parent(genesis_identity()).unwrap();
        let entity = EntityRef::Tribute(EntityId36::new(WorldwideDay::new(7), [3_u8; 32]));
        assert!(matches!(
            parent.read_leaf_verified(entity, b256(88)),
            Err(PrecompileError::Fatal(_))
        ));
    }

    #[test]
    fn parent_proof_candidate_and_finalized_reopen_form_one_authenticated_flow() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let parent = service.open_parent(genesis_identity()).unwrap();
        let entity = EntityRef::Tribute(EntityId36::new(WorldwideDay::new(7), [3_u8; 32]));
        let commitment = Commitment::try_from(b256(17).0).unwrap();

        assert_eq!(
            parent
                .read_leaf_verified(entity, genesis_identity().root)
                .unwrap(),
            None
        );
        let provisional = parent
            .prepare_seal(
                1,
                &[FinalLeafMutation {
                    entity,
                    final_leaf: Some(commitment),
                }],
                &[],
            )
            .unwrap();
        let block_hash = b256(2);
        assert_eq!(
            service
                .publish_candidate(block_hash, provisional.clone())
                .unwrap(),
            PublicationOutcome::Published
        );
        assert_eq!(
            service.publish_candidate(block_hash, provisional).unwrap(),
            PublicationOutcome::AlreadyPublished
        );

        let staged = service.candidate(1, block_hash).unwrap().unwrap();
        assert_eq!(staged.parent_root(), genesis().new_root);
        let new_root = staged.new_root();
        assert_eq!(
            service.apply_finalized(1, block_hash, new_root).unwrap(),
            FinalizedCandidateOutcome::Applied(staged.marker(ACTIVE_COMMITMENT_SCHEME))
        );
        assert_eq!(service.retention_height(), 1);
        assert!(service.candidate(1, block_hash).unwrap().is_none());

        let reopened = service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 1,
                block_hash,
                root: new_root,
            })
            .unwrap();
        assert_eq!(
            reopened.read_leaf_verified(entity, new_root).unwrap(),
            Some(commitment)
        );
    }

    #[test]
    fn one_candidate_atomically_seals_and_reopens_changes_from_multiple_shards() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let identity = EntityId36::try_from(
            hex::decode("00000001000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        let tribute = EntityRef::Tribute(identity);
        let bucket = EntityRef::NodBucket(identity);
        let tribute_leaf = Commitment::try_from(b256(11).0).unwrap();
        let bucket_leaf = Commitment::try_from(b256(12).0).unwrap();

        let parent = service.open_parent(genesis_identity()).unwrap();
        let provisional = parent
            .prepare_seal(
                1,
                &[
                    FinalLeafMutation {
                        entity: tribute,
                        final_leaf: Some(tribute_leaf),
                    },
                    FinalLeafMutation {
                        entity: bucket,
                        final_leaf: Some(bucket_leaf),
                    },
                ],
                &[],
            )
            .unwrap();
        assert_eq!(provisional.changed_shard_count(), 2);
        assert_eq!(provisional.changed_collections.len(), 2);
        assert!(provisional.changed_collections.values().all(|collection| {
            let collection = collection.mutation().expect("mutation operation");
            collection.shard_set.parent_shard_roots.len() == K_PROVISIONAL as usize
                && collection.shard_set.new_shard_roots.len() == K_PROVISIONAL as usize
        }));

        let block_hash = b256(91);
        let new_root = provisional.new_root();
        service.publish_candidate(block_hash, provisional).unwrap();
        service.apply_finalized(1, block_hash, new_root).unwrap();

        let reopened = service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 1,
                block_hash,
                root: new_root,
            })
            .unwrap();
        assert_eq!(
            reopened.read_leaf_verified(tribute, new_root).unwrap(),
            Some(tribute_leaf)
        );
        assert_eq!(
            reopened.read_leaf_verified(bucket, new_root).unwrap(),
            Some(bucket_leaf)
        );
    }

    #[test]
    fn completed_finalization_is_idempotent_after_candidate_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let parent = service.open_parent(genesis_identity()).unwrap();
        let provisional = parent.prepare_seal(1, &[], &[]).unwrap();
        let block_hash = b256(2);
        service.publish_candidate(block_hash, provisional).unwrap();

        let root = service
            .candidate(1, block_hash)
            .unwrap()
            .unwrap()
            .new_root();

        assert!(matches!(
            service.apply_finalized(1, block_hash, root).unwrap(),
            FinalizedCandidateOutcome::Applied(_)
        ));
        assert!(matches!(
            service.apply_finalized(1, block_hash, root).unwrap(),
            FinalizedCandidateOutcome::AlreadyApplied(_)
        ));
        assert_eq!(service.retention_height(), 1);
        assert!(matches!(
            service.apply_finalized(2, b256(3), b256(4)),
            Err(TreeServiceError::CandidateMissing { .. })
        ));
    }

    #[test]
    fn authoritative_root_mismatch_cannot_mutate_mdbx_or_drop_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let parent = service.open_parent(genesis_identity()).unwrap();
        let provisional = parent.prepare_seal(1, &[], &[]).unwrap();
        let block_hash = b256(2);
        service.publish_candidate(block_hash, provisional).unwrap();

        assert!(matches!(
            service.apply_finalized(1, block_hash, b256(77)),
            Err(TreeServiceError::AuthoritativeRootMismatch { .. })
        ));
        assert_eq!(service.finalized_marker().unwrap(), genesis());
        assert_eq!(service.retention_height(), 0);
        assert!(service.candidate(1, block_hash).unwrap().is_some());
    }

    #[test]
    fn late_payload_rejection_discards_only_the_exact_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let first_hash = b256(2);
        let competing_hash = b256(3);
        for hash in [first_hash, competing_hash] {
            let provisional = service
                .open_parent(genesis_identity())
                .unwrap()
                .prepare_seal(1, &[], &[])
                .unwrap();
            service.publish_candidate(hash, provisional).unwrap();
        }

        assert!(service.discard_candidate(1, first_hash).unwrap());
        assert!(service.candidate(1, first_hash).unwrap().is_none());
        assert!(service.candidate(1, competing_hash).unwrap().is_some());
        assert_eq!(service.finalized_marker().unwrap(), genesis());
    }

    #[test]
    fn stale_or_wrong_parent_candidate_is_rejected_before_cache_publication() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let mut wrong_parent = service
            .open_parent(genesis_identity())
            .unwrap()
            .prepare_seal(1, &[], &[])
            .unwrap();
        wrong_parent.parent_block_hash = b256(44);
        assert!(matches!(
            service.publish_candidate(b256(2), wrong_parent),
            Err(TreeServiceError::NonContiguousPublication { .. })
        ));
        assert!(service.candidate(1, b256(2)).unwrap().is_none());
    }
}
