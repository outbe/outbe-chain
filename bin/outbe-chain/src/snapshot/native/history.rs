//! Read historical state without writing secondary files beside native sources.

use std::path::Path;

use alloy_primitives::B256;
use reth_ethereum::{
    node::api::NodeTypesWithDBAdapter,
    provider::db::DatabaseEnv,
    tasks::{RayonConfig, RuntimeBuilder, RuntimeConfig, TokioConfig},
};
use reth_provider::{providers::RocksDBProvider, DBProvider, ProviderFactory, StateProvider};
use reth_storage_overlay::{OverlayManager, OverlayStateProvider};

use super::RethReadOnlyView;
use crate::snapshot::{config::RequestedLayout, validation::Incomplete};

type NativeFactory = ProviderFactory<NodeTypesWithDBAdapter<outbe_node::OutbeNode, DatabaseEnv>>;

pub(crate) struct HistoricalStateReader {
    factory: NativeFactory,
    // Drop providers before removing their scratch-owned RocksDB and secondary files.
    _directory: tempfile::TempDir,
}

impl HistoricalStateReader {
    pub(crate) fn open(
        layout: &RequestedLayout,
        view: &RethReadOnlyView,
        scratch: &Path,
    ) -> eyre::Result<Self> {
        outbe_snapshot::layout::validate_layout(&[], &view.protected, &[scratch.to_path_buf()])?;
        let directory = tempfile::Builder::new()
            .prefix("native-history-")
            .tempdir_in(scratch)?;
        let rocks = directory.path().join("rocksdb");
        if layout.execution_rocksdb_root.try_exists()? {
            copy_regular_tree(&layout.execution_rocksdb_root, &rocks)?;
        } else if view.progress.storage_version == 1 {
            // Version 1 keeps history in MDBX. Reth still requires a RocksDB handle.
            drop(
                RocksDBProvider::builder(&rocks)
                    .with_default_tables()
                    .build()?,
            );
        } else {
            return Err(Incomplete("native RocksDB history is missing".into()).into());
        }
        let runtime = RuntimeBuilder::new(RuntimeConfig {
            tokio: tokio::runtime::Handle::try_current()
                .map(TokioConfig::existing_handle)
                .unwrap_or_else(|_| TokioConfig::with_worker_threads(1)),
            rayon: RayonConfig {
                cpu_threads: Some(1),
                rpc_threads: Some(1),
                storage_threads: Some(1),
                proof_storage_worker_threads: Some(1),
                proof_account_worker_threads: Some(1),
                prewarming_threads: Some(1),
                bal_streaming_threads: Some(1),
                state_trie_overlay_worker_threads: Some(1),
                ..Default::default()
            },
        })
        .build()?;
        let rocksdb = RocksDBProvider::builder(&rocks)
            .with_default_tables()
            .with_read_only(true)
            .build()?;
        let factory = ProviderFactory::new(
            view.db.clone(),
            view.chain.clone(),
            view.static_files.clone(),
            rocksdb,
            runtime,
        )?
        .with_read_only_sync(false);
        Ok(Self {
            factory,
            _directory: directory,
        })
    }

    pub(crate) fn with_state<T>(
        &self,
        block_hash: B256,
        read: impl FnOnce(&dyn StateProvider) -> eyre::Result<T>,
    ) -> eyre::Result<T> {
        let state = OverlayStateProvider::new(
            self.factory
                .provider()?
                .disable_long_read_transaction_safety(),
            OverlayManager::<outbe_primitives::OutbePrimitives>::default()
                .overlay_builder(block_hash),
        );
        read(&state)
    }
}

fn copy_regular_tree(source: &Path, destination: &Path) -> eyre::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if !metadata.is_dir() {
        return Err(Incomplete(format!(
            "native history directory is unavailable: {}",
            source.display()
        ))
        .into());
    }
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_regular_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            return Err(Incomplete(format!(
                "unsupported native history file: {}",
                entry.path().display()
            ))
            .into());
        }
    }
    Ok(())
}
