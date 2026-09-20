use std::fs;

use alloy_consensus::{Header, Sealable};
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    table::Table,
    tables::{self, ChainStateKey},
    transaction::{DbTx, DbTxMut},
};

use super::super::{
    config::{parse_node_inputs, resolve_layout},
    native::inspect_reth,
};
use crate::OutbeHeader;

type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;

fn fixture() -> (
    tempfile::TempDir,
    super::super::config::NativeLayout,
    OutbeHeader,
    OutbeHeader,
) {
    let root = tempfile::tempdir().unwrap();
    let inputs = parse_node_inputs(super::layout::native_arguments(root.path())).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    fs::create_dir_all(&layout.static_files_root).unwrap();
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let h = OutbeHeader::new(Header {
        number: 100,
        ..Default::default()
    });
    let e = OutbeHeader::new(Header {
        number: 101,
        parent_hash: h.hash_slow(),
        ..Default::default()
    });
    let tx = db.tx_mut().unwrap();
    for header in [layout.chain.genesis_header().clone(), h.clone(), e.clone()] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
            .unwrap();
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 100)
        .unwrap();
    tx.put::<tables::StageCheckpoints>("Execution".into(), StageCheckpoint::new(101))
        .unwrap();
    tx.put::<tables::StageCheckpoints>("Finish".into(), StageCheckpoint::new(100))
        .unwrap();
    tx.put::<tables::Metadata>(
        "storage_settings".into(),
        br#"{"storage_v2":true}"#.to_vec(),
    )
    .unwrap();
    tx.put::<tables::Metadata>(
        "partial_state_trie_unwind".into(),
        br#"{"finish_block_number":100,"partial_state_trie":99}"#.to_vec(),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);

    (root, layout, h, e)
}

#[test]
fn reth_inspection_preserves_finalized_execution_and_unwind_observations() {
    let (_root, layout, h, e) = fixture();
    let before = fingerprint(&layout.chain_root);
    let progress = inspect_reth(&layout).unwrap();
    assert_eq!(progress.finalized.number, 100);
    assert_eq!(progress.finalized.hash, hex::encode(h.hash_slow()));
    assert_eq!(progress.execution.number, 101);
    assert_eq!(progress.execution.hash, hex::encode(e.hash_slow()));
    assert_eq!(progress.execution_stage, Some(101));
    assert_eq!(progress.finish_stage, Some(100));
    assert_eq!(progress.partial_state_trie, None);
    assert_eq!(progress.storage_version, 2);
    let unwind = progress.unwind.unwrap();
    assert_eq!(unwind.finish_block_number, 100);
    assert_eq!(unwind.partial_state_trie, 99);
    assert_eq!(fingerprint(&layout.chain_root), before);
}

// MDBX reader slots live in its existing lock file, not persistent chain state.
fn fingerprint(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, u64> {
    use std::{hash::Hasher, io::Read};
    fn visit(
        root: &std::path::Path,
        at: &std::path::Path,
        found: &mut std::collections::BTreeMap<std::path::PathBuf, u64>,
    ) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, found);
            } else if path.file_name().unwrap() != "mdbx.lck" {
                let mut file = fs::File::open(&path).unwrap();
                let mut digest = std::hash::DefaultHasher::new();
                let mut buffer = [0; 65536];
                loop {
                    let count = file.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    digest.write(&buffer[..count]);
                }
                found.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    digest.finish(),
                );
            }
        }
    }
    let mut found = std::collections::BTreeMap::new();
    visit(root, root, &mut found);
    assert!(!found.is_empty());
    found
}

#[test]
fn malformed_storage_settings_are_not_silently_legacy_and_absence_is_v1() {
    let (_root, layout, _, _) = fixture();
    for value in [Some(b"broken JSON".to_vec()), Some(b"{}".to_vec()), None] {
        let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        if let Some(bytes) = &value {
            tx.put::<tables::Metadata>("storage_settings".into(), bytes.clone())
                .unwrap();
        } else {
            tx.delete::<tables::Metadata>("storage_settings".into(), None)
                .unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        let before = fingerprint(&layout.chain_root);
        let result = inspect_reth(&layout);
        if value.is_some() {
            assert!(result.unwrap_err().to_string().contains("storage_settings"));
        } else {
            assert_eq!(result.unwrap().storage_version, 1);
        }
        assert_eq!(fingerprint(&layout.chain_root), before);
    }
}

#[test]
fn missing_execution_store_is_not_initialized() {
    let root = tempfile::tempdir().unwrap();
    let inputs = parse_node_inputs(super::layout::native_arguments(root.path())).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    assert!(inspect_reth(&layout).is_err());
    assert!(!layout.chain_root.exists());
    assert!(!layout.static_files_root.exists());
}

#[test]
fn wrong_genesis_is_rejected_without_rewriting_source() {
    let (_root, layout, _, _) = fixture();
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let foreign = OutbeHeader::default();
    tx.put::<tables::CanonicalHeaders>(0, foreign.hash_slow())
        .unwrap();
    tx.put::<tables::Headers<OutbeHeader>>(0, foreign).unwrap();
    tx.commit().unwrap();
    drop(db);
    let before = fingerprint(&layout.chain_root);
    assert!(inspect_reth(&layout)
        .unwrap_err()
        .to_string()
        .contains("different genesis"));
    assert_eq!(fingerprint(&layout.chain_root), before);
}

#[test]
fn headers_in_native_static_files_are_inspected_without_copying_them_into_mdbx() {
    use outbe_primitives::OutbePrimitives;
    use reth_provider::{providers::StaticFileProvider, StaticFileSegment, StaticFileWriter};

    let (_root, layout, h, e) = fixture();
    let static_files =
        StaticFileProvider::<OutbePrimitives>::read_write(&layout.static_files_root).unwrap();
    {
        let mut writer = static_files
            .get_writer(0, StaticFileSegment::Headers)
            .unwrap();
        for number in 0..=101 {
            let header = match number {
                0 => layout.chain.genesis_header().clone(),
                100 => h.clone(),
                101 => e.clone(),
                _ => OutbeHeader::new(Header {
                    number,
                    ..Default::default()
                }),
            };
            writer.append_header(&header, &header.hash_slow()).unwrap();
        }
    }
    static_files.commit().unwrap();
    drop(static_files);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    for number in [0, 100, 101] {
        tx.delete::<tables::Headers<OutbeHeader>>(number, None)
            .unwrap();
        tx.delete::<tables::CanonicalHeaders>(number, None).unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    let before = fingerprint(&layout.chain_root);
    let progress = inspect_reth(&layout).unwrap();
    assert_eq!(progress.finalized.hash, hex::encode(h.hash_slow()));
    assert_eq!(progress.execution.hash, hex::encode(e.hash_slow()));
    assert_eq!(fingerprint(&layout.chain_root), before);
}
