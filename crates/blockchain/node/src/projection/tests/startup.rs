use super::*;

#[test]
fn startup_rejects_unavailable_mongodb_before_exex_runs() {
    let started = std::time::Instant::now();
    drop(
        prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: outbe_primitives::chain::DEVNET_CHAIN_ID,
            genesis_hash: B256::repeat_byte(0x11),
            storage: outbe_offchain_storage::StorageConfig {
                start_block: 1,
                backend: outbe_offchain_storage::StorageBackend::MongoDb(outbe_offchain_storage::MongoStorageConfig {
                    uri: "mongodb://127.0.0.1:1/?directConnection=true&serverSelectionTimeoutMS=50".to_owned(),
                    database: "startup_unavailable".to_owned(),
                }),
            },
        })
        .err()
        .expect("unavailable MongoDB must fail startup preparation"),
    );
    assert!(
        started.elapsed() >= PROJECTION_RECOVERY_DEADLINE,
        "startup returned before the shared reconnect deadline"
    );
    assert!(
        started.elapsed() <= PROJECTION_RECOVERY_DEADLINE + Duration::from_millis(250),
        "startup exceeded the shared reconnect deadline"
    );
}

#[test]
fn startup_checkpoint_floor_ignores_stale_finality_then_releases() {
    let checkpoint = FinalizedTarget::new(4, B256::repeat_byte(0x44));
    let mut floor = Some(checkpoint);

    assert!(!super::admit_startup_finalized_target(
        &mut floor,
        FinalizedTarget::new(3, B256::repeat_byte(0x33)),
    )
    .unwrap());
    assert_eq!(floor, Some(checkpoint));

    let error = super::admit_startup_finalized_target(
        &mut floor,
        FinalizedTarget::new(4, B256::repeat_byte(0x45)),
    )
    .unwrap_err();
    assert!(error.to_string().contains("conflicts"));
    assert_eq!(floor, Some(checkpoint));

    assert!(super::admit_startup_finalized_target(&mut floor, checkpoint).unwrap());
    assert_eq!(floor, None);
}

#[test]
fn persisted_checkpoint_waits_for_reth_finality_marker_recovery() {
    let checkpoint = ProjectionCheckpoint {
        block_number: 4,
        block_hash: B256::repeat_byte(0x44),
    };
    assert_eq!(
        require_finalized_checkpoint(checkpoint, None).unwrap(),
        None
    );

    assert_eq!(
        require_finalized_checkpoint(
            checkpoint,
            Some(FinalizedTarget::new(3, B256::repeat_byte(0x33))),
        )
        .expect("one-block crash-consistency gap must recover"),
        None,
    );

    assert_eq!(
        require_finalized_checkpoint(
            checkpoint,
            Some(FinalizedTarget::new(2, B256::repeat_byte(0x22))),
        )
        .expect("a transiently stale finality marker must recover"),
        None,
    );

    let error = require_finalized_checkpoint(
        checkpoint,
        Some(FinalizedTarget::new(4, B256::repeat_byte(0x45))),
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match local Reth finalized"));

    assert_eq!(
        require_finalized_checkpoint(
            checkpoint,
            Some(FinalizedTarget::new(4, checkpoint.block_hash)),
        )
        .unwrap(),
        Some(FinalizedTarget::new(4, checkpoint.block_hash)),
    );

    let ahead = FinalizedTarget::new(5, B256::repeat_byte(0x55));
    assert_eq!(
        require_finalized_checkpoint(checkpoint, Some(ahead)).unwrap(),
        Some(ahead),
    );
}

#[test]
fn node_runtime_opens_logical_projection_over_pending_overlay() {
    let durable = Arc::new(MemoryStorage::new());
    let projection_config = ProjectionConfig {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        start_block: 1,
    };
    OffchainDataProjection::open(projection_config, durable.clone(), durable.clone()).unwrap();
    let durable_reader: StorageReaderHandle = durable.clone();
    let (overlay, mut projector) =
        super::open_logical_projection(projection_config, durable_reader, None).unwrap();
    let block = FinalizedBlock {
        number: 1,
        hash: B256::repeat_byte(0x22),
        receipts: Vec::new(),
    };

    projector.project_block(&block).unwrap();

    assert_eq!(projector.state().checkpoint.unwrap().block_number, 1);
    assert_eq!(
        outbe_offchain_data::read_projection_state(projection_config, durable)
            .unwrap()
            .unwrap()
            .checkpoint,
        None,
        "the durable Mongo-shaped base must not be written by the logical projector"
    );
    assert_eq!(
        outbe_offchain_data::read_projection_state(projection_config, overlay)
            .unwrap()
            .unwrap()
            .checkpoint
            .unwrap()
            .block_number,
        1
    );
}

#[test]
fn projection_network_gate_accepts_known_networks_and_rejects_unknown_ids() {
    for chain_id in [
        outbe_primitives::chain::DEVNET_CHAIN_ID,
        outbe_primitives::chain::TESTNET_CHAIN_ID,
        outbe_primitives::chain::MAINNET_CHAIN_ID,
    ] {
        validate_projection_network(chain_id).unwrap();
    }
    assert!(validate_projection_network(1_000_000_001).is_err());
}
