//! Day directories travel with the off-chain snapshot inventory.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy_primitives::B256;
use outbe_offchain_data::{
    read_projection_state, DayDatabaseRoute, OffchainDataProjection, ProjectionConfig,
};
use outbe_offchain_storage::{DayDatabases, Key, Namespace, RocksDbReader, StorageWriter, Value};
use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_snapshot::manifest::DomainKind;
use outbe_tribute::{read_tribute_day_mark, write_tribute_day_mark, TributeDayMark};

use super::super::inventory::enumerate_native_files;
use super::super::projection_store::projection_database;
use super::native::fixture;

#[test]
fn snapshot_restores_live_days_and_sweeps_pending() {
    let (root, layout, _, _) = fixture();
    let genesis_hash = layout.chain.genesis_hash();
    for path in [
        layout.chain_root.join("compressed_entities/smt/mdbx.dat"),
        layout
            .ocomp_root
            .join("exporter-v1/discovery/closure-checkpoint-v1/checkpoint.v1"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"native").unwrap();
    }

    let databases = DayDatabases::open(&layout.offchain_root).unwrap();
    let shared = Arc::new(databases.directory().open_shared().unwrap());
    drop(databases.tribute(7).unwrap());
    drop(databases.tribute(8).unwrap());
    drop(databases.nod(7).unwrap());
    let state = outbe_offchain_data::ProjectionState {
        chain_id: layout.chain.chain().id(),
        genesis_hash,
        storage_schema_version: outbe_offchain_data::STORAGE_SCHEMA_VERSION,
        start_block: layout.projection_start_block,
        checkpoint: Some(ProjectionCheckpoint {
            block_number: 98,
            block_hash: B256::repeat_byte(98),
        }),
    };
    shared
        .put(
            Namespace::new("projection_state").unwrap(),
            &Key::new(b"offchain_data".to_vec()).unwrap(),
            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
        )
        .unwrap();
    write_tribute_day_mark(shared.as_ref(), 8, TributeDayMark::DropPending).unwrap();
    let live_day = fs::read(layout.offchain_root.join("tribute-days/7/CURRENT")).unwrap();
    drop(shared);
    drop(databases);

    let database = projection_database(&layout.offchain_root);
    assert_eq!(database, layout.offchain_root.join("shared"));
    let reader =
        Arc::new(RocksDbReader::open(&database, &root.path().join("reader-scratch")).unwrap());
    let checkpoint = read_projection_state(
        ProjectionConfig {
            chain_id: layout.chain.chain().id(),
            genesis_hash,
            start_block: layout.projection_start_block,
        },
        reader,
    )
    .unwrap()
    .unwrap()
    .checkpoint
    .unwrap();
    assert_eq!(checkpoint.block_number, 98);

    let inventory = enumerate_native_files(&layout).unwrap();
    let offchain = inventory
        .domains
        .iter()
        .find(|domain| domain.kind == DomainKind::OffchainProjection)
        .unwrap();
    for relative in [
        "shared/CURRENT",
        "tribute-days/7/CURRENT",
        "tribute-days/8/CURRENT",
        "nod-days/7/CURRENT",
    ] {
        assert!(
            offchain
                .members
                .iter()
                .any(|path| path == Path::new(relative)),
            "missing {relative}"
        );
    }

    // The signed archive copies this member list. Placement uses the same list.
    let staged = root.path().join("staged-offchain");
    place(&layout.offchain_root, &offchain.members, &staged);
    fs::remove_dir_all(&layout.offchain_root).unwrap();
    assert!(!layout.offchain_root.exists());
    place(&staged, &offchain.members, &layout.offchain_root);
    assert_eq!(
        fs::read(layout.offchain_root.join("tribute-days/7/CURRENT")).unwrap(),
        live_day
    );
    assert!(layout
        .offchain_root
        .join("tribute-days/8/CURRENT")
        .is_file());
    assert!(layout.offchain_root.join("nod-days/7/CURRENT").is_file());

    let restored = DayDatabases::open(&layout.offchain_root).unwrap();
    let shared = Arc::new(restored.directory().open_shared().unwrap());
    let mut projection = OffchainDataProjection::open(
        ProjectionConfig {
            chain_id: layout.chain.chain().id(),
            genesis_hash,
            start_block: layout.projection_start_block,
        },
        shared.clone(),
        shared.clone(),
    )
    .unwrap();
    projection
        .set_day_route(DayDatabaseRoute {
            databases: Arc::new(restored),
            durable_reader: shared.clone(),
            durable_writer: shared.clone(),
        })
        .unwrap();
    assert!(layout
        .offchain_root
        .join("tribute-days/7/CURRENT")
        .is_file());
    assert!(!layout.offchain_root.join("tribute-days/8").exists());
    assert!(layout.offchain_root.join("nod-days/7/CURRENT").is_file());
    assert_eq!(
        read_tribute_day_mark(shared.as_ref(), 8).unwrap(),
        Some(TributeDayMark::DropPending)
    );
}

#[test]
fn legacy_projection_database_stays_at_the_root() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("CURRENT"), b"MANIFEST-000001\n").unwrap();
    assert_eq!(projection_database(root.path()), root.path());
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
