//! Configuration and native projection preflight after conventional file placement.
//! These component tests do not claim a full node process launch.

use std::{fs, path::Path, sync::Arc};

use alloy_primitives::B256;
use outbe_node::projection::{prepare_offchain_data_projection, OffchainDataProjectionConfig};
use outbe_offchain_data::{FinalizedBlock, OffchainDataProjection, ProjectionConfig};
use outbe_offchain_storage::{RocksDbStorage, StorageBackend, StorageConfig};
use outbe_primitives::projection::{ProjectionCheckpoint, ProjectionStatus};

fn copy_stopped_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_stopped_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn copied_projection_uses_native_checkpoint_and_recipient_configuration_on_each_restart() {
    let donor = tempfile::tempdir().unwrap();
    let recipient = tempfile::tempdir().unwrap();
    let config = ProjectionConfig {
        chain_id: outbe_primitives::chain::TESTNET_CHAIN_ID,
        genesis_hash: B256::repeat_byte(0x71),
        start_block: 1,
    };
    let donor_storage = donor.path().join("projection");
    {
        let storage = Arc::new(RocksDbStorage::open(&donor_storage).unwrap());
        let mut projection =
            OffchainDataProjection::open(config, storage.clone(), storage).unwrap();
        for height in 1..=3 {
            projection
                .project_block(&FinalizedBlock {
                    number: height,
                    hash: B256::repeat_byte(height as u8),
                    receipts: Vec::new(),
                })
                .unwrap();
        }
    }
    let recipient_storage = recipient.path().join("projection");
    copy_stopped_directory(&donor_storage, &recipient_storage);
    assert!(recipient_storage.join("CURRENT").is_file());
    donor.close().unwrap();
    assert!(!donor_storage.exists());

    let configuration = recipient.path().join("configuration");
    fs::create_dir(&configuration).unwrap();
    let storage_file = configuration.join("offchain.toml");
    fs::write(
        &storage_file,
        "version = 1\nbackend = 'rocksdb'\nstart_block = 1\n[rocksdb]\npath = '../projection'\nsecondary_path = '../secondary'\n",
    )
    .unwrap();
    let storage = StorageConfig::load(&storage_file).unwrap();
    let StorageBackend::RocksDb(rocks) = &storage.backend else {
        panic!("expected native RocksDB storage");
    };
    assert_eq!(rocks.path, recipient_storage);
    assert_eq!(rocks.secondary_path, recipient.path().join("secondary"));
    assert_eq!(storage.start_block, 1);

    let secret = configuration.join("recipient-p2p.key");
    let default_secret = recipient.path().join("chain/discovery-secret");
    let network = reth_node_core::args::NetworkArgs {
        p2p_secret_key: Some(secret.clone()),
        ..Default::default()
    };
    let (_, own_public) =
        super::load_reth_p2p_node_host_signer(&network, default_secret.clone()).unwrap();
    let own_key_bytes = fs::read(&secret).unwrap();

    // No manifest, archive or validation receipt is supplied to ordinary preflight.
    for height in [3, 4] {
        let prepared = prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: config.chain_id,
            genesis_hash: config.genesis_hash,
            storage: storage.clone(),
        })
        .unwrap();
        assert_eq!(
            prepared.readiness().current(),
            ProjectionStatus::CatchingUp {
                checkpoint: Some(ProjectionCheckpoint {
                    block_number: height,
                    block_hash: B256::repeat_byte(height as u8),
                }),
            }
        );
        drop(prepared);
        let (_, public) =
            super::load_reth_p2p_node_host_signer(&network, default_secret.clone()).unwrap();
        assert_eq!(public, own_public);
        assert_eq!(fs::read(&secret).unwrap(), own_key_bytes);
        assert!(!default_secret.exists());
        if height == 3 {
            let storage = Arc::new(RocksDbStorage::open(&recipient_storage).unwrap());
            let mut projection =
                OffchainDataProjection::open(config, storage.clone(), storage).unwrap();
            projection
                .project_block(&FinalizedBlock {
                    number: 4,
                    hash: B256::repeat_byte(4),
                    receipts: Vec::new(),
                })
                .unwrap();
        }
    }

    let mut wrong_start = storage.clone();
    wrong_start.start_block = 4;
    let error = prepare_offchain_data_projection(OffchainDataProjectionConfig {
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        storage: wrong_start,
    })
    .err()
    .expect("snapshot height is not a replacement for native start_block");
    assert!(error.to_string().contains("start_block 1"));
    assert!(
        prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: config.chain_id,
            genesis_hash: B256::repeat_byte(0x72),
            storage,
        })
        .is_err()
    );
    assert_eq!(fs::read(&secret).unwrap(), own_key_bytes);
}
