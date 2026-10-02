//! Entity partitions travel together with the signed off-chain snapshot inventory.
use super::super::{inventory::enumerate_native_files, projection_store::projection_database};
use super::native::fixture;
use outbe_offchain_storage::partitioned::adapters::{
    RocksPartitionDataSource, RocksPartitionReadView,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, Key, Namespace, PartitionedStorage, StorageReader, StorageScope,
    StorageWriter, Value,
};
use outbe_snapshot::manifest::DomainKind;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[test]
fn snapshot_restores_entity_hierarchy_and_retirement_preserves_nod_shards() {
    let (root, layout, _, _) = fixture();
    for path in [
        layout.chain_root.join("compressed_entities/smt/mdbx.dat"),
        layout
            .ocomp_root
            .join("exporter-v1/discovery/closure-checkpoint-v1/checkpoint.v1"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"native").unwrap();
    }
    let ns = Namespace::new("fixture").unwrap();
    let key = Key::new([1]).unwrap();
    let tribute7 = StorageScope::numbered("tribute", "wwd", 7).unwrap();
    let tribute8 = StorageScope::numbered("tribute", "wwd", 8).unwrap();
    let nod = StorageScope::numbered("nod", "nod-shards", 17).unwrap();
    {
        let source = Arc::new(RocksPartitionDataSource::open(&layout.offchain_root).unwrap());
        let storage = PartitionedStorage::new(
            source,
            outbe_offchain_data::entity_partition_routing().unwrap(),
        );
        for scope in [&tribute7, &tribute8, &nod] {
            storage
                .put(
                    ns.clone().with_scope(scope.clone()),
                    &key,
                    &Value::new([7]).unwrap(),
                )
                .unwrap();
        }
    }
    assert_eq!(
        projection_database(&layout.offchain_root),
        layout.offchain_root.join("system/shared")
    );
    let inventory = enumerate_native_files(&layout).unwrap();
    let offchain = inventory
        .domains
        .iter()
        .find(|domain| domain.kind == DomainKind::OffchainProjection)
        .unwrap();
    for relative in [
        "system/shared/CURRENT",
        "tribute/wwd/7/CURRENT",
        "tribute/wwd/8/CURRENT",
        "nod/nod-shards/17/CURRENT",
    ] {
        assert!(
            offchain
                .members
                .iter()
                .any(|path| path == Path::new(relative)),
            "missing {relative}"
        );
    }
    let staged = root.path().join("staged-offchain");
    place(&layout.offchain_root, &offchain.members, &staged);
    fs::remove_dir_all(&layout.offchain_root).unwrap();
    place(&staged, &offchain.members, &layout.offchain_root);
    {
        let source = Arc::new(RocksPartitionDataSource::open(&layout.offchain_root).unwrap());
        let storage = PartitionedStorage::new(
            source,
            outbe_offchain_data::entity_partition_routing().unwrap(),
        );
        let mut batch = AtomicWriteBatch::new();
        batch.retire_scope(tribute8.clone());
        storage.apply_atomic(&batch).unwrap();
        storage.apply_atomic(&batch).unwrap();
    }
    assert!(!layout.offchain_root.join("tribute/wwd/8").exists());
    let scratch = tempfile::tempdir().unwrap();
    let storage = PartitionedStorage::read_only(
        Arc::new(RocksPartitionReadView::open(&layout.offchain_root, scratch.path()).unwrap()),
        outbe_offchain_data::entity_partition_routing().unwrap(),
    );
    assert!(storage
        .get(ns.clone().with_scope(tribute7), &key)
        .unwrap()
        .is_some());
    assert!(storage.get(ns.with_scope(nod), &key).unwrap().is_some());
}

#[test]
fn legacy_projection_root_is_rejected_without_moving_its_files() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("CURRENT"), b"MANIFEST-000001\n").unwrap();
    assert!(RocksPartitionDataSource::open(root.path()).is_err());
    assert!(root.path().join("CURRENT").is_file());
    assert!(!projection_database(root.path()).exists());
}

fn place(source_root: &Path, members: &[PathBuf], destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for relative in members {
        let source = source_root.join(relative);
        let dest = destination.join(relative);
        if source.is_dir() {
            fs::create_dir_all(&dest).unwrap();
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::copy(&source, &dest).unwrap();
        }
    }
}
