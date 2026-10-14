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
    PersistenceError, TreeKey, TreeNamespace,
};
use crate::{
    collection::{collection_root, sealed_root},
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::TreeKey as SmtTreeKey,
    CeDomain, CollectionKey,
};

mod envelope;
use envelope::{aggregate_b256_roots, validate_catalog_envelope, validate_shard_envelope};

pub type ShardIndex = u32;

#[derive(Clone, Copy)]
pub(crate) struct RootTransition {
    pub(crate) parent: B256,
    pub(crate) new: B256,
}

#[derive(Clone, Copy)]
pub(crate) struct CatalogRoots {
    pub(crate) sealed: RootTransition,
    pub(crate) catalog: RootTransition,
}

/// A staged store mutation. Deletes exist only in candidate memory. Finalized
/// MDBX applies them by removing the corresponding record.
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

/// ADR-009's canonical shard-set payload reused inside one ADR-010 collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalShardSetBatch {
    pub(crate) shard_count: u32,
    pub(crate) parent_shard_top_root: B256,
    pub(crate) new_shard_top_root: B256,
    pub(crate) parent_shard_roots: Vec<B256>,
    pub(crate) new_shard_roots: Vec<B256>,
    pub(crate) changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    pub(crate) encoded_size: usize,
}

impl ProvisionalShardSetBatch {
    pub(crate) fn new(
        shard_count: u32,
        roots: RootTransition,
        parent_shard_roots: Vec<B256>,
        new_shard_roots: Vec<B256>,
        changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    ) -> Result<Self, StagingError> {
        let parent_shard_top_root = roots.parent;
        let new_shard_top_root = roots.new;
        validate_shard_envelope(
            shard_count,
            roots,
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
            shard_count,
            parent_shard_top_root,
            new_shard_top_root,
            parent_shard_roots,
            new_shard_roots,
            changed_shards,
            encoded_size,
        })
    }

    #[must_use]
    pub const fn shard_count(&self) -> u32 {
        self.shard_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionBatch {
    pub(crate) domain_id: u16,
    pub(crate) parent_collection_root: Option<B256>,
    pub(crate) new_collection_root: B256,
    pub(crate) shard_set: ProvisionalShardSetBatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetirementBatch {
    pub(crate) domain_id: u16,
    pub(crate) parent_collection_root: B256,
    pub(crate) parent_shard_roots: Vec<B256>,
}

impl RetirementBatch {
    pub(crate) fn new(
        domain: CeDomain,
        key: CollectionKey,
        parent_collection_root: B256,
        parent_shard_roots: Vec<B256>,
    ) -> Result<Self, StagingError> {
        if domain != CeDomain::Tribute || parent_shard_roots.len() != domain.shard_count() as usize
        {
            return Err(StagingError::InvalidCatalogEnvelope(
                "only a complete Tribute collection may retire",
            ));
        }
        let top =
            aggregate_b256_shard_roots(&parent_shard_roots).map_err(StagingError::tree_hash)?;
        if collection_root(domain, key, top).map_err(StagingError::Collection)?
            != parent_collection_root
        {
            return Err(StagingError::InvalidCatalogEnvelope(
                "retirement parent collection root mismatch",
            ));
        }
        Ok(Self {
            domain_id: domain.id(),
            parent_collection_root,
            parent_shard_roots,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectionOperation {
    Mutate(CollectionBatch),
    Retire(RetirementBatch),
}

impl CollectionOperation {
    pub(crate) fn mutation(&self) -> Option<&CollectionBatch> {
        match self {
            Self::Mutate(batch) => Some(batch),
            Self::Retire(_) => None,
        }
    }
}

impl CollectionBatch {
    pub(crate) fn new(
        domain: CeDomain,
        key: CollectionKey,
        parent_collection_root: Option<B256>,
        new_collection_root: B256,
        shard_set: ProvisionalShardSetBatch,
    ) -> Result<Self, StagingError> {
        if shard_set.shard_count != domain.shard_count() {
            return Err(StagingError::InvalidCatalogEnvelope(
                "collection shard count does not match domain topology",
            ));
        }
        if let Some(parent) = parent_collection_root {
            let expected = collection_root(domain, key, shard_set.parent_shard_top_root)
                .map_err(StagingError::Collection)?;
            if parent != expected {
                return Err(StagingError::InvalidCatalogEnvelope(
                    "parent collection root mismatch",
                ));
            }
        } else if shard_set
            .parent_shard_roots
            .iter()
            .any(|root| *root != B256::ZERO)
        {
            return Err(StagingError::InvalidCatalogEnvelope(
                "absent collection must start from all-zero shard roots",
            ));
        }
        let expected = collection_root(domain, key, shard_set.new_shard_top_root)
            .map_err(StagingError::Collection)?;
        if expected != new_collection_root || shard_set.changed_shards.is_empty() {
            return Err(StagingError::InvalidCatalogEnvelope(
                "changed collection root or shard payload mismatch",
            ));
        }
        Ok(Self {
            domain_id: domain.id(),
            parent_collection_root,
            new_collection_root,
            shard_set,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalCatalogBatch {
    pub(crate) parent_catalog_root: B256,
    pub(crate) new_catalog_root: B256,
    pub(crate) branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
    pub(crate) leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
}

macro_rules! tree_batch_accessors {
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
            self.parent_r_sealed
        }
        #[must_use]
        pub const fn new_root(&self) -> B256 {
            self.new_r_sealed
        }
        #[must_use]
        pub fn changed_shard_count(&self) -> usize {
            self.changed_collections
                .values()
                .filter_map(CollectionOperation::mutation)
                .map(|batch| batch.shard_set.changed_shards.len())
                .sum()
        }
        #[must_use]
        pub fn branch_change_count(&self) -> usize {
            self.changed_collections
                .values()
                .filter_map(CollectionOperation::mutation)
                .flat_map(|batch| batch.shard_set.changed_shards.values())
                .map(ProvisionalShardBatch::branch_change_count)
                .sum::<usize>()
                + self
                    .catalog_batch
                    .as_ref()
                    .map_or(0, |batch| batch.branch_changes.len())
        }
        #[must_use]
        pub fn leaf_change_count(&self) -> usize {
            self.changed_collections
                .values()
                .filter_map(CollectionOperation::mutation)
                .flat_map(|batch| batch.shard_set.changed_shards.values())
                .map(ProvisionalShardBatch::leaf_change_count)
                .sum::<usize>()
                + self
                    .catalog_batch
                    .as_ref()
                    .map_or(0, |batch| batch.leaf_changes.len())
        }
        #[must_use]
        pub const fn encoded_size(&self) -> usize {
            self.encoded_size
        }
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionalTreeBatch {
    pub(crate) block_number: u64,
    pub(crate) parent_block_hash: B256,
    pub(crate) parent_r_sealed: B256,
    pub(crate) new_r_sealed: B256,
    pub(crate) parent_catalog_root: B256,
    pub(crate) new_catalog_root: B256,
    pub(crate) changed_collections: BTreeMap<CollectionKey, CollectionOperation>,
    pub(crate) catalog_batch: Option<ProvisionalCatalogBatch>,
    pub(crate) encoded_size: usize,
}

impl ProvisionalTreeBatch {
    /// Builds the ordinary no-effective-change candidate for an exact parent.
    pub fn new_identity(
        block_number: u64,
        parent_block_hash: B256,
        parent_catalog_root: B256,
    ) -> Result<Self, StagingError> {
        let parent_r_sealed = sealed_root(parent_catalog_root).map_err(StagingError::Collection)?;
        Self::new(
            block_number,
            parent_block_hash,
            CatalogRoots {
                sealed: RootTransition {
                    parent: parent_r_sealed,
                    new: parent_r_sealed,
                },
                catalog: RootTransition {
                    parent: parent_catalog_root,
                    new: parent_catalog_root,
                },
            },
            BTreeMap::new(),
            None,
        )
    }

    /// Synthetic single-collection fixture retained only for historical tests
    /// and benchmarks. It is deliberately not part of the public API.
    pub(crate) fn new_fixture_single_collection(
        block_number: u64,
        parent_block_hash: B256,
        new_root: B256,
        branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
        leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
    ) -> Result<Self, StagingError> {
        let parent_catalog_root = B256::ZERO;
        let parent_r_sealed = sealed_root(parent_catalog_root)
            .map_err(|_| StagingError::InvalidCatalogEnvelope("invalid benchmark parent"))?;
        if branch_changes.is_empty() && leaf_changes.is_empty() {
            return Self::new_identity(block_number, parent_block_hash, parent_catalog_root);
        }

        let domain = CeDomain::Tribute;
        let collection_key = CollectionKey::try_from(B256::from([0_u8; 32]))
            .map_err(|_| StagingError::InvalidCatalogEnvelope("invalid benchmark collection"))?;
        let mut parent_shard_roots = vec![B256::ZERO; domain.shard_count() as usize];
        let mut new_shard_roots = parent_shard_roots.clone();
        new_shard_roots[0] = new_root;
        let parent_shard_top_root = aggregate_b256_shard_roots(&parent_shard_roots)
            .map_err(|_| StagingError::InvalidCatalogEnvelope("invalid benchmark shard roots"))?;
        let new_shard_top_root = aggregate_b256_shard_roots(&new_shard_roots)
            .map_err(|_| StagingError::InvalidCatalogEnvelope("invalid benchmark shard roots"))?;
        let shard = ProvisionalShardBatch::new(B256::ZERO, new_root, branch_changes, leaf_changes)?;
        let shard_set = ProvisionalShardSetBatch::new(
            domain.shard_count(),
            crate::staging::RootTransition {
                parent: parent_shard_top_root,
                new: new_shard_top_root,
            },
            std::mem::take(&mut parent_shard_roots),
            new_shard_roots,
            BTreeMap::from([(0, shard)]),
        )?;
        let new_collection_root = collection_root(domain, collection_key, new_shard_top_root)
            .map_err(|_| {
                StagingError::InvalidCatalogEnvelope("invalid benchmark collection root")
            })?;
        let collection = CollectionOperation::Mutate(CollectionBatch::new(
            domain,
            collection_key,
            None,
            new_collection_root,
            shard_set,
        )?);
        let catalog_key = TreeKey::try_from(B256::from(*collection_key.as_bytes()))?;
        let catalog_value = LeafValue::try_from(new_collection_root)?;
        let new_catalog_root = new_root;
        let catalog_batch = ProvisionalCatalogBatch {
            parent_catalog_root,
            new_catalog_root,
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::from([(catalog_key, TreeChange::Set(catalog_value))]),
        };
        let new_r_sealed = sealed_root(new_catalog_root)
            .map_err(|_| StagingError::InvalidCatalogEnvelope("invalid benchmark root"))?;
        Self::new(
            block_number,
            parent_block_hash,
            CatalogRoots {
                sealed: RootTransition {
                    parent: parent_r_sealed,
                    new: new_r_sealed,
                },
                catalog: RootTransition {
                    parent: parent_catalog_root,
                    new: new_catalog_root,
                },
            },
            BTreeMap::from([(collection_key, collection)]),
            Some(catalog_batch),
        )
    }

    pub(crate) fn new(
        block_number: u64,
        parent_block_hash: B256,
        roots: CatalogRoots,
        changed_collections: BTreeMap<CollectionKey, CollectionOperation>,
        catalog_batch: Option<ProvisionalCatalogBatch>,
    ) -> Result<Self, StagingError> {
        let CatalogRoots {
            sealed:
                RootTransition {
                    parent: parent_r_sealed,
                    new: new_r_sealed,
                },
            catalog:
                RootTransition {
                    parent: parent_catalog_root,
                    new: new_catalog_root,
                },
        } = roots;
        validate_catalog_envelope(roots, &changed_collections, catalog_batch.as_ref())?;
        let mut batch = Self {
            block_number,
            parent_block_hash,
            parent_r_sealed,
            new_r_sealed,
            parent_catalog_root,
            new_catalog_root,
            changed_collections,
            catalog_batch,
            encoded_size: 0,
        };
        batch.encoded_size = canonical_tree_bytes(&batch, B256::ZERO)?.len();
        Ok(batch)
    }

    #[must_use]
    pub fn freeze(self, block_hash: B256) -> StagedTreeBatch {
        StagedTreeBatch {
            block_number: self.block_number,
            block_hash,
            parent_block_hash: self.parent_block_hash,
            parent_r_sealed: self.parent_r_sealed,
            new_r_sealed: self.new_r_sealed,
            parent_catalog_root: self.parent_catalog_root,
            new_catalog_root: self.new_catalog_root,
            changed_collections: self.changed_collections,
            catalog_batch: self.catalog_batch,
            encoded_size: self.encoded_size,
        }
    }

    tree_batch_accessors!();
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedTreeBatch {
    pub(crate) block_number: u64,
    pub(crate) block_hash: B256,
    pub(crate) parent_block_hash: B256,
    pub(crate) parent_r_sealed: B256,
    pub(crate) new_r_sealed: B256,
    pub(crate) parent_catalog_root: B256,
    pub(crate) new_catalog_root: B256,
    pub(crate) changed_collections: BTreeMap<CollectionKey, CollectionOperation>,
    pub(crate) catalog_batch: Option<ProvisionalCatalogBatch>,
    pub(crate) encoded_size: usize,
}

impl StagedTreeBatch {
    #[must_use]
    pub const fn marker(&self, commitment_scheme_version: u32) -> FinalizedMarker {
        FinalizedMarker {
            commitment_scheme_version,
            height: self.block_number,
            block_hash: self.block_hash,
            parent_block_hash: self.parent_block_hash,
            parent_root: self.parent_r_sealed,
            new_root: self.new_r_sealed,
        }
    }

    pub fn validate_encoded_size(&self) -> Result<(), StagingError> {
        validate_catalog_envelope(
            CatalogRoots {
                sealed: RootTransition {
                    parent: self.parent_r_sealed,
                    new: self.new_r_sealed,
                },
                catalog: RootTransition {
                    parent: self.parent_catalog_root,
                    new: self.new_catalog_root,
                },
            },
            &self.changed_collections,
            self.catalog_batch.as_ref(),
        )?;
        let actual = canonical_staged_tree_bytes(self)?.len();
        if actual != self.encoded_size {
            return Err(StagingError::EncodedSizeMismatch {
                declared: self.encoded_size,
                actual,
            });
        }
        Ok(())
    }

    pub(crate) fn canonical_bytes(&self) -> Result<Vec<u8>, StagingError> {
        canonical_staged_tree_bytes(self)
    }

    tree_batch_accessors!();

    #[must_use]
    pub const fn block_hash(&self) -> B256 {
        self.block_hash
    }
}

fn be4_len(len: usize) -> Result<[u8; 4], StagingError> {
    u32::try_from(len)
        .map(u32::to_be_bytes)
        .map_err(|_| StagingError::EncodedSizeOverflow)
}

fn append_change_maps(
    bytes: &mut Vec<u8>,
    branch_changes: &BTreeMap<BranchKey, TreeChange<BranchNode>>,
    leaf_changes: &BTreeMap<TreeKey, TreeChange<LeafValue>>,
) -> Result<(), StagingError> {
    bytes.extend_from_slice(&be4_len(branch_changes.len())?);
    for (key, change) in branch_changes {
        bytes.extend_from_slice(&key.encode());
        match change {
            TreeChange::Set(node) => {
                bytes.push(1);
                let encoded = node.encode();
                bytes.extend_from_slice(&be4_len(encoded.len())?);
                bytes.extend_from_slice(&encoded);
            }
            TreeChange::Delete => bytes.push(0),
        }
    }
    bytes.extend_from_slice(&be4_len(leaf_changes.len())?);
    for (key, change) in leaf_changes {
        bytes.extend_from_slice(&key.encode());
        match change {
            TreeChange::Set(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.encode());
            }
            TreeChange::Delete => bytes.push(0),
        }
    }
    Ok(())
}

fn append_shard_set(
    bytes: &mut Vec<u8>,
    shard_set: &ProvisionalShardSetBatch,
) -> Result<(), StagingError> {
    bytes.extend_from_slice(&shard_set.shard_count.to_be_bytes());
    bytes.extend_from_slice(shard_set.parent_shard_top_root.as_slice());
    bytes.extend_from_slice(shard_set.new_shard_top_root.as_slice());
    for root in &shard_set.parent_shard_roots {
        bytes.extend_from_slice(root.as_slice());
    }
    for root in &shard_set.new_shard_roots {
        bytes.extend_from_slice(root.as_slice());
    }
    bytes.extend_from_slice(&be4_len(shard_set.changed_shards.len())?);
    for (index, shard) in &shard_set.changed_shards {
        bytes.extend_from_slice(&index.to_be_bytes());
        bytes.extend_from_slice(shard.parent_shard_root.as_slice());
        bytes.extend_from_slice(shard.new_shard_root.as_slice());
        append_change_maps(bytes, &shard.branch_changes, &shard.leaf_changes)?;
    }
    bytes.extend_from_slice(
        &u64::try_from(shard_set.encoded_size)
            .map_err(|_| StagingError::EncodedSizeOverflow)?
            .to_be_bytes(),
    );
    Ok(())
}

fn canonical_tree_bytes(
    batch: &ProvisionalTreeBatch,
    block_hash: B256,
) -> Result<Vec<u8>, StagingError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&batch.block_number.to_be_bytes());
    bytes.extend_from_slice(block_hash.as_slice());
    bytes.extend_from_slice(batch.parent_block_hash.as_slice());
    bytes.extend_from_slice(batch.parent_r_sealed.as_slice());
    bytes.extend_from_slice(batch.new_r_sealed.as_slice());
    bytes.extend_from_slice(batch.parent_catalog_root.as_slice());
    bytes.extend_from_slice(batch.new_catalog_root.as_slice());
    bytes.extend_from_slice(&be4_len(batch.changed_collections.len())?);
    for (key, operation) in &batch.changed_collections {
        bytes.extend_from_slice(key.as_bytes());
        match operation {
            CollectionOperation::Mutate(collection) => {
                bytes.push(0);
                bytes.extend_from_slice(&collection.domain_id.to_be_bytes());
                match collection.parent_collection_root {
                    Some(root) => {
                        bytes.push(1);
                        bytes.extend_from_slice(root.as_slice());
                    }
                    None => bytes.push(0),
                }
                bytes.extend_from_slice(collection.new_collection_root.as_slice());
                append_shard_set(&mut bytes, &collection.shard_set)?;
            }
            CollectionOperation::Retire(retirement) => {
                bytes.push(1);
                bytes.extend_from_slice(&retirement.domain_id.to_be_bytes());
                bytes.extend_from_slice(retirement.parent_collection_root.as_slice());
                bytes.extend_from_slice(&be4_len(retirement.parent_shard_roots.len())?);
                for root in &retirement.parent_shard_roots {
                    bytes.extend_from_slice(root.as_slice());
                }
            }
        }
    }
    match &batch.catalog_batch {
        Some(catalog) => {
            bytes.push(1);
            bytes.extend_from_slice(catalog.parent_catalog_root.as_slice());
            bytes.extend_from_slice(catalog.new_catalog_root.as_slice());
            append_change_maps(&mut bytes, &catalog.branch_changes, &catalog.leaf_changes)?;
        }
        None => bytes.push(0),
    }
    bytes.extend_from_slice(
        &u64::try_from(batch.encoded_size)
            .map_err(|_| StagingError::EncodedSizeOverflow)?
            .to_be_bytes(),
    );
    Ok(bytes)
}

fn canonical_staged_tree_bytes(batch: &StagedTreeBatch) -> Result<Vec<u8>, StagingError> {
    canonical_tree_bytes(
        &ProvisionalTreeBatch {
            block_number: batch.block_number,
            parent_block_hash: batch.parent_block_hash,
            parent_r_sealed: batch.parent_r_sealed,
            new_r_sealed: batch.new_r_sealed,
            parent_catalog_root: batch.parent_catalog_root,
            new_catalog_root: batch.new_catalog_root,
            changed_collections: batch.changed_collections.clone(),
            catalog_batch: batch.catalog_batch.clone(),
            encoded_size: batch.encoded_size,
        },
        batch.block_hash,
    )
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
    let mut bytes = Vec::new();
    append_shard_set(
        &mut bytes,
        &ProvisionalShardSetBatch {
            shard_count,
            parent_shard_top_root: aggregate_b256_roots(parent_roots)?,
            new_shard_top_root: aggregate_b256_roots(new_roots)?,
            parent_shard_roots: parent_roots.to_vec(),
            new_shard_roots: new_roots.to_vec(),
            changed_shards: changed_shards.clone(),
            encoded_size: 0,
        },
    )?;
    let size = bytes.len();
    u64::try_from(size).map_err(|_| StagingError::EncodedSizeOverflow)?;
    Ok(size)
}

/// A consistent finalized read transaction used by one exact-parent view.
/// Implementations must return all reads from the same immutable snapshot.
pub trait FinalizedTreeSnapshot: Send {
    fn marker(&self) -> Result<FinalizedMarker, PersistenceError>;
    fn tree_root(&self, namespace: TreeNamespace) -> Result<Option<B256>, PersistenceError>;
    fn collection_has_records(&self, collection: CollectionKey) -> Result<bool, PersistenceError>;
    fn collection_root_count(&self, collection: CollectionKey) -> Result<usize, PersistenceError>;
    fn collection_leaf_count(&self, collection: CollectionKey) -> Result<usize, PersistenceError>;
    fn read_branch(
        &self,
        namespace: TreeNamespace,
        key: BranchKey,
    ) -> Result<Option<BranchNode>, PersistenceError>;
    fn read_leaf(
        &self,
        namespace: TreeNamespace,
        key: TreeKey,
    ) -> Result<Option<LeafValue>, PersistenceError>;
}

mod store;
pub use store::{AuthenticatedCatalogView, StagingCkbStore};

mod cache;
pub use cache::{CandidateCache, CandidateCacheLimits, PublicationOutcome};

#[derive(Debug, Error)]
pub enum StagingError {
    #[error("CES2 hashing failed: {0}")]
    Hash(#[source] crate::CommitmentError),
    #[error("CES2 collection construction failed: {0}")]
    Collection(#[source] crate::CollectionError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error("staged record size overflow")]
    EncodedSizeOverflow,
    #[error("staged encoded size mismatch: declared {declared}, actual {actual}")]
    EncodedSizeMismatch { declared: usize, actual: usize },
    #[error("invalid staged shard-set envelope: {0}")]
    InvalidShardEnvelope(&'static str),
    #[error("invalid staged Root Catalog envelope: {0}")]
    InvalidCatalogEnvelope(&'static str),
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

impl StagingError {
    pub fn is_infrastructure(&self) -> bool {
        match self {
            Self::Hash(error) => error.is_infrastructure(),
            Self::Collection(error) => error.is_infrastructure(),
            Self::Persistence(error) => error.is_infrastructure(),
            _ => false,
        }
    }

    pub(crate) fn tree_hash(error: crate::smt::TreeError) -> Self {
        match error {
            crate::smt::TreeError::Poseidon(error) => Self::Hash(error),
            _ => Self::InvalidShardEnvelope("invalid shard root aggregate"),
        }
    }
}

#[cfg(test)]
mod tests;
