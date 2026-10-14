use super::*;

/// One exact finalized-parent view. Construction checks all identity fields,
/// including equality between the EVM-authoritative root and the MDBX marker.
#[derive(Clone)]
pub struct AuthenticatedCatalogView {
    snapshot: Arc<Mutex<Box<dyn FinalizedTreeSnapshot>>>,
    identity: ExactParentIdentity,
    catalog_root: B256,
}

impl std::fmt::Debug for AuthenticatedCatalogView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedCatalogView")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedCatalogView {
    pub fn open(
        snapshot: Box<dyn FinalizedTreeSnapshot>,
        required: ExactParentIdentity,
    ) -> Result<Self, StagingError> {
        let marker = snapshot.marker()?;
        marker.verify_exact_parent(required)?;
        let catalog_root = snapshot.tree_root(TreeNamespace::Catalog)?.ok_or(
            StagingError::InvalidCatalogEnvelope("catalog root is missing"),
        )?;
        if sealed_root(catalog_root).map_err(StagingError::Collection)? != required.root {
            return Err(StagingError::InvalidCatalogEnvelope(
                "exact-parent catalog root does not match authoritative sealed root",
            ));
        }
        Ok(Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
            identity: required,
            catalog_root,
        })
    }

    #[must_use]
    pub const fn catalog_root(&self) -> B256 {
        self.catalog_root
    }

    #[must_use]
    pub const fn identity(&self) -> ExactParentIdentity {
        self.identity
    }

    pub fn tree_root(&self, namespace: TreeNamespace) -> Result<Option<B256>, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .tree_root(namespace)
            .map_err(Into::into)
    }

    pub fn collection_has_records(&self, collection: CollectionKey) -> Result<bool, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .collection_has_records(collection)
            .map_err(Into::into)
    }

    pub fn collection_root_count(&self, collection: CollectionKey) -> Result<usize, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .collection_root_count(collection)
            .map_err(Into::into)
    }

    /// Full-folds the persisted non-zero leaves for one collection inside this
    /// exact immutable snapshot. The count is authoritative CE materialization,
    /// never a Mongo/body-store count.
    pub fn collection_leaf_count(&self, collection: CollectionKey) -> Result<usize, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .collection_leaf_count(collection)
            .map_err(Into::into)
    }

    pub fn read_branch(
        &self,
        namespace: TreeNamespace,
        key: BranchKey,
    ) -> Result<Option<BranchNode>, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .read_branch(namespace, key)
            .map_err(Into::into)
    }

    pub fn read_leaf(
        &self,
        namespace: TreeNamespace,
        key: TreeKey,
    ) -> Result<Option<LeafValue>, StagingError> {
        self.snapshot
            .lock()
            .map_err(|_| StagingError::SnapshotLockPoisoned)?
            .read_leaf(namespace, key)
            .map_err(Into::into)
    }
}

/// CKB-compatible speculative store semantics: candidate writes shadow the
/// immutable exact-parent snapshot and remain confined to ordered maps.
#[derive(Debug)]
pub struct StagingCkbStore {
    base: AuthenticatedCatalogView,
    namespace: TreeNamespace,
    parent_root: B256,
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
    pub fn new(
        base: AuthenticatedCatalogView,
        namespace: TreeNamespace,
        parent_root: B256,
    ) -> Self {
        Self {
            base,
            namespace,
            parent_root,
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::new(),
        }
    }

    pub fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, StagingError> {
        match self.branch_changes.get(&key) {
            Some(TreeChange::Set(node)) => Ok(Some(*node)),
            Some(TreeChange::Delete) => Ok(None),
            None => self.base.read_branch(self.namespace, key),
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
            None => self.base.read_leaf(self.namespace, key),
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
        let parent_root = self.parent_root;
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
