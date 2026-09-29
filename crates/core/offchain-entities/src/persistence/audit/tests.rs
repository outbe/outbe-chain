use std::sync::Arc;

use alloy_primitives::B256;
use reth_db::{
    cursor::DbCursorRO,
    database::Database,
    transaction::{DbTx, DbTxMut},
};

use super::{CeAuditError, CeAuditLimits, CeAuditReport, CeAuditVisitor, CeAuditWork};
use crate::persistence::{
    tables, BranchKey, BranchNode, CeMdbx, CeMdbxReadOnly, EnvironmentIdentity,
    ExactParentIdentity, FieldValue, FinalizedMarker, LeafValue, MergeValue, TreeKey,
    TreeNamespace, LOCAL_STORAGE_SCHEMA_VERSION,
};
use crate::{
    api::{AuthenticatedParentTree, EntityRef, FinalLeafMutation},
    collection_key, sealed_root, CeDomain, CeTopologyV1, Commitment, MdbxAuthenticatedTree,
    WwdEntityId, ACTIVE_COMMITMENT_SCHEME, K_PROVISIONAL,
};

struct Fixture {
    directory: tempfile::TempDir,
    db: Arc<CeMdbx>,
    identity: EnvironmentIdentity,
    marker: FinalizedMarker,
    id: WwdEntityId,
}

impl Fixture {
    fn new(populated: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let genesis_hash = B256::repeat_byte(0x30);
        let identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 10,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        };
        let marker = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: sealed_root(B256::ZERO).unwrap(),
        };
        let db = Arc::new(CeMdbx::open(directory.path(), identity.clone(), marker).unwrap());
        let mut raw = [0; 32];
        raw[..4].copy_from_slice(&20_260_718_u32.to_be_bytes());
        raw[31] = 7;
        let id = WwdEntityId::try_from(raw.as_slice()).unwrap();
        let mut fixture = Self {
            directory,
            db,
            identity,
            marker,
            id,
        };
        if populated {
            let mutations = [
                EntityRef::Tribute(id),
                EntityRef::NodItem(id),
                EntityRef::NodBucket(id),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, entity)| FinalLeafMutation {
                entity,
                final_leaf: Some(
                    Commitment::try_from(B256::with_last_byte(index as u8 + 1).0).unwrap(),
                ),
            })
            .collect::<Vec<_>>();
            fixture.apply(&mutations);
        }
        fixture
    }

    fn required(&self) -> ExactParentIdentity {
        ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: self.marker.height,
            block_hash: self.marker.block_hash,
            root: self.marker.new_root,
        }
    }

    fn apply(&mut self, mutations: &[FinalLeafMutation]) {
        let parent = MdbxAuthenticatedTree::open(self.db.clone(), self.required()).unwrap();
        let number = self.marker.height + 1;
        let staged = parent
            .prepare_seal(number, mutations, &[])
            .unwrap()
            .freeze(B256::with_last_byte(number as u8));
        self.db.apply_finalized(&staged).unwrap();
        self.marker = staged.marker(ACTIVE_COMMITMENT_SCHEME);
    }

    fn audit(self, visitor: &mut impl CeAuditVisitor) -> Result<CeAuditReport, CeAuditError> {
        let required = self.required();
        drop(self.db);
        let before = fingerprint(self.directory.path());
        let result = (|| {
            let reader = CeMdbxReadOnly::open(self.directory.path(), self.identity)?;
            let scratch = tempfile::tempdir().unwrap();
            let work = CeAuditWork::create(
                scratch.path().join("audit"),
                CeAuditLimits {
                    records_per_run: 2,
                    merge_fan_in: 2,
                },
            )?;
            reader.audit_exact(required, &work, visitor)
        })();
        assert_eq!(fingerprint(self.directory.path()), before);
        result
    }
}

fn fingerprint(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, (u64, u32, u64)> {
    use std::{hash::Hasher, io::Read, os::unix::fs::PermissionsExt};
    let mut pending = vec![root.to_path_buf()];
    let mut result = std::collections::BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let mut digest = std::hash::DefaultHasher::new();
            let bookkeeping = entry.file_name() == "mdbx.lck";
            if metadata.is_dir() {
                pending.push(path.clone());
            } else if !bookkeeping {
                assert!(metadata.is_file());
                let mut file = std::fs::File::open(&path).unwrap();
                let mut buffer = [0; 65536];
                loop {
                    let count = file.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    digest.write(&buffer[..count]);
                }
            }
            result.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (
                    if bookkeeping { 0 } else { metadata.len() },
                    metadata.permissions().mode(),
                    digest.finish(),
                ),
            );
        }
    }
    result
}

#[derive(Default)]
struct Leaves(Vec<(TreeNamespace, TreeKey, LeafValue)>);

impl CeAuditVisitor for Leaves {
    fn visit_leaf(
        &mut self,
        namespace: TreeNamespace,
        key: TreeKey,
        value: LeafValue,
    ) -> Result<(), CeAuditError> {
        assert!(!self
            .0
            .iter()
            .any(|entry| entry.0 == namespace && entry.1 == key));
        self.0.push((namespace, key, value));
        Ok(())
    }
}

#[test]
fn exhaustive_audit_accepts_native_genesis_and_all_three_nonzero_collections() {
    for populated in [false, true] {
        let fixture = Fixture::new(populated);
        let root = fixture.marker.new_root;
        let mut leaves = Leaves::default();
        let report = fixture.audit(&mut leaves).unwrap();
        assert_eq!(report.sealed_root, root);
        assert_eq!(
            report.trees,
            if populated {
                1 + 3 * u64::from(K_PROVISIONAL)
            } else {
                1
            }
        );
        assert_eq!(report.leaves, if populated { 6 } else { 0 });
        assert_eq!(leaves.0.len() as u64, report.leaves);
        assert!(report.peak_buffered_leaves <= 2);
    }
}

#[test]
fn exhaustive_audit_accepts_materialized_empty_shards_without_retiring_collections() {
    let mut fixture = Fixture::new(true);
    fixture.apply(&[FinalLeafMutation {
        entity: EntityRef::Tribute(fixture.id),
        final_leaf: None,
    }]);
    let report = fixture.audit(&mut Leaves::default()).unwrap();
    assert_eq!(report.trees, 1 + 3 * u64::from(K_PROVISIONAL));
    assert_eq!(report.leaves, 5);
}

#[test]
fn retired_collection_is_absent_while_other_domains_remain_auditable() {
    let mut fixture = Fixture::new(true);
    let parent = MdbxAuthenticatedTree::open(fixture.db.clone(), fixture.required()).unwrap();
    let staged = parent
        .prepare_seal(
            2,
            &[],
            &[crate::PartitionRef::TributeWwd(fixture.id.worldwide_day())],
        )
        .unwrap()
        .freeze(B256::with_last_byte(2));
    fixture.db.apply_finalized(&staged).unwrap();
    fixture.marker = staged.marker(ACTIVE_COMMITMENT_SCHEME);
    drop(parent);
    let report = fixture.audit(&mut Leaves::default()).unwrap();
    assert_eq!(
        (report.trees, report.leaves),
        (1 + 2 * u64::from(K_PROVISIONAL), 4)
    );
}

#[test]
fn missing_catalog_root_is_corruption_even_for_an_otherwise_empty_store() {
    let fixture = Fixture::new(false);
    let tx = fixture.db.db.tx_mut().unwrap();
    tx.delete::<tables::CeTreeRoots>(TreeNamespace::Catalog.encode(), None)
        .unwrap();
    tx.commit().unwrap();
    assert!(fixture.audit(&mut Leaves::default()).is_err());
}

#[test]
fn unchanged_marker_cannot_hide_corrupt_leaves_branches_or_collection_population() {
    for corrupt in 0..7 {
        let fixture = Fixture::new(true);
        let tx = fixture.db.db.tx_mut().unwrap();
        match corrupt {
            0 => {
                let (key, _) = tx
                    .cursor_read::<tables::CeLeaves>()
                    .unwrap()
                    .first()
                    .unwrap()
                    .unwrap();
                tx.put::<tables::CeLeaves>(key, B256::with_last_byte(99).to_vec())
                    .unwrap();
            }
            1 => {
                let (key, _) = tx
                    .cursor_read::<tables::CeBranches>()
                    .unwrap()
                    .first()
                    .unwrap()
                    .unwrap();
                tx.delete::<tables::CeBranches>(key, None).unwrap();
            }
            2 => {
                let key = BranchKey::new(0, B256::with_last_byte(21)).unwrap();
                let node = BranchNode {
                    left: MergeValue::Value(FieldValue::try_from(B256::with_last_byte(1)).unwrap()),
                    right: MergeValue::Value(FieldValue::try_from(B256::ZERO).unwrap()),
                };
                tx.put::<tables::CeBranches>(
                    crate::persistence::prefixed_key(TreeNamespace::Catalog, &key.encode()),
                    node.encode(),
                )
                .unwrap();
            }
            3 => {
                let key = collection_key(CeDomain::Tribute, fixture.id).unwrap();
                let empty_shard = (0..K_PROVISIONAL)
                    .find(|shard| {
                        tx.get::<tables::CeTreeRoots>(
                            TreeNamespace::CollectionShard(key, *shard).encode(),
                        )
                        .unwrap()
                            == Some(B256::ZERO.to_vec())
                    })
                    .unwrap();
                tx.delete::<tables::CeTreeRoots>(
                    TreeNamespace::CollectionShard(key, empty_shard).encode(),
                    None,
                )
                .unwrap();
            }
            4 => {
                tx.put::<tables::CeLeaves>(vec![99; 33], B256::with_last_byte(1).to_vec())
                    .unwrap();
            }
            5 => {
                let (key, _) = tx
                    .cursor_read::<tables::CeLeaves>()
                    .unwrap()
                    .first()
                    .unwrap()
                    .unwrap();
                tx.put::<tables::CeLeaves>(key, vec![255; 32]).unwrap();
            }
            6 => {
                let key = collection_key(CeDomain::Tribute, fixture.id).unwrap();
                tx.delete::<tables::CeLeaves>(
                    crate::persistence::prefixed_key(TreeNamespace::Catalog, key.as_bytes()),
                    None,
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        tx.commit().unwrap();
        assert!(
            fixture.audit(&mut Leaves::default()).is_err(),
            "accepted corruption {corrupt}"
        );
    }
}

#[test]
fn genesis_cannot_have_materialized_collections_or_a_different_block_hash() {
    for populated in [false, true] {
        let mut fixture = Fixture::new(populated);
        fixture.marker.height = 0;
        fixture.marker.block_hash = if populated {
            fixture.identity.genesis_hash
        } else {
            B256::repeat_byte(99)
        };
        fixture.marker.parent_block_hash = B256::ZERO;
        fixture.marker.parent_root = B256::ZERO;
        let tx = fixture.db.db.tx_mut().unwrap();
        tx.put::<tables::CeMetadata>(
            crate::persistence::LAST_APPLIED_KEY.to_vec(),
            fixture.marker.encode().to_vec(),
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(
            fixture.audit(&mut Leaves::default()).is_err(),
            "accepted invalid genesis, populated={populated}"
        );
    }
}

#[test]
fn visitor_failure_aborts_the_audit() {
    struct Stop;
    impl CeAuditVisitor for Stop {
        fn visit_leaf(
            &mut self,
            _: TreeNamespace,
            _: TreeKey,
            _: LeafValue,
        ) -> Result<(), CeAuditError> {
            Err(CeAuditError::Invalid("visitor stopped".into()))
        }
    }
    let error = Fixture::new(true).audit(&mut Stop).unwrap_err();
    assert!(error.to_string().contains("visitor stopped"));
}
