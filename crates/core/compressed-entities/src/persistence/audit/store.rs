//! Native CKB storage confined to a caller-owned scratch transaction.

use alloy_primitives::B256;
use outbe_sparse_merkle_tree_v061::{
    error::Error as CkbError,
    merge::MergeValue as CkbMergeValue,
    traits::{StoreReadOps, StoreWriteOps},
    BranchKey as CkbBranchKey, BranchNode as CkbBranchNode, H256,
};
use reth_db::transaction::{DbTx, DbTxMut};

use crate::persistence::{
    prefixed_key, tables, BranchKey, BranchNode, FieldValue, LeafValue, MergeValue,
    PersistenceError, TreeKey, TreeNamespace,
};

/// Borrow an externally created scratch transaction. The adapter neither opens
/// a database nor commits it, and never receives the read-only source handle.
pub(super) struct ScratchTreeStore<'a, T: DbTx + DbTxMut> {
    tx: &'a T,
    namespace: TreeNamespace,
}

impl<'a, T: DbTx + DbTxMut> ScratchTreeStore<'a, T> {
    pub(super) const fn new(tx: &'a T, namespace: TreeNamespace) -> Self {
        Self { tx, namespace }
    }
}

impl<T: DbTx + DbTxMut> StoreReadOps<H256> for ScratchTreeStore<'_, T> {
    fn get_branch(&self, key: &CkbBranchKey) -> Result<Option<CkbBranchNode>, CkbError> {
        let key = branch_key(key).map_err(store_error)?;
        self.tx
            .get::<tables::CeBranches>(prefixed_key(self.namespace, &key.encode()))
            .map_err(store_error)?
            .map(|bytes| {
                BranchNode::decode(&bytes)
                    .map(to_ckb_branch)
                    .map_err(store_error)
            })
            .transpose()
    }

    fn get_leaf(&self, key: &H256) -> Result<Option<H256>, CkbError> {
        let key = tree_key(*key).map_err(store_error)?;
        self.tx
            .get::<tables::CeLeaves>(prefixed_key(self.namespace, &key.encode()))
            .map_err(store_error)?
            .map(|bytes| {
                LeafValue::decode(&bytes)
                    .map(|leaf| H256::from(leaf.encode()))
                    .map_err(store_error)
            })
            .transpose()
    }
}

impl<T: DbTx + DbTxMut> StoreWriteOps<H256> for ScratchTreeStore<'_, T> {
    fn insert_branch(&mut self, key: CkbBranchKey, branch: CkbBranchNode) -> Result<(), CkbError> {
        let key = branch_key(&key).map_err(store_error)?;
        let node = BranchNode {
            left: from_ckb_merge(&branch.left).map_err(store_error)?,
            right: from_ckb_merge(&branch.right).map_err(store_error)?,
        };
        self.tx
            .put::<tables::CeBranches>(prefixed_key(self.namespace, &key.encode()), node.encode())
            .map_err(store_error)
    }

    fn insert_leaf(&mut self, key: H256, leaf: H256) -> Result<(), CkbError> {
        let key = tree_key(key).map_err(store_error)?;
        let leaf = LeafValue::try_from(B256::from(<[u8; 32]>::from(leaf))).map_err(store_error)?;
        self.tx
            .put::<tables::CeLeaves>(
                prefixed_key(self.namespace, &key.encode()),
                leaf.encode().to_vec(),
            )
            .map_err(store_error)
    }

    fn remove_branch(&mut self, key: &CkbBranchKey) -> Result<(), CkbError> {
        let key = branch_key(key).map_err(store_error)?;
        self.tx
            .delete::<tables::CeBranches>(prefixed_key(self.namespace, &key.encode()), None)
            .map_err(store_error)?;
        Ok(())
    }

    fn remove_leaf(&mut self, key: &H256) -> Result<(), CkbError> {
        let key = tree_key(*key).map_err(store_error)?;
        self.tx
            .delete::<tables::CeLeaves>(prefixed_key(self.namespace, &key.encode()), None)
            .map_err(store_error)?;
        Ok(())
    }
}

fn tree_key(key: H256) -> Result<TreeKey, PersistenceError> {
    TreeKey::try_from(B256::from(<[u8; 32]>::from(key)))
}

fn branch_key(key: &CkbBranchKey) -> Result<BranchKey, PersistenceError> {
    BranchKey::new(key.height, B256::from(<[u8; 32]>::from(key.node_key)))
}

fn from_ckb_merge(value: &CkbMergeValue) -> Result<MergeValue, PersistenceError> {
    let field = |value: H256| FieldValue::try_from(B256::from(<[u8; 32]>::from(value)));
    match value {
        CkbMergeValue::Value(value) => Ok(MergeValue::Value(field(*value)?)),
        CkbMergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => Ok(MergeValue::MergeWithZero {
            base_node: field(*base_node)?,
            zero_bits: field(*zero_bits)?,
            zero_count: *zero_count,
        }),
    }
}

fn to_ckb_merge(value: MergeValue) -> CkbMergeValue {
    match value {
        MergeValue::Value(value) => CkbMergeValue::Value(H256::from(value.encode())),
        MergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => CkbMergeValue::MergeWithZero {
            base_node: H256::from(base_node.encode()),
            zero_bits: H256::from(zero_bits.encode()),
            zero_count,
        },
    }
}

fn to_ckb_branch(node: BranchNode) -> CkbBranchNode {
    CkbBranchNode {
        left: to_ckb_merge(node.left),
        right: to_ckb_merge(node.right),
    }
}

fn store_error(error: impl std::fmt::Display) -> CkbError {
    CkbError::Store(error.to_string())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use outbe_sparse_merkle_tree_v061::{
        merge::MergeValue as CkbMergeValue,
        traits::{StoreReadOps, StoreWriteOps},
        BranchKey as CkbBranchKey, BranchNode as CkbBranchNode, H256,
    };
    use reth_db::{
        database::Database,
        mdbx::{create_db, DatabaseArguments},
        transaction::{DbTx, DbTxMut},
        DatabaseEnv,
    };

    use super::ScratchTreeStore;
    use crate::persistence::{
        prefixed_key, tables, BranchKey, BranchNode, FieldValue, LeafValue, MergeValue, TreeKey,
        TreeNamespace,
    };

    fn scratch() -> (tempfile::TempDir, DatabaseEnv) {
        let directory = tempfile::tempdir().unwrap();
        let mut db = create_db(directory.path(), DatabaseArguments::test()).unwrap();
        db.create_and_track_tables_for::<tables::CeTables>()
            .unwrap();
        (directory, db)
    }

    fn hash(value: u8) -> H256 {
        H256::from(B256::with_last_byte(value).0)
    }

    #[test]
    fn scratch_round_trips_native_records_updates_removes_and_isolates_namespaces() {
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let mut store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let key = CkbBranchKey {
            height: 7,
            node_key: hash(0),
        };
        let branch = CkbBranchNode {
            left: CkbMergeValue::Value(hash(5)),
            right: CkbMergeValue::MergeWithZero {
                base_node: hash(6),
                zero_bits: hash(7),
                zero_count: 8,
            },
        };
        assert!(store.get_branch(&key).unwrap().is_none());
        store.insert_branch(key.clone(), branch.clone()).unwrap();
        assert_eq!(store.get_branch(&key).unwrap(), Some(branch));
        let encoded_key = BranchKey::new(7, B256::ZERO).unwrap();
        let persisted = tx
            .get::<tables::CeBranches>(prefixed_key(TreeNamespace::Catalog, &encoded_key.encode()))
            .unwrap()
            .unwrap();
        assert_eq!(
            BranchNode::decode(&persisted).unwrap(),
            BranchNode {
                left: MergeValue::Value(FieldValue::try_from(B256::with_last_byte(5)).unwrap()),
                right: MergeValue::MergeWithZero {
                    base_node: FieldValue::try_from(B256::with_last_byte(6)).unwrap(),
                    zero_bits: FieldValue::try_from(B256::with_last_byte(7)).unwrap(),
                    zero_count: 8,
                },
            }
        );
        store.insert_leaf(hash(1), hash(9)).unwrap();
        store.insert_leaf(hash(1), hash(10)).unwrap();
        assert_eq!(store.get_leaf(&hash(1)).unwrap(), Some(hash(10)));
        assert_eq!(tx.entries::<tables::CeLeaves>().unwrap(), 1);
        let leaf_key = TreeKey::try_from(B256::with_last_byte(1)).unwrap();
        let leaf = tx
            .get::<tables::CeLeaves>(prefixed_key(TreeNamespace::Catalog, &leaf_key.encode()))
            .unwrap()
            .unwrap();
        assert_eq!(
            LeafValue::decode(&leaf).unwrap().into_inner(),
            B256::with_last_byte(10)
        );

        let collection = crate::CollectionKey::try_from(B256::with_last_byte(1)).unwrap();
        let mut other = ScratchTreeStore::new(&tx, TreeNamespace::CollectionShard(collection, 0));
        assert!(other.get_leaf(&hash(1)).unwrap().is_none());
        assert!(other.get_branch(&key).unwrap().is_none());
        other.insert_leaf(hash(1), hash(11)).unwrap();
        store.remove_leaf(&hash(1)).unwrap();
        store.remove_branch(&key).unwrap();
        assert!(store.get_leaf(&hash(1)).unwrap().is_none());
        assert!(store.get_branch(&key).unwrap().is_none());
        assert_eq!(other.get_leaf(&hash(1)).unwrap(), Some(hash(11)));
        store.remove_leaf(&hash(1)).unwrap();
        store.remove_branch(&key).unwrap();
        assert_eq!(tx.entries::<tables::CeTreeRoots>().unwrap(), 0);
        assert_eq!(tx.entries::<tables::CeMetadata>().unwrap(), 0);
    }

    #[test]
    fn scratch_rejects_noncanonical_fields_before_writing() {
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let mut store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let invalid = H256::from([0xff; 32]);
        assert!(store.insert_leaf(invalid, hash(1)).is_err());
        assert!(store.insert_leaf(hash(1), invalid).is_err());
        assert!(store.insert_leaf(hash(1), H256::zero()).is_err());
        assert!(store.get_leaf(&invalid).is_err());
        assert!(store.remove_leaf(&invalid).is_err());
        for invalid_branch in [
            CkbMergeValue::Value(invalid),
            CkbMergeValue::MergeWithZero {
                base_node: invalid,
                zero_bits: hash(0),
                zero_count: 1,
            },
            CkbMergeValue::MergeWithZero {
                base_node: hash(1),
                zero_bits: invalid,
                zero_count: 1,
            },
        ] {
            assert!(store
                .insert_branch(
                    CkbBranchKey {
                        height: 1,
                        node_key: hash(0)
                    },
                    CkbBranchNode {
                        left: CkbMergeValue::Value(hash(0)),
                        right: invalid_branch
                    },
                )
                .is_err());
        }
        let key = CkbBranchKey {
            height: 1,
            node_key: invalid,
        };
        assert!(store.get_branch(&key).is_err());
        assert!(store.remove_branch(&key).is_err());
        assert_eq!(tx.entries::<tables::CeBranches>().unwrap(), 0);
        assert_eq!(tx.entries::<tables::CeLeaves>().unwrap(), 0);
    }

    #[test]
    fn scratch_reads_reject_malformed_native_values() {
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let leaf_key = TreeKey::try_from(B256::with_last_byte(1)).unwrap();
        tx.put::<tables::CeLeaves>(
            prefixed_key(TreeNamespace::Catalog, &leaf_key.encode()),
            vec![0; 32],
        )
        .unwrap();
        assert!(store.get_leaf(&hash(1)).is_err());
        let key = BranchKey::new(1, B256::ZERO).unwrap();
        tx.put::<tables::CeBranches>(prefixed_key(TreeNamespace::Catalog, &key.encode()), vec![0])
            .unwrap();
        assert!(store
            .get_branch(&CkbBranchKey {
                height: 1,
                node_key: hash(0)
            })
            .is_err());
    }

    #[test]
    fn poseidon_tree_uses_the_borrowed_native_store() {
        use crate::smt::{PoseidonSmt, TreeKey, TreeLeaf, TreeRoot};
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
        let mut expected = PoseidonSmt::empty();
        for (key, value) in [(1, 4), (2, 5), (1, 6)] {
            let key = TreeKey::from_be_bytes(B256::with_last_byte(key).0).unwrap();
            let leaf = TreeLeaf::from_be_bytes(B256::with_last_byte(value).0).unwrap();
            assert_eq!(
                tree.update(key, leaf).unwrap(),
                expected.update(key, leaf).unwrap()
            );
        }
        for value in [1, 2] {
            let key = TreeKey::from_be_bytes(B256::with_last_byte(value).0).unwrap();
            assert_eq!(
                tree.update(key, TreeLeaf::ZERO).unwrap(),
                expected.update(key, TreeLeaf::ZERO).unwrap()
            );
        }
        assert_eq!(tree.root().unwrap(), TreeRoot::EMPTY);
        assert_eq!(tx.entries::<tables::CeLeaves>().unwrap(), 0);
    }

    #[test]
    fn audit_rejects_a_wrong_shard_even_when_every_root_and_branch_is_consistent() {
        use super::super::{CeAuditError, CeAuditLimits, CeAuditVisitor, CeAuditWork};
        use crate::{
            persistence::{
                CeMdbx, CeMdbxReadOnly, EnvironmentIdentity, ExactParentIdentity, FinalizedMarker,
                LAST_APPLIED_KEY, LOCAL_STORAGE_SCHEMA_VERSION,
            },
            sharding::{aggregate_b256_shard_roots, shard_index},
            smt::{derive_tree_key, PoseidonSmt, TreeKey as SmtKey, TreeLeaf, TreeRoot},
            CeDomain, CeTopologyV1, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, K_PROVISIONAL,
        };

        struct Observe;
        impl CeAuditVisitor for Observe {
            fn visit_leaf(
                &mut self,
                _: TreeNamespace,
                _: TreeKey,
                _: LeafValue,
            ) -> Result<(), CeAuditError> {
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let genesis_hash = B256::with_last_byte(42);
        let identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 10,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        };
        let genesis = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: crate::sealed_root(B256::ZERO).unwrap(),
        };
        let writer = CeMdbx::open(directory.path(), identity.clone(), genesis).unwrap();
        let tx = writer.db.tx_mut().unwrap();
        let mut raw_id = [0; 32];
        raw_id[..4].copy_from_slice(&20_260_718_u32.to_be_bytes());
        raw_id[31] = 1;
        let id = WwdEntityId::try_from(raw_id.as_slice()).unwrap();
        let collection = crate::collection_key(CeDomain::Tribute, id).unwrap();
        let key = derive_tree_key(crate::schema::Collection::Tribute, id).unwrap();
        let correct_shard = shard_index(key, K_PROVISIONAL).unwrap();
        let wrong_shard = (correct_shard + 1) % K_PROVISIONAL;
        assert_ne!(correct_shard, wrong_shard);

        // Deliberately bypass the production writer's shard envelope checks.
        // Rebuild all native branch records and commitments so hash checks alone
        // cannot distinguish this malformed namespace placement.
        let mut roots = vec![B256::ZERO; K_PROVISIONAL as usize];
        {
            let store =
                ScratchTreeStore::new(&tx, TreeNamespace::CollectionShard(collection, wrong_shard));
            let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
            roots[wrong_shard as usize] = B256::from(
                tree.update(
                    key,
                    TreeLeaf::from_be_bytes(B256::with_last_byte(7).0).unwrap(),
                )
                .unwrap()
                .as_bytes(),
            );
        }
        for shard in 0..K_PROVISIONAL {
            tx.put::<tables::CeTreeRoots>(
                TreeNamespace::CollectionShard(collection, shard).encode(),
                roots[shard as usize].to_vec(),
            )
            .unwrap();
        }
        let collection_root = crate::collection_root(
            CeDomain::Tribute,
            collection,
            aggregate_b256_shard_roots(&roots).unwrap(),
        )
        .unwrap();
        let catalog_root = {
            let store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
            let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
            B256::from(
                tree.update(
                    SmtKey::from_be_bytes(*collection.as_bytes()).unwrap(),
                    TreeLeaf::from_be_bytes(collection_root.0).unwrap(),
                )
                .unwrap()
                .as_bytes(),
            )
        };
        tx.put::<tables::CeTreeRoots>(TreeNamespace::Catalog.encode(), catalog_root.to_vec())
            .unwrap();
        let marker = FinalizedMarker {
            height: 1,
            block_hash: B256::with_last_byte(43),
            parent_block_hash: genesis.block_hash,
            parent_root: genesis.new_root,
            new_root: crate::sealed_root(catalog_root).unwrap(),
            ..genesis
        };
        tx.put::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec(), marker.encode().to_vec())
            .unwrap();
        tx.commit().unwrap();
        drop(writer);

        let reader = CeMdbxReadOnly::open(directory.path(), identity).unwrap();
        let required = ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        };
        reader.open_exact(required).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let work = CeAuditWork::create(
            scratch.path().join("audit"),
            CeAuditLimits {
                records_per_run: 2,
                merge_fan_in: 2,
            },
        )
        .unwrap();
        assert!(
            reader.audit_exact(required, &work, &mut Observe).is_err(),
            "accepted a coherent tree whose leaf is in the wrong native shard",
        );
    }
}
