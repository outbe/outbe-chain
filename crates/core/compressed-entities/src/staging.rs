//! Immutable speculative tree batches and exact-parent staging views.
//!
//! The types in this module deliberately model CKB store records instead of a
//! generic tree backend. Candidate writes are ordered, immutable once frozen,
//! and never reach the finalized MDBX environment through this API.

use std::{collections::BTreeMap, sync::Arc};

use alloy_primitives::B256;
use outbe_sparse_merkle_tree_v061::{
    error::Error as VendorError,
    merge::MergeValue as VendorMergeValue,
    traits::{StoreReadOps, StoreWriteOps},
    BranchKey as VendorBranchKey, BranchNode as VendorBranchNode, H256,
};
use thiserror::Error;

use crate::persistence::{
    BranchKey, BranchNode, ExactParentIdentity, FieldValue, FinalizedMarker, LeafValue, MergeValue,
    PersistenceError, TreeKey,
};

/// A staged store mutation. Deletes are represented only in candidate memory;
/// finalized MDBX applies them by removing the corresponding record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeChange<T> {
    Set(T),
    Delete,
}

/// A provisional candidate produced before the executor assigns the block
/// hash. It cannot be published until [`Self::freeze`] succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalTreeBatch {
    pub block_number: u64,
    pub parent_block_hash: B256,
    pub parent_root: B256,
    pub new_root: B256,
    pub branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
    pub leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
    pub encoded_size: usize,
}

impl ProvisionalTreeBatch {
    /// Constructs a batch and derives its canonical branch/leaf record size.
    pub fn new(
        block_number: u64,
        parent_block_hash: B256,
        parent_root: B256,
        new_root: B256,
        branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
        leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
    ) -> Result<Self, StagingError> {
        crate::persistence::validate_root(parent_root)?;
        crate::persistence::validate_root(new_root)?;
        let encoded_size = encoded_changes_size(&branch_changes, &leaf_changes)?;
        Ok(Self {
            block_number,
            parent_block_hash,
            parent_root,
            new_root,
            branch_changes,
            leaf_changes,
            encoded_size,
        })
    }

    /// Assigns the executor-produced block hash and freezes this candidate for
    /// publication. Metadata and maps are retained verbatim.
    #[must_use]
    pub fn freeze(self, block_hash: B256) -> StagedTreeBatch {
        StagedTreeBatch {
            block_number: self.block_number,
            block_hash,
            parent_block_hash: self.parent_block_hash,
            parent_root: self.parent_root,
            new_root: self.new_root,
            branch_changes: self.branch_changes,
            leaf_changes: self.leaf_changes,
            encoded_size: self.encoded_size,
        }
    }
}

/// An immutable, hash-addressed candidate ready for the finality coordinator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedTreeBatch {
    pub block_number: u64,
    pub block_hash: B256,
    pub parent_block_hash: B256,
    pub parent_root: B256,
    pub new_root: B256,
    pub branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
    pub leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
    pub encoded_size: usize,
}

impl StagedTreeBatch {
    /// Returns the marker which must be written after this batch's records in
    /// the same finalized MDBX transaction.
    #[must_use]
    pub const fn marker(&self, commitment_scheme_version: u32) -> FinalizedMarker {
        FinalizedMarker {
            commitment_scheme_version,
            height: self.block_number,
            block_hash: self.block_hash,
            parent_block_hash: self.parent_block_hash,
            parent_root: self.parent_root,
            new_root: self.new_root,
        }
    }

    /// Rechecks the cached size against the typed ordered maps.
    pub fn validate_encoded_size(&self) -> Result<(), StagingError> {
        let actual = encoded_changes_size(&self.branch_changes, &self.leaf_changes)?;
        if actual != self.encoded_size {
            return Err(StagingError::EncodedSizeMismatch {
                declared: self.encoded_size,
                actual,
            });
        }
        Ok(())
    }
}

fn encoded_changes_size(
    branches: &BTreeMap<BranchKey, TreeChange<BranchNode>>,
    leaves: &BTreeMap<TreeKey, TreeChange<LeafValue>>,
) -> Result<usize, StagingError> {
    let mut size = 0_usize;
    for (key, change) in branches {
        size = size
            .checked_add(key.encode().len())
            .ok_or(StagingError::EncodedSizeOverflow)?;
        if let TreeChange::Set(node) = change {
            size = size
                .checked_add(node.encode().len())
                .ok_or(StagingError::EncodedSizeOverflow)?;
        }
    }
    for (key, change) in leaves {
        size = size
            .checked_add(key.encode().len())
            .ok_or(StagingError::EncodedSizeOverflow)?;
        if let TreeChange::Set(value) = change {
            size = size
                .checked_add(value.encode().len())
                .ok_or(StagingError::EncodedSizeOverflow)?;
        }
    }
    Ok(size)
}

/// A consistent finalized read transaction used by one exact-parent view.
/// Implementations must return all reads from the same immutable snapshot.
pub trait FinalizedTreeSnapshot: Send {
    fn marker(&self) -> Result<FinalizedMarker, PersistenceError>;
    fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, PersistenceError>;
    fn read_leaf(&self, key: TreeKey) -> Result<Option<LeafValue>, PersistenceError>;
}

/// One exact finalized-parent view. Construction checks all identity fields,
/// including equality between the EVM-authoritative root and the MDBX marker.
pub struct AuthenticatedTreeView {
    snapshot: Box<dyn FinalizedTreeSnapshot>,
    identity: ExactParentIdentity,
}

impl std::fmt::Debug for AuthenticatedTreeView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedTreeView")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedTreeView {
    pub fn open(
        snapshot: Box<dyn FinalizedTreeSnapshot>,
        required: ExactParentIdentity,
    ) -> Result<Self, StagingError> {
        let marker = snapshot.marker()?;
        marker.verify_exact_parent(required)?;
        Ok(Self {
            snapshot,
            identity: required,
        })
    }

    #[must_use]
    pub const fn identity(&self) -> ExactParentIdentity {
        self.identity
    }

    pub fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, StagingError> {
        self.snapshot.read_branch(key).map_err(Into::into)
    }

    pub fn read_leaf(&self, key: TreeKey) -> Result<Option<LeafValue>, StagingError> {
        self.snapshot.read_leaf(key).map_err(Into::into)
    }
}

/// CKB-compatible speculative store semantics: candidate writes shadow the
/// immutable exact-parent snapshot and remain confined to ordered maps.
#[derive(Debug)]
pub struct StagingCkbStore {
    base: AuthenticatedTreeView,
    branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
    leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
}

impl StoreReadOps<H256> for StagingCkbStore {
    fn get_branch(
        &self,
        branch_key: &VendorBranchKey,
    ) -> Result<Option<VendorBranchNode>, VendorError> {
        let key = from_vendor_branch_key(branch_key).map_err(vendor_store_error)?;
        Ok(self
            .read_branch(key)
            .map_err(vendor_store_error)?
            .map(to_vendor_branch_node))
    }

    fn get_leaf(&self, leaf_key: &H256) -> Result<Option<H256>, VendorError> {
        let key = from_vendor_tree_key(*leaf_key).map_err(vendor_store_error)?;
        self.read_leaf(key)
            .map_err(vendor_store_error)
            .map(|value| value.map(|leaf| H256::from(leaf.encode())))
    }
}

impl StoreWriteOps<H256> for StagingCkbStore {
    fn insert_branch(
        &mut self,
        branch_key: VendorBranchKey,
        branch: VendorBranchNode,
    ) -> Result<(), VendorError> {
        let key = from_vendor_branch_key(&branch_key).map_err(vendor_store_error)?;
        let node = from_vendor_branch_node(&branch).map_err(vendor_store_error)?;
        self.write_branch(key, node);
        Ok(())
    }

    fn insert_leaf(&mut self, leaf_key: H256, leaf: H256) -> Result<(), VendorError> {
        let key = from_vendor_tree_key(leaf_key).map_err(vendor_store_error)?;
        let value =
            LeafValue::try_from(B256::from(<[u8; 32]>::from(leaf))).map_err(vendor_store_error)?;
        self.write_leaf(key, value);
        Ok(())
    }

    fn remove_branch(&mut self, branch_key: &VendorBranchKey) -> Result<(), VendorError> {
        let key = from_vendor_branch_key(branch_key).map_err(vendor_store_error)?;
        self.delete_branch(key);
        Ok(())
    }

    fn remove_leaf(&mut self, leaf_key: &H256) -> Result<(), VendorError> {
        let key = from_vendor_tree_key(*leaf_key).map_err(vendor_store_error)?;
        self.delete_leaf(key);
        Ok(())
    }
}

fn from_vendor_tree_key(key: H256) -> Result<TreeKey, PersistenceError> {
    TreeKey::try_from(B256::from(<[u8; 32]>::from(key)))
}

fn from_vendor_branch_key(key: &VendorBranchKey) -> Result<BranchKey, PersistenceError> {
    BranchKey::new(key.height, B256::from(<[u8; 32]>::from(key.node_key)))
}

fn to_vendor_branch_node(node: BranchNode) -> VendorBranchNode {
    VendorBranchNode {
        left: to_vendor_merge_value(node.left),
        right: to_vendor_merge_value(node.right),
    }
}

fn from_vendor_branch_node(node: &VendorBranchNode) -> Result<BranchNode, PersistenceError> {
    Ok(BranchNode {
        left: from_vendor_merge_value(&node.left)?,
        right: from_vendor_merge_value(&node.right)?,
    })
}

fn to_vendor_merge_value(value: MergeValue) -> VendorMergeValue {
    match value {
        MergeValue::Value(value) => VendorMergeValue::Value(H256::from(value.encode())),
        MergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => VendorMergeValue::MergeWithZero {
            base_node: H256::from(base_node.encode()),
            zero_bits: H256::from(zero_bits.encode()),
            zero_count,
        },
    }
}

fn from_vendor_merge_value(value: &VendorMergeValue) -> Result<MergeValue, PersistenceError> {
    match value {
        VendorMergeValue::Value(value) => {
            Ok(MergeValue::Value(FieldValue::try_from(B256::from(<[u8;
                32]>::from(
                *value,
            )))?))
        }
        VendorMergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => Ok(MergeValue::MergeWithZero {
            base_node: FieldValue::try_from(B256::from(<[u8; 32]>::from(*base_node)))?,
            zero_bits: FieldValue::try_from(B256::from(<[u8; 32]>::from(*zero_bits)))?,
            zero_count: *zero_count,
        }),
    }
}

fn vendor_store_error(error: impl std::fmt::Display) -> VendorError {
    VendorError::Store(error.to_string())
}

impl StagingCkbStore {
    #[must_use]
    pub fn new(base: AuthenticatedTreeView) -> Self {
        Self {
            base,
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::new(),
        }
    }

    pub fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, StagingError> {
        match self.branch_changes.get(&key) {
            Some(TreeChange::Set(node)) => Ok(Some(*node)),
            Some(TreeChange::Delete) => Ok(None),
            None => self.base.read_branch(key),
        }
    }

    pub fn write_branch(&mut self, key: BranchKey, node: BranchNode) {
        self.branch_changes.insert(key, TreeChange::Set(node));
    }

    pub fn delete_branch(&mut self, key: BranchKey) {
        self.branch_changes.insert(key, TreeChange::Delete);
    }

    pub fn read_leaf(&self, key: TreeKey) -> Result<Option<LeafValue>, StagingError> {
        match self.leaf_changes.get(&key) {
            Some(TreeChange::Set(value)) => Ok(Some(*value)),
            Some(TreeChange::Delete) => Ok(None),
            None => self.base.read_leaf(key),
        }
    }

    pub fn write_leaf(&mut self, key: TreeKey, value: LeafValue) {
        self.leaf_changes.insert(key, TreeChange::Set(value));
    }

    pub fn delete_leaf(&mut self, key: TreeKey) {
        self.leaf_changes.insert(key, TreeChange::Delete);
    }

    pub fn freeze(
        self,
        block_number: u64,
        parent_block_hash: B256,
        new_root: B256,
    ) -> Result<ProvisionalTreeBatch, StagingError> {
        let identity = self.base.identity();
        let expected_parent_number = block_number
            .checked_sub(1)
            .ok_or(StagingError::CandidateBlockNumberZero)?;
        if identity.block_number != expected_parent_number
            || identity.block_hash != parent_block_hash
        {
            return Err(StagingError::CandidateParentMismatch {
                block_number,
                parent_block_hash,
                view: identity,
            });
        }
        ProvisionalTreeBatch::new(
            block_number,
            parent_block_hash,
            identity.root,
            new_root,
            self.branch_changes,
            self.leaf_changes,
        )
    }
}

/// Benchmark-fixed local candidate cache bounds. No entry is implicitly
/// evicted, so a required pending-finalized candidate cannot disappear under
/// pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateCacheLimits {
    pub max_candidates: usize,
    pub max_encoded_bytes: usize,
}

#[derive(Debug)]
pub struct CandidateCache {
    limits: CandidateCacheLimits,
    encoded_bytes: usize,
    candidates: BTreeMap<B256, Arc<StagedTreeBatch>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationOutcome {
    Published,
    AlreadyPublished,
}

impl CandidateCache {
    #[must_use]
    pub fn new(limits: CandidateCacheLimits) -> Self {
        Self {
            limits,
            encoded_bytes: 0,
            candidates: BTreeMap::new(),
        }
    }

    pub fn publish(&mut self, batch: StagedTreeBatch) -> Result<PublicationOutcome, StagingError> {
        batch.validate_encoded_size()?;
        if let Some(existing) = self.candidates.get(&batch.block_hash) {
            if existing.as_ref() == &batch {
                return Ok(PublicationOutcome::AlreadyPublished);
            }
            return Err(StagingError::ConflictingPublication {
                block_hash: batch.block_hash,
            });
        }

        let candidate_count = self
            .candidates
            .len()
            .checked_add(1)
            .ok_or(StagingError::CacheCapacity)?;
        let encoded_bytes = self
            .encoded_bytes
            .checked_add(batch.encoded_size)
            .ok_or(StagingError::CacheCapacity)?;
        if candidate_count > self.limits.max_candidates
            || encoded_bytes > self.limits.max_encoded_bytes
        {
            return Err(StagingError::CacheCapacity);
        }

        self.encoded_bytes = encoded_bytes;
        self.candidates.insert(batch.block_hash, Arc::new(batch));
        Ok(PublicationOutcome::Published)
    }

    #[must_use]
    pub fn get(&self, block_hash: B256) -> Option<Arc<StagedTreeBatch>> {
        self.candidates.get(&block_hash).cloned()
    }

    /// Removes one rejected speculative payload without disturbing competing
    /// candidates. Open readers retain their immutable `Arc`.
    pub fn remove(&mut self, block_hash: B256) -> Option<Arc<StagedTreeBatch>> {
        let removed = self.candidates.remove(&block_hash);
        if let Some(batch) = &removed {
            self.encoded_bytes = self.encoded_bytes.saturating_sub(batch.encoded_size);
        }
        removed
    }

    /// Removes the winning entry and every losing candidate at or below the
    /// committed height. Open readers keep their own `Arc` immutable batch.
    pub fn remove_finalized(&mut self, height: u64) {
        let removed: Vec<_> = self
            .candidates
            .iter()
            .filter_map(|(hash, batch)| (batch.block_number <= height).then_some(*hash))
            .collect();
        for hash in removed {
            self.remove(hash);
        }
    }

    /// Restart deliberately discards all speculative candidates.
    pub fn discard_all_on_restart(&mut self) {
        self.candidates.clear();
        self.encoded_bytes = 0;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    #[must_use]
    pub const fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
}

#[derive(Debug, Error)]
pub enum StagingError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error("staged record size overflow")]
    EncodedSizeOverflow,
    #[error("staged encoded size mismatch: declared {declared}, actual {actual}")]
    EncodedSizeMismatch { declared: usize, actual: usize },
    #[error("candidate does not fit the configured speculative tree cache")]
    CacheCapacity,
    #[error("conflicting staged tree batch for block {block_hash}")]
    ConflictingPublication { block_hash: B256 },
    #[error(
        "candidate block {block_number} parent {parent_block_hash} does not match exact view {view:?}"
    )]
    CandidateParentMismatch {
        block_number: u64,
        parent_block_hash: B256,
        view: ExactParentIdentity,
    },
    #[error("candidate block number zero has no finalized execution parent")]
    CandidateBlockNumberZero,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{FieldValue, MergeValue};

    #[derive(Debug)]
    struct MemorySnapshot {
        marker: FinalizedMarker,
        branches: BTreeMap<BranchKey, BranchNode>,
        leaves: BTreeMap<TreeKey, LeafValue>,
    }

    impl FinalizedTreeSnapshot for MemorySnapshot {
        fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
            Ok(self.marker)
        }

        fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, PersistenceError> {
            Ok(self.branches.get(&key).copied())
        }

        fn read_leaf(&self, key: TreeKey) -> Result<Option<LeafValue>, PersistenceError> {
            Ok(self.leaves.get(&key).copied())
        }
    }

    fn b256(last: u8) -> B256 {
        let mut bytes = [0_u8; 32];
        bytes[31] = last;
        B256::from(bytes)
    }

    fn marker() -> FinalizedMarker {
        FinalizedMarker {
            commitment_scheme_version: 1,
            height: 7,
            block_hash: b256(7),
            parent_block_hash: b256(6),
            parent_root: b256(16),
            new_root: b256(17),
        }
    }

    fn exact() -> ExactParentIdentity {
        ExactParentIdentity {
            commitment_scheme_version: 1,
            block_number: 7,
            block_hash: b256(7),
            root: b256(17),
        }
    }

    fn leaf(last: u8) -> LeafValue {
        LeafValue::try_from(b256(last)).unwrap()
    }

    fn key(last: u8) -> TreeKey {
        TreeKey::try_from(b256(last)).unwrap()
    }

    fn node(last: u8) -> BranchNode {
        BranchNode {
            left: MergeValue::Value(FieldValue::try_from(b256(last)).unwrap()),
            right: MergeValue::Value(FieldValue::try_from(b256(last + 1)).unwrap()),
        }
    }

    fn view(snapshot: MemorySnapshot) -> AuthenticatedTreeView {
        AuthenticatedTreeView::open(Box::new(snapshot), exact()).unwrap()
    }

    #[test]
    fn exact_view_rejects_every_marker_identity_mismatch() {
        type MarkerMutation = Box<dyn Fn(&mut FinalizedMarker)>;

        let expected = exact();
        let mut mutations: Vec<MarkerMutation> = vec![
            Box::new(|value| value.commitment_scheme_version += 1),
            Box::new(|value| value.height += 1),
            Box::new(|value| value.block_hash = b256(99)),
            Box::new(|value| value.new_root = b256(99)),
        ];
        for mutate in &mut mutations {
            let mut wrong = marker();
            mutate(&mut wrong);
            let snapshot = MemorySnapshot {
                marker: wrong,
                branches: BTreeMap::new(),
                leaves: BTreeMap::new(),
            };
            assert!(AuthenticatedTreeView::open(Box::new(snapshot), expected).is_err());
        }
    }

    #[test]
    fn candidate_writes_shadow_base_without_mutating_snapshot() {
        let branch_key = BranchKey::new(9, b256(5)).unwrap();
        let leaf_key = key(6);
        let base_node = node(10);
        let base_leaf = leaf(12);
        let snapshot = MemorySnapshot {
            marker: marker(),
            branches: BTreeMap::from([(branch_key, base_node)]),
            leaves: BTreeMap::from([(leaf_key, base_leaf)]),
        };
        let mut store = StagingCkbStore::new(view(snapshot));

        assert_eq!(store.read_branch(branch_key).unwrap(), Some(base_node));
        assert_eq!(store.read_leaf(leaf_key).unwrap(), Some(base_leaf));
        store.write_branch(branch_key, node(20));
        store.delete_leaf(leaf_key);
        assert_eq!(store.read_branch(branch_key).unwrap(), Some(node(20)));
        assert_eq!(store.read_leaf(leaf_key).unwrap(), None);

        let batch = store.freeze(8, b256(7), b256(18)).unwrap();
        assert_eq!(batch.branch_changes.len(), 1);
        assert_eq!(batch.leaf_changes.len(), 1);
        assert_ne!(batch.encoded_size, 0);
    }

    #[test]
    fn ckb_store_traits_round_trip_typed_records_and_reject_invalid_leaf() {
        let snapshot = MemorySnapshot {
            marker: marker(),
            branches: BTreeMap::new(),
            leaves: BTreeMap::new(),
        };
        let mut store = StagingCkbStore::new(view(snapshot));
        let vendor_key = VendorBranchKey::new(12, H256::from(key(4).encode()));
        let vendor_node = VendorBranchNode {
            left: VendorMergeValue::Value(H256::from(leaf(5).encode())),
            right: VendorMergeValue::MergeWithZero {
                base_node: H256::from(leaf(6).encode()),
                zero_bits: H256::from(key(7).encode()),
                zero_count: 0,
            },
        };
        StoreWriteOps::insert_branch(&mut store, vendor_key.clone(), vendor_node.clone()).unwrap();
        assert_eq!(
            StoreReadOps::<H256>::get_branch(&store, &vendor_key).unwrap(),
            Some(vendor_node)
        );

        let leaf_key = H256::from(key(8).encode());
        let leaf_value = H256::from(leaf(9).encode());
        StoreWriteOps::insert_leaf(&mut store, leaf_key, leaf_value).unwrap();
        assert_eq!(
            StoreReadOps::<H256>::get_leaf(&store, &leaf_key).unwrap(),
            Some(leaf_value)
        );
        assert!(StoreWriteOps::insert_leaf(&mut store, leaf_key, H256::zero()).is_err());
        assert!(
            StoreWriteOps::insert_leaf(&mut store, H256::from([u8::MAX; 32]), leaf_value,).is_err()
        );
    }

    #[test]
    fn publication_is_structurally_idempotent_and_never_evicts() {
        let provisional = ProvisionalTreeBatch::new(
            8,
            b256(7),
            b256(17),
            b256(18),
            BTreeMap::new(),
            BTreeMap::from([(key(1), TreeChange::Set(leaf(2)))]),
        )
        .unwrap();
        let batch = provisional.freeze(b256(8));
        let mut cache = CandidateCache::new(CandidateCacheLimits {
            max_candidates: 1,
            max_encoded_bytes: batch.encoded_size,
        });
        assert_eq!(
            cache.publish(batch.clone()).unwrap(),
            PublicationOutcome::Published
        );
        assert_eq!(
            cache.publish(batch.clone()).unwrap(),
            PublicationOutcome::AlreadyPublished
        );

        let competing = ProvisionalTreeBatch::new(
            8,
            b256(7),
            b256(17),
            b256(19),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .unwrap()
        .freeze(b256(9));
        assert!(matches!(
            cache.publish(competing),
            Err(StagingError::CacheCapacity)
        ));
        assert_eq!(cache.get(b256(8)).unwrap().as_ref(), &batch);
    }

    #[test]
    fn same_hash_with_different_typed_batch_is_corruption() {
        let first = ProvisionalTreeBatch::new(
            8,
            b256(7),
            b256(17),
            b256(18),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .unwrap()
        .freeze(b256(8));
        let mut conflicting = first.clone();
        conflicting.new_root = b256(19);
        let mut cache = CandidateCache::new(CandidateCacheLimits {
            max_candidates: 2,
            max_encoded_bytes: 1_000,
        });
        cache.publish(first).unwrap();
        assert!(matches!(
            cache.publish(conflicting),
            Err(StagingError::ConflictingPublication { .. })
        ));
    }

    #[test]
    fn finalization_and_restart_remove_only_by_explicit_policy() {
        let mut cache = CandidateCache::new(CandidateCacheLimits {
            max_candidates: 4,
            max_encoded_bytes: 1_000,
        });
        for (height, hash) in [(8, 8), (8, 9), (9, 10)] {
            cache
                .publish(
                    ProvisionalTreeBatch::new(
                        height,
                        b256(7),
                        b256(17),
                        b256(18),
                        BTreeMap::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                    .freeze(b256(hash)),
                )
                .unwrap();
        }
        cache.remove_finalized(8);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(b256(10)).is_some());
        cache.discard_all_on_restart();
        assert!(cache.is_empty());
        assert_eq!(cache.encoded_bytes(), 0);
    }
}
