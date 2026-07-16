//! Immutable speculative tree batches and exact-parent staging views.
//!
//! The types in this module deliberately model CKB store records instead of a
//! generic tree backend. Candidate writes are ordered, immutable once frozen,
//! and never reach the finalized MDBX environment through this API.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

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
use crate::{
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::TreeKey as SmtTreeKey,
};

pub type ShardIndex = u32;

macro_rules! batch_identity_accessors {
    () => {
        #[must_use]
        pub const fn block_number(&self) -> u64 {
            self.block_number
        }
        #[must_use]
        pub const fn parent_block_hash(&self) -> B256 {
            self.parent_block_hash
        }
        #[must_use]
        pub const fn parent_root(&self) -> B256 {
            self.parent_root
        }
        #[must_use]
        pub const fn new_root(&self) -> B256 {
            self.new_root
        }
        #[must_use]
        pub const fn shard_count(&self) -> u32 {
            self.shard_count
        }
        #[must_use]
        pub fn parent_shard_roots(&self) -> &[B256] {
            &self.parent_shard_roots
        }
        #[must_use]
        pub fn new_shard_roots(&self) -> &[B256] {
            &self.new_shard_roots
        }
        #[must_use]
        pub fn changed_shard_count(&self) -> usize {
            self.changed_shards.len()
        }
        #[must_use]
        pub fn branch_change_count(&self) -> usize {
            self.changed_shards
                .values()
                .map(ProvisionalShardBatch::branch_change_count)
                .sum()
        }
        #[must_use]
        pub fn leaf_change_count(&self) -> usize {
            self.changed_shards
                .values()
                .map(ProvisionalShardBatch::leaf_change_count)
                .sum()
        }
        #[must_use]
        pub const fn encoded_size(&self) -> usize {
            self.encoded_size
        }
    };
}

/// A staged store mutation. Deletes are represented only in candidate memory;
/// finalized MDBX applies them by removing the corresponding record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeChange<T> {
    Set(T),
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalShardBatch {
    pub(crate) parent_shard_root: B256,
    pub(crate) new_shard_root: B256,
    pub(crate) branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
    pub(crate) leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
}

impl ProvisionalShardBatch {
    pub(crate) fn new(
        parent_shard_root: B256,
        new_shard_root: B256,
        branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
        leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
    ) -> Result<Self, StagingError> {
        crate::persistence::validate_root(parent_shard_root)?;
        crate::persistence::validate_root(new_shard_root)?;
        if parent_shard_root == new_shard_root {
            return Err(StagingError::InvalidShardEnvelope(
                "changed shard roots must differ",
            ));
        }
        if branch_changes.is_empty() && leaf_changes.is_empty() {
            return Err(StagingError::InvalidShardEnvelope(
                "changed shard must contain effective records",
            ));
        }
        Ok(Self {
            parent_shard_root,
            new_shard_root,
            branch_changes,
            leaf_changes,
        })
    }

    #[must_use]
    pub fn branch_change_count(&self) -> usize {
        self.branch_changes.len()
    }

    #[must_use]
    pub fn leaf_change_count(&self) -> usize {
        self.leaf_changes.len()
    }
}

/// A provisional shard-set candidate produced before the executor assigns the
/// block hash. It cannot be published until [`Self::freeze`] succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalTreeBatch {
    pub(crate) block_number: u64,
    pub(crate) parent_block_hash: B256,
    pub(crate) parent_root: B256,
    pub(crate) new_root: B256,
    pub(crate) shard_count: u32,
    pub(crate) parent_shard_roots: Vec<B256>,
    pub(crate) new_shard_roots: Vec<B256>,
    pub(crate) changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    pub(crate) encoded_size: usize,
}

impl ProvisionalTreeBatch {
    /// ADR-008 control constructor retained for `K = 1` benchmark and test
    /// comparisons. Production ADR-009 construction uses [`Self::new`].
    pub fn new_unsharded(
        block_number: u64,
        parent_block_hash: B256,
        parent_root: B256,
        new_root: B256,
        branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
        leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
    ) -> Result<Self, StagingError> {
        let changed_shards = if parent_root == new_root {
            if !branch_changes.is_empty() || !leaf_changes.is_empty() {
                return Err(StagingError::InvalidShardEnvelope(
                    "net-no-op shard contains records",
                ));
            }
            BTreeMap::new()
        } else {
            BTreeMap::from([(
                0,
                ProvisionalShardBatch::new(parent_root, new_root, branch_changes, leaf_changes)?,
            )])
        };
        Self::new(
            block_number,
            parent_block_hash,
            parent_root,
            new_root,
            1,
            vec![parent_root],
            vec![new_root],
            changed_shards,
        )
    }

    /// Constructs a batch and derives its canonical branch/leaf record size.
    #[allow(clippy::too_many_arguments)] // Mirrors the ADR-009 shard-set envelope fields.
    pub(crate) fn new(
        block_number: u64,
        parent_block_hash: B256,
        parent_root: B256,
        new_root: B256,
        shard_count: u32,
        parent_shard_roots: Vec<B256>,
        new_shard_roots: Vec<B256>,
        changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    ) -> Result<Self, StagingError> {
        crate::persistence::validate_root(parent_root)?;
        crate::persistence::validate_root(new_root)?;
        validate_shard_envelope(
            shard_count,
            parent_root,
            new_root,
            &parent_shard_roots,
            &new_shard_roots,
            &changed_shards,
        )?;
        let encoded_size = encoded_shard_set_size(
            shard_count,
            &parent_shard_roots,
            &new_shard_roots,
            &changed_shards,
        )?;
        Ok(Self {
            block_number,
            parent_block_hash,
            parent_root,
            new_root,
            shard_count,
            parent_shard_roots,
            new_shard_roots,
            changed_shards,
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
            shard_count: self.shard_count,
            parent_shard_roots: self.parent_shard_roots,
            new_shard_roots: self.new_shard_roots,
            changed_shards: self.changed_shards,
            encoded_size: self.encoded_size,
        }
    }

    batch_identity_accessors!();
}

/// An immutable, hash-addressed candidate ready for the finality coordinator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedTreeBatch {
    pub(crate) block_number: u64,
    pub(crate) block_hash: B256,
    pub(crate) parent_block_hash: B256,
    pub(crate) parent_root: B256,
    pub(crate) new_root: B256,
    pub(crate) shard_count: u32,
    pub(crate) parent_shard_roots: Vec<B256>,
    pub(crate) new_shard_roots: Vec<B256>,
    pub(crate) changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    pub(crate) encoded_size: usize,
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
        validate_shard_envelope(
            self.shard_count,
            self.parent_root,
            self.new_root,
            &self.parent_shard_roots,
            &self.new_shard_roots,
            &self.changed_shards,
        )?;
        let actual = encoded_shard_set_size(
            self.shard_count,
            &self.parent_shard_roots,
            &self.new_shard_roots,
            &self.changed_shards,
        )?;
        if actual != self.encoded_size {
            return Err(StagingError::EncodedSizeMismatch {
                declared: self.encoded_size,
                actual,
            });
        }
        Ok(())
    }

    pub(crate) fn canonical_bytes(&self) -> Result<Vec<u8>, StagingError> {
        let encoded_size =
            u64::try_from(self.encoded_size).map_err(|_| StagingError::EncodedSizeOverflow)?;
        let mut bytes = Vec::with_capacity(self.encoded_size);
        visit_canonical_batch(
            CanonicalBatch {
                block_number: self.block_number,
                block_hash: self.block_hash,
                parent_block_hash: self.parent_block_hash,
                parent_root: self.parent_root,
                new_root: self.new_root,
                shard_count: self.shard_count,
                parent_roots: &self.parent_shard_roots,
                new_roots: &self.new_shard_roots,
                changed_shards: &self.changed_shards,
                encoded_size,
            },
            |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            },
        )?;
        if bytes.len() != self.encoded_size {
            return Err(StagingError::EncodedSizeMismatch {
                declared: self.encoded_size,
                actual: bytes.len(),
            });
        }
        Ok(bytes)
    }

    batch_identity_accessors!();

    #[must_use]
    pub const fn block_hash(&self) -> B256 {
        self.block_hash
    }
}

fn validate_shard_envelope(
    shard_count: u32,
    parent_root: B256,
    new_root: B256,
    parent_shard_roots: &[B256],
    new_shard_roots: &[B256],
    changed_shards: &BTreeMap<ShardIndex, ProvisionalShardBatch>,
) -> Result<(), StagingError> {
    let expected_len = usize::try_from(shard_count)
        .map_err(|_| StagingError::InvalidShardEnvelope("shard count is not representable"))?;
    if shard_count == 0 || shard_count > 32 || !shard_count.is_power_of_two() {
        return Err(StagingError::InvalidShardEnvelope("invalid shard count"));
    }
    if parent_shard_roots.len() != expected_len || new_shard_roots.len() != expected_len {
        return Err(StagingError::InvalidShardEnvelope(
            "root vector length mismatch",
        ));
    }
    let parent = aggregate_b256_roots(parent_shard_roots)?;
    let new = aggregate_b256_roots(new_shard_roots)?;
    if parent != parent_root || new != new_root {
        return Err(StagingError::InvalidShardEnvelope(
            "aggregate root mismatch",
        ));
    }
    for index in 0..shard_count {
        let position = usize::try_from(index)
            .map_err(|_| StagingError::InvalidShardEnvelope("shard index overflow"))?;
        let changed = parent_shard_roots[position] != new_shard_roots[position];
        match (changed, changed_shards.get(&index)) {
            (true, Some(batch)) => {
                if batch.parent_shard_root != parent_shard_roots[position]
                    || batch.new_shard_root != new_shard_roots[position]
                {
                    return Err(StagingError::InvalidShardEnvelope(
                        "changed shard root mismatch",
                    ));
                }
                for key in batch.leaf_changes.keys() {
                    let smt_key = SmtTreeKey::from_be_bytes(key.encode())
                        .map_err(|_| StagingError::InvalidShardEnvelope("invalid leaf key"))?;
                    if shard_index(smt_key, shard_count).map_err(|_| {
                        StagingError::InvalidShardEnvelope("invalid shard derivation")
                    })? != index
                    {
                        return Err(StagingError::InvalidShardEnvelope("misderived leaf shard"));
                    }
                }
            }
            (false, None) => {}
            (true, None) => {
                return Err(StagingError::InvalidShardEnvelope("missing changed shard"));
            }
            (false, Some(_)) => {
                return Err(StagingError::InvalidShardEnvelope(
                    "unchanged shard included",
                ));
            }
        }
    }
    if changed_shards.keys().any(|index| *index >= shard_count) {
        return Err(StagingError::InvalidShardEnvelope(
            "shard index out of range",
        ));
    }
    Ok(())
}

fn aggregate_b256_roots(roots: &[B256]) -> Result<B256, StagingError> {
    aggregate_b256_shard_roots(roots)
        .map_err(|_| StagingError::InvalidShardEnvelope("invalid shard root aggregate"))
}

struct CanonicalBatch<'a> {
    block_number: u64,
    block_hash: B256,
    parent_block_hash: B256,
    parent_root: B256,
    new_root: B256,
    shard_count: u32,
    parent_roots: &'a [B256],
    new_roots: &'a [B256],
    changed_shards: &'a BTreeMap<ShardIndex, ProvisionalShardBatch>,
    encoded_size: u64,
}

/// Visits the platform-independent ADR-009 candidate accounting codec.
///
/// Root-vector lengths are the encoded `shard_count`. Map lengths and variable
/// branch values use BE4. Every record carries the BE4 persistent shard prefix,
/// and every change carries an explicit `Delete = 0` / `Set = 1` discriminant.
fn visit_canonical_batch(
    batch: CanonicalBatch<'_>,
    mut emit: impl FnMut(&[u8]) -> Result<(), StagingError>,
) -> Result<(), StagingError> {
    fn be4_len(len: usize) -> Result<[u8; 4], StagingError> {
        u32::try_from(len)
            .map(u32::to_be_bytes)
            .map_err(|_| StagingError::EncodedSizeOverflow)
    }

    emit(&batch.block_number.to_be_bytes())?;
    emit(batch.block_hash.as_slice())?;
    emit(batch.parent_block_hash.as_slice())?;
    emit(batch.parent_root.as_slice())?;
    emit(batch.new_root.as_slice())?;
    emit(&batch.shard_count.to_be_bytes())?;
    for root in batch.parent_roots {
        emit(root.as_slice())?;
    }
    for root in batch.new_roots {
        emit(root.as_slice())?;
    }
    emit(&be4_len(batch.changed_shards.len())?)?;
    for (index, shard) in batch.changed_shards {
        let shard_prefix = index.to_be_bytes();
        emit(&shard_prefix)?;
        emit(shard.parent_shard_root.as_slice())?;
        emit(shard.new_shard_root.as_slice())?;
        emit(&be4_len(shard.branch_changes.len())?)?;
        for (key, change) in &shard.branch_changes {
            emit(&shard_prefix)?;
            emit(&key.encode())?;
            match change {
                TreeChange::Set(node) => {
                    emit(&[1])?;
                    let encoded = node.encode();
                    emit(&be4_len(encoded.len())?)?;
                    emit(&encoded)?;
                }
                TreeChange::Delete => emit(&[0])?,
            }
        }
        emit(&be4_len(shard.leaf_changes.len())?)?;
        for (key, change) in &shard.leaf_changes {
            emit(&shard_prefix)?;
            emit(&key.encode())?;
            match change {
                TreeChange::Set(value) => {
                    emit(&[1])?;
                    emit(&value.encode())?;
                }
                TreeChange::Delete => emit(&[0])?,
            }
        }
    }
    emit(&batch.encoded_size.to_be_bytes())
}

fn encoded_shard_set_size(
    shard_count: u32,
    parent_roots: &[B256],
    new_roots: &[B256],
    changed_shards: &BTreeMap<ShardIndex, ProvisionalShardBatch>,
) -> Result<usize, StagingError> {
    if usize::try_from(shard_count).ok() != Some(parent_roots.len()) {
        return Err(StagingError::InvalidShardEnvelope(
            "root vector length mismatch",
        ));
    }
    let mut size = 0_usize;
    visit_canonical_batch(
        CanonicalBatch {
            block_number: 0,
            block_hash: B256::ZERO,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: B256::ZERO,
            shard_count,
            parent_roots,
            new_roots,
            changed_shards,
            encoded_size: 0,
        },
        |chunk| {
            size = size
                .checked_add(chunk.len())
                .ok_or(StagingError::EncodedSizeOverflow)?;
            Ok(())
        },
    )?;
    u64::try_from(size).map_err(|_| StagingError::EncodedSizeOverflow)?;
    Ok(size)
}

/// A consistent finalized read transaction used by one exact-parent view.
/// Implementations must return all reads from the same immutable snapshot.
pub trait FinalizedTreeSnapshot: Send {
    fn marker(&self) -> Result<FinalizedMarker, PersistenceError>;
    fn shard_roots(&self) -> Result<Vec<B256>, PersistenceError>;
    fn read_branch(
        &self,
        shard_index: ShardIndex,
        key: BranchKey,
    ) -> Result<Option<BranchNode>, PersistenceError>;
    fn read_leaf(
        &self,
        shard_index: ShardIndex,
        key: TreeKey,
    ) -> Result<Option<LeafValue>, PersistenceError>;
}

/// One exact finalized-parent view. Construction checks all identity fields,
/// including equality between the EVM-authoritative root and the MDBX marker.
#[derive(Clone)]
pub struct AuthenticatedTreeView {
    snapshot: Arc<Mutex<Box<dyn FinalizedTreeSnapshot>>>,
    identity: ExactParentIdentity,
    shard_roots: Arc<[B256]>,
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
        shard_count: u32,
    ) -> Result<Self, StagingError> {
        let marker = snapshot.marker()?;
        marker.verify_exact_parent(required)?;
        let shard_roots = snapshot.shard_roots()?;
        if shard_roots.len() != usize::try_from(shard_count).unwrap_or(usize::MAX) {
            return Err(StagingError::InvalidShardEnvelope(
                "root vector length mismatch",
            ));
        }
        if aggregate_b256_roots(&shard_roots)? != required.root {
            return Err(StagingError::InvalidShardEnvelope(
                "exact-parent shard roots do not match authoritative root",
            ));
        }
        Ok(Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
            identity: required,
            shard_roots: Arc::from(shard_roots),
        })
    }

    #[must_use]
    pub fn shard_roots(&self) -> &[B256] {
        &self.shard_roots
    }

    pub fn read_branch(
        &self,
        shard_index: ShardIndex,
        key: BranchKey,
    ) -> Result<Option<BranchNode>, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .read_branch(shard_index, key)
            .map_err(Into::into)
    }

    pub fn read_leaf(
        &self,
        shard_index: ShardIndex,
        key: TreeKey,
    ) -> Result<Option<LeafValue>, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .read_leaf(shard_index, key)
            .map_err(Into::into)
    }
}

/// CKB-compatible speculative store semantics: candidate writes shadow the
/// immutable exact-parent snapshot and remain confined to ordered maps.
#[derive(Debug)]
pub struct StagingCkbStore {
    base: AuthenticatedTreeView,
    shard_index: ShardIndex,
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
    pub fn new(base: AuthenticatedTreeView, shard_index: ShardIndex) -> Result<Self, StagingError> {
        if usize::try_from(shard_index).unwrap_or(usize::MAX) >= base.shard_roots.len() {
            return Err(StagingError::InvalidShardEnvelope(
                "shard index out of range",
            ));
        }
        Ok(Self {
            base,
            shard_index,
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::new(),
        })
    }

    pub fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, StagingError> {
        match self.branch_changes.get(&key) {
            Some(TreeChange::Set(node)) => Ok(Some(*node)),
            Some(TreeChange::Delete) => Ok(None),
            None => self.base.read_branch(self.shard_index, key),
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
            None => self.base.read_leaf(self.shard_index, key),
        }
    }

    pub fn write_leaf(&mut self, key: TreeKey, value: LeafValue) {
        self.leaf_changes.insert(key, TreeChange::Set(value));
    }

    pub fn delete_leaf(&mut self, key: TreeKey) {
        self.leaf_changes.insert(key, TreeChange::Delete);
    }

    pub fn freeze_shard(
        self,
        new_root: B256,
    ) -> Result<Option<ProvisionalShardBatch>, StagingError> {
        let parent_root = self.base.shard_roots[usize::try_from(self.shard_index)
            .map_err(|_| StagingError::InvalidShardEnvelope("shard index overflow"))?];
        if new_root == parent_root {
            if !self.branch_changes.is_empty() || !self.leaf_changes.is_empty() {
                return Err(StagingError::InvalidShardEnvelope(
                    "net-no-op shard contains records",
                ));
            }
            return Ok(None);
        }
        ProvisionalShardBatch::new(
            parent_root,
            new_root,
            self.branch_changes,
            self.leaf_changes,
        )
        .map(Some)
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
    #[error("invalid staged shard-set envelope: {0}")]
    InvalidShardEnvelope(&'static str),
    #[error("exact-parent snapshot lock poisoned")]
    SnapshotLockPoisoned,
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

        fn shard_roots(&self) -> Result<Vec<B256>, PersistenceError> {
            Ok(vec![self.marker.new_root])
        }

        fn read_branch(
            &self,
            shard_index: ShardIndex,
            key: BranchKey,
        ) -> Result<Option<BranchNode>, PersistenceError> {
            assert_eq!(shard_index, 0);
            Ok(self.branches.get(&key).copied())
        }

        fn read_leaf(
            &self,
            shard_index: ShardIndex,
            key: TreeKey,
        ) -> Result<Option<LeafValue>, PersistenceError> {
            assert_eq!(shard_index, 0);
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
        AuthenticatedTreeView::open(Box::new(snapshot), exact(), 1).unwrap()
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
            assert!(AuthenticatedTreeView::open(Box::new(snapshot), expected, 1).is_err());
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
        let mut store = StagingCkbStore::new(view(snapshot), 0).unwrap();

        assert_eq!(store.read_branch(branch_key).unwrap(), Some(base_node));
        assert_eq!(store.read_leaf(leaf_key).unwrap(), Some(base_leaf));
        store.write_branch(branch_key, node(20));
        store.delete_leaf(leaf_key);
        assert_eq!(store.read_branch(branch_key).unwrap(), Some(node(20)));
        assert_eq!(store.read_leaf(leaf_key).unwrap(), None);

        let batch = store.freeze_shard(b256(18)).unwrap().unwrap();
        assert_eq!(batch.branch_changes.len(), 1);
        assert_eq!(batch.leaf_changes.len(), 1);
    }

    #[test]
    fn ckb_store_traits_round_trip_typed_records_and_reject_invalid_leaf() {
        let snapshot = MemorySnapshot {
            marker: marker(),
            branches: BTreeMap::new(),
            leaves: BTreeMap::new(),
        };
        let mut store = StagingCkbStore::new(view(snapshot), 0).unwrap();
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
    fn shard_set_envelope_binds_complete_vectors_derivations_and_canonical_size() {
        let parent_roots = vec![B256::ZERO; 16];
        let parent_root = aggregate_b256_roots(&parent_roots).unwrap();
        let mut new_roots = parent_roots.clone();
        new_roots[1] = b256(9);
        let new_root = aggregate_b256_roots(&new_roots).unwrap();
        let changed = ProvisionalShardBatch::new(
            B256::ZERO,
            b256(9),
            BTreeMap::new(),
            BTreeMap::from([(key(1), TreeChange::Set(leaf(2)))]),
        )
        .unwrap();
        let provisional = ProvisionalTreeBatch::new(
            8,
            b256(7),
            parent_root,
            new_root,
            16,
            parent_roots.clone(),
            new_roots.clone(),
            BTreeMap::from([(1, changed.clone())]),
        )
        .unwrap();
        assert_eq!(provisional.shard_count(), 16);
        assert_eq!(provisional.changed_shard_count(), 1);
        assert_eq!(provisional.encoded_size(), 1_321);

        let staged = provisional.freeze(b256(8));
        assert_eq!(staged.parent_shard_roots(), parent_roots);
        assert_eq!(staged.new_shard_roots(), new_roots);
        staged.validate_encoded_size().unwrap();
        let canonical = staged.canonical_bytes().unwrap();
        assert_eq!(canonical.len(), staged.encoded_size());
        assert_eq!(
            &canonical[canonical.len() - 8..],
            &u64::try_from(staged.encoded_size()).unwrap().to_be_bytes()
        );
        assert_eq!(
            crate::bench_support::candidate_checksum(&staged),
            alloy_primitives::keccak256(canonical)
        );

        assert!(ProvisionalTreeBatch::new(
            8,
            b256(7),
            parent_root,
            new_root,
            16,
            vec![B256::ZERO; 15],
            new_roots.clone(),
            BTreeMap::from([(1, changed.clone())]),
        )
        .is_err());
        assert!(ProvisionalTreeBatch::new(
            8,
            b256(7),
            parent_root,
            new_root,
            16,
            parent_roots.clone(),
            new_roots.clone(),
            BTreeMap::from([(2, changed.clone())]),
        )
        .is_err());
        let misderived = ProvisionalShardBatch::new(
            B256::ZERO,
            b256(9),
            BTreeMap::new(),
            BTreeMap::from([(key(2), TreeChange::Set(leaf(2)))]),
        )
        .unwrap();
        assert!(ProvisionalTreeBatch::new(
            8,
            b256(7),
            parent_root,
            new_root,
            16,
            parent_roots,
            new_roots,
            BTreeMap::from([(1, misderived)]),
        )
        .is_err());
    }

    #[test]
    fn publication_is_structurally_idempotent_and_never_evicts() {
        let provisional = ProvisionalTreeBatch::new_unsharded(
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

        let competing = ProvisionalTreeBatch::new_unsharded(
            8,
            b256(7),
            b256(17),
            b256(19),
            BTreeMap::new(),
            BTreeMap::from([(key(2), TreeChange::Set(leaf(3)))]),
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
        let first = ProvisionalTreeBatch::new_unsharded(
            8,
            b256(7),
            b256(17),
            b256(18),
            BTreeMap::new(),
            BTreeMap::from([(key(1), TreeChange::Set(leaf(2)))]),
        )
        .unwrap()
        .freeze(b256(8));
        let conflicting = ProvisionalTreeBatch::new_unsharded(
            8,
            b256(7),
            b256(17),
            b256(19),
            BTreeMap::new(),
            BTreeMap::from([(key(1), TreeChange::Set(leaf(3)))]),
        )
        .unwrap()
        .freeze(b256(8));
        let mut cache = CandidateCache::new(CandidateCacheLimits {
            max_candidates: 2,
            max_encoded_bytes: 2_000,
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
            max_encoded_bytes: 2_000,
        });
        for (height, hash) in [(8, 8), (8, 9), (9, 10)] {
            cache
                .publish(
                    ProvisionalTreeBatch::new_unsharded(
                        height,
                        b256(7),
                        b256(17),
                        b256(18),
                        BTreeMap::new(),
                        BTreeMap::from([(key(hash), TreeChange::Set(leaf(hash + 1)))]),
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
