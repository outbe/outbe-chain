use std::collections::BTreeMap;

use outbe_sparse_merkle_tree_v061::{
    error::Error,
    traits::{StoreReadOps, StoreWriteOps},
    BranchKey, BranchNode, H256,
};

use super::facade::{validate_merge_value, vendor_store_error};

#[derive(Default)]
pub(super) struct UpdateChanges {
    branches: BTreeMap<BranchKey, Option<BranchNode>>,
    leaves: BTreeMap<H256, Option<H256>>,
}

impl UpdateChanges {
    pub(super) fn apply(self, store: &mut impl StoreWriteOps<H256>) -> Result<(), Error> {
        for (key, value) in self.branches {
            match value {
                Some(value) => store.insert_branch(key, value)?,
                None => store.remove_branch(&key)?,
            }
        }
        for (key, value) in self.leaves {
            match value {
                Some(value) => store.insert_leaf(key, value)?,
                None => store.remove_leaf(&key)?,
            }
        }
        Ok(())
    }
}

/// CKB must finish all fallible hashes before touching the caller's store.
pub(super) struct UpdateOverlay<'a, S> {
    base: &'a S,
    pub(super) changes: UpdateChanges,
}

impl<'a, S> UpdateOverlay<'a, S> {
    pub(super) fn new(base: &'a S) -> Self {
        Self {
            base,
            changes: UpdateChanges::default(),
        }
    }
}

impl<S: StoreReadOps<H256>> StoreReadOps<H256> for UpdateOverlay<'_, S> {
    fn get_branch(&self, key: &BranchKey) -> Result<Option<BranchNode>, Error> {
        match self.changes.branches.get(key) {
            Some(value) => Ok(value.clone()),
            None => self.base.get_branch(key),
        }
    }
    fn get_leaf(&self, key: &H256) -> Result<Option<H256>, Error> {
        match self.changes.leaves.get(key) {
            Some(value) => Ok(*value),
            None => self.base.get_leaf(key),
        }
    }
}

impl<S> StoreWriteOps<H256> for UpdateOverlay<'_, S> {
    fn insert_branch(&mut self, key: BranchKey, branch: BranchNode) -> Result<(), Error> {
        validate_merge_value(&branch.left).map_err(vendor_store_error)?;
        validate_merge_value(&branch.right).map_err(vendor_store_error)?;
        self.changes.branches.insert(key, Some(branch));
        Ok(())
    }
    fn insert_leaf(&mut self, key: H256, leaf: H256) -> Result<(), Error> {
        self.changes.leaves.insert(key, Some(leaf));
        Ok(())
    }
    fn remove_branch(&mut self, key: &BranchKey) -> Result<(), Error> {
        self.changes.branches.insert(key.clone(), None);
        Ok(())
    }
    fn remove_leaf(&mut self, key: &H256) -> Result<(), Error> {
        self.changes.leaves.insert(*key, None);
        Ok(())
    }
}
