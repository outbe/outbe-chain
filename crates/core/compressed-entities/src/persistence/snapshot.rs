use alloy_primitives::B256;
use std::path::PathBuf;

use super::{
    collection_has_records, count_collection_leaf_records, count_collection_root_records,
    prefixed_key, read_tree_root, tables, BranchKey, BranchNode, FinalizedMarker, LeafValue,
    PersistenceError, TreeKey, TreeNamespace,
};
use crate::staging::FinalizedTreeSnapshot;
use crate::CollectionKey;
use reth_db::mdbx::tx::Tx;
use reth_db::mdbx::RO;
use reth_db::transaction::DbTx;

pub(super) struct MdbxSnapshot {
    pub(super) path: PathBuf,
    pub(super) tx: Tx<RO>,
    pub(super) marker: FinalizedMarker,
}

impl FinalizedTreeSnapshot for MdbxSnapshot {
    fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
        Ok(self.marker)
    }

    fn tree_root(&self, namespace: TreeNamespace) -> Result<Option<B256>, PersistenceError> {
        read_tree_root(&self.tx, &self.path, namespace)
    }

    fn collection_has_records(&self, collection: CollectionKey) -> Result<bool, PersistenceError> {
        collection_has_records(&self.tx, &self.path, collection)
    }

    fn collection_root_count(&self, collection: CollectionKey) -> Result<usize, PersistenceError> {
        count_collection_root_records(&self.tx, &self.path, collection)
    }

    fn collection_leaf_count(&self, collection: CollectionKey) -> Result<usize, PersistenceError> {
        count_collection_leaf_records(&self.tx, &self.path, collection)
    }

    fn read_branch(
        &self,
        namespace: TreeNamespace,
        key: BranchKey,
    ) -> Result<Option<BranchNode>, PersistenceError> {
        self.tx
            .get::<tables::CeBranches>(prefixed_key(namespace, &key.encode()))
            .map_err(|error| PersistenceError::Database {
                path: self.path.clone(),
                message: error.to_string(),
            })?
            .map(|bytes| BranchNode::decode(&bytes))
            .transpose()
    }

    fn read_leaf(
        &self,
        namespace: TreeNamespace,
        key: TreeKey,
    ) -> Result<Option<LeafValue>, PersistenceError> {
        self.tx
            .get::<tables::CeLeaves>(prefixed_key(namespace, &key.encode()))
            .map_err(|error| PersistenceError::Database {
                path: self.path.clone(),
                message: error.to_string(),
            })?
            .map(|bytes| LeafValue::decode(&bytes))
            .transpose()
    }
}
