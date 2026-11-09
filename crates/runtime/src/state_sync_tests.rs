//! Native checkpoint and recipient identity checks after a stopped-store copy.
//! These component tests do not start a full node process.

use alloy_consensus::{Header, Sealable};
use outbe_consensus::executor::actor::FinalizedExecutionBarrier;
use outbe_engine::finalization::{
    DurableExecutionState, RethDurableExecutionState, RethFinalizationBarrier,
};
use outbe_primitives::{
    chain::TESTNET_CHAIN_ID, projection::ProjectionCheckpoint, OutbeHeader, OutbePrimitives,
};
use reth_ethereum::{
    chainspec::ChainSpec,
    node::api::NodeTypesWithDBAdapter,
    provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        models::StoredBlockBodyIndices,
        table::Table,
        tables::{self, ChainStateKey},
        transaction::{DbTx, DbTxMut},
        DatabaseEnv,
    },
    tasks::Runtime,
};
use reth_primitives_traits::SealedHeader;
use reth_provider::{
    providers::{BlockchainProvider, RocksDBProvider, StaticFileProvider},
    static_file::StaticFileSegment,
    BlockHashReader, ProviderFactory, StaticFileProviderBuilder, StaticFileWriter, StorageSettings,
    StorageSettingsCache,
};
use std::{fs, path::Path, sync::Arc};

type Nodes = NodeTypesWithDBAdapter<outbe_node::OutbeNode, DatabaseEnv>;
type Provider = BlockchainProvider<Nodes>;
type Stage = <tables::StageCheckpoints as Table>::Value;
const H: u64 = 3;

fn genesis_header() -> OutbeHeader {
    OutbeHeader::new(Header::default())
}

fn chain() -> Arc<ChainSpec<OutbeHeader>> {
    Arc::new(ChainSpec {
        chain: TESTNET_CHAIN_ID.into(),
        genesis_header: SealedHeader::seal_slow(genesis_header()),
        ..Default::default()
    })
}

fn headers() -> Vec<OutbeHeader> {
    outbe_consensus::test_harness::linked_headers(genesis_header(), H)
}

fn point(headers: &[OutbeHeader], height: u64) -> ProjectionCheckpoint {
    ProjectionCheckpoint {
        block_number: height,
        block_hash: headers[height as usize].hash_slow(),
    }
}

fn static_files(root: &Path) -> StaticFileProvider<OutbePrimitives> {
    StaticFileProviderBuilder::read_write(root.join("static_files"))
        .with_blocks_per_file_for_segment(StaticFileSegment::Headers, 1)
        .build()
        .unwrap()
}

fn open_provider(root: &Path) -> Provider {
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let factory = ProviderFactory::<Nodes>::new(
        db,
        chain(),
        static_files(root),
        RocksDBProvider::new(root.join("rocksdb")).unwrap(),
        Runtime::test(),
    )
    .unwrap();
    assert_eq!(factory.cached_storage_settings(), StorageSettings::v1());
    BlockchainProvider::new(factory).unwrap()
}

// This native empty-body fixture does not claim EVM block execution.
// The reopened provider supplies durable headers, progress and frame indexes.
fn seed_reth(root: &Path, headers: &[OutbeHeader]) {
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    tx.put::<tables::Metadata>(
        "storage_settings".into(),
        serde_json::to_vec(&StorageSettings::v1()).unwrap(),
    )
    .unwrap();
    for header in headers {
        let number = header.inner.number;
        let hash = header.hash_slow();
        tx.put::<tables::CanonicalHeaders>(number, hash).unwrap();
        tx.put::<tables::HeaderNumbers>(hash, number).unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(number, header.clone())
            .unwrap();
        tx.put::<tables::BlockBodyIndices>(
            number,
            StoredBlockBodyIndices {
                first_tx_num: 0,
                tx_count: 0,
            },
        )
        .unwrap();
    }
    for stage in ["Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(H))
            .unwrap();
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, H)
        .unwrap();
    tx.put::<tables::ChainState>(ChainStateKey::LastSafeBlock, H)
        .unwrap();
    tx.commit().unwrap();
    drop(db);
    let files = static_files(root);
    let mut writer = files.latest_writer(StaticFileSegment::Headers).unwrap();
    for header in headers {
        writer.append_header(header, &header.hash_slow()).unwrap();
    }
    writer.commit().unwrap();
}

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

fn copied_fixture() -> (tempfile::TempDir, Vec<OutbeHeader>) {
    let donor = tempfile::tempdir().unwrap();
    let recipient = tempfile::tempdir().unwrap();
    let headers = headers();
    seed_reth(donor.path(), &headers);
    drop(open_provider(donor.path()));
    for name in ["db", "static_files", "rocksdb"] {
        copy_stopped_directory(&donor.path().join(name), &recipient.path().join(name));
    }
    assert!(
        fs::metadata(recipient.path().join("db/mdbx.dat"))
            .unwrap()
            .len()
            > 0
    );
    donor.close().unwrap();
    (recipient, headers)
}

#[test]
fn copied_native_checkpoint_and_recipient_identity_survive_repeated_reopen() {
    let (recipient, headers) = copied_fixture();
    let secret = recipient.path().join("recipient-p2p.key");
    let default_secret = recipient.path().join("chain/discovery-secret");
    let network = reth_node_core::args::NetworkArgs {
        p2p_secret_key: Some(secret.clone()),
        ..Default::default()
    };
    let (_, own_public) =
        super::load_reth_p2p_node_host_signer(&network, default_secret.clone()).unwrap();
    let own_key_bytes = fs::read(&secret).unwrap();
    let target = point(&headers, H);
    for _ in 0..2 {
        let provider = open_provider(recipient.path());
        let source = Arc::new(RethDurableExecutionState::new(provider));
        let evidence = source.block_and_root(H).unwrap().unwrap();
        assert_eq!(evidence.block_hash, target.block_hash);
        assert_eq!(
            evidence.parent_block_hash,
            point(&headers, H - 1).block_hash
        );
        RethFinalizationBarrier::new(source)
            .verify_checkpoint(target)
            .unwrap();
        let (_, public) =
            super::load_reth_p2p_node_host_signer(&network, default_secret.clone()).unwrap();
        assert_eq!(public, own_public);
        assert_eq!(fs::read(&secret).unwrap(), own_key_bytes);
        assert!(!default_secret.exists());
    }
}

#[test]
fn copied_native_missing_frame_is_not_repaired_by_checkpoint_verification() {
    let (recipient, headers) = copied_fixture();
    {
        let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        assert!(tx.delete::<tables::BlockBodyIndices>(H, None).unwrap());
        tx.commit().unwrap();
    }
    let target = point(&headers, H);
    for _ in 0..2 {
        let provider = open_provider(recipient.path());
        RethFinalizationBarrier::new(Arc::new(RethDurableExecutionState::new(provider)))
            .verify_checkpoint(target)
            .unwrap();
    }
    let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
    assert!(db
        .tx()
        .unwrap()
        .get::<tables::BlockBodyIndices>(H)
        .unwrap()
        .is_none());
}

#[test]
fn copied_native_header_does_not_replace_missing_durable_state_progress() {
    let (recipient, headers) = copied_fixture();
    {
        let db = init_db(recipient.path().join("db"), DatabaseArguments::test()).unwrap();
        let tx = db.tx_mut().unwrap();
        tx.put::<tables::StageCheckpoints>("Finish".into(), Stage::new(H - 1))
            .unwrap();
        tx.commit().unwrap();
    }
    let target = point(&headers, H);
    for _ in 0..2 {
        let provider = open_provider(recipient.path());
        assert_eq!(provider.block_hash(H).unwrap(), Some(target.block_hash));
        let source = Arc::new(RethDurableExecutionState::new(provider));
        assert!(source.block_and_root(H).unwrap().is_none());
        assert!(RethFinalizationBarrier::new(source)
            .verify_checkpoint(target)
            .is_err());
    }
}
