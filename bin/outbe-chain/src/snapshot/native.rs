//! Read durable native progress without executing or repairing the stopped dataset.

use alloy_consensus::Sealable;
use eyre::{ensure, WrapErr};
use outbe_primitives::{OutbeHeader, OutbePrimitives};
use outbe_snapshot::manifest::{BlockIdentity, UnwindProgress};
use reth_ethereum::provider::db::{
    database::Database,
    mdbx::DatabaseArguments,
    models::PartialStateTrieUnwindMarker,
    open_db_read_only,
    tables::{self, ChainStateKey},
    transaction::DbTx,
    ClientVersion, DatabaseEnv,
};
use reth_provider::{
    providers::StaticFileProvider, BlockHashReader, HeaderProvider, StorageSettings,
};

use super::config::NativeLayout;

#[derive(Debug)]
pub(crate) struct RethProgress {
    pub finalized: BlockIdentity,
    pub execution: BlockIdentity,
    pub execution_stage: Option<u64>,
    pub finish_stage: Option<u64>,
    pub partial_state_trie: Option<u64>,
    pub unwind: Option<UnwindProgress>,
    pub storage_version: u32,
}

pub(crate) fn inspect_reth(layout: &NativeLayout) -> eyre::Result<RethProgress> {
    let db = open_db_read_only(
        layout.chain_root.join("db"),
        DatabaseArguments::new(ClientVersion::default()),
    )
    .wrap_err("open existing execution database read-only")?;
    let static_files = open_existing_static_files(&layout.static_files_root)?;
    read_progress(&db, &static_files, &layout.chain, true)
}

fn open_existing_static_files(
    path: &std::path::Path,
) -> eyre::Result<StaticFileProvider<OutbePrimitives>> {
    // Reth's read-only index initialization otherwise creates a missing directory.
    ensure!(path.is_dir(), "missing static-file directory");
    Ok(StaticFileProvider::read_only(path)?)
}

fn read_progress(
    db: &DatabaseEnv,
    static_files: &StaticFileProvider<OutbePrimitives>,
    chain: &reth_chainspec::ChainSpec<OutbeHeader>,
    require_genesis: bool,
) -> eyre::Result<RethProgress> {
    let tx = db.tx()?;
    let read_identity = |number: u64| -> eyre::Result<BlockIdentity> {
        let header = match tx.get::<tables::Headers<OutbeHeader>>(number)? {
            Some(header) => Some(header),
            None => static_files.header_by_number(number)?,
        }
        .ok_or_else(|| eyre::eyre!("missing retained header {number}"))?;
        let hash = match tx.get::<tables::CanonicalHeaders>(number)? {
            Some(hash) => Some(hash),
            None => static_files.block_hash(number)?,
        }
        .ok_or_else(|| eyre::eyre!("missing canonical hash {number}"))?;
        ensure!(
            header.inner.number == number && header.hash_slow() == hash,
            "stored header does not match canonical identity at {number}"
        );
        Ok(BlockIdentity {
            number,
            hash: hex::encode(hash),
        })
    };
    if require_genesis
        || tx.get::<tables::Headers<OutbeHeader>>(0)?.is_some()
        || static_files.header_by_number(0)?.is_some()
    {
        let genesis = read_identity(0)?;
        ensure!(
            genesis.hash == hex::encode(chain.genesis_hash()),
            "execution database belongs to a different genesis"
        );
    }
    let finalized_number = tx
        .get::<tables::ChainState>(ChainStateKey::LastFinalizedBlock)?
        .ok_or_else(|| eyre::eyre!("missing durable LastFinalizedBlock"))?;
    let execution = tx.get::<tables::StageCheckpoints>("Execution".into())?;
    let finish = tx.get::<tables::StageCheckpoints>("Finish".into())?;
    let execution_number = execution
        .as_ref()
        .map(|stage| stage.block_number)
        .ok_or_else(|| eyre::eyre!("missing execution stage checkpoint"))?;
    let storage_version = match tx.get::<tables::Metadata>("storage_settings".into())? {
        Some(bytes) => {
            let settings: StorageSettings = serde_json::from_slice(&bytes)
                .wrap_err("invalid stored execution storage_settings")?;
            if settings.is_v2() {
                2
            } else {
                1
            }
        }
        None => 1,
    };
    let unwind = tx
        .get::<tables::Metadata>("partial_state_trie_unwind".into())?
        .map(|bytes| serde_json::from_slice::<PartialStateTrieUnwindMarker>(&bytes))
        .transpose()
        .wrap_err("invalid stored partial state trie unwind marker")?
        .map(|marker| UnwindProgress {
            finish_block_number: marker.finish_block_number,
            partial_state_trie: marker.partial_state_trie,
        });
    let result = RethProgress {
        finalized: read_identity(finalized_number)?,
        execution: read_identity(execution_number)?,
        execution_stage: execution.map(|stage| stage.block_number),
        finish_stage: finish.as_ref().map(|stage| stage.block_number),
        partial_state_trie: finish
            .as_ref()
            .and_then(|stage| stage.finish_stage_checkpoint())
            .and_then(|checkpoint| checkpoint.partial_state_trie),
        unwind,
        storage_version,
    };
    tx.commit()?;
    Ok(result)
}

// Native readers used by the standalone offline validator.
pub(crate) struct RethReadOnlyView {
    pub db: DatabaseEnv,
    pub static_files: StaticFileProvider<OutbePrimitives>,
    pub chain: std::sync::Arc<reth_chainspec::ChainSpec<OutbeHeader>>,
    pub progress: RethProgress,
    pub protected: outbe_snapshot::layout::ProtectedPaths,
}

impl RethReadOnlyView {
    /// A stopped offline scan may take hours; the live-node reader timeout must
    /// not discard its immutable database snapshot partway through that scan.
    pub fn read_transaction(&self) -> eyre::Result<<DatabaseEnv as Database>::TX> {
        let mut tx = self.db.tx()?;
        tx.disable_long_read_transaction_safety();
        Ok(tx)
    }

    #[cfg(test)]
    pub fn open(layout: &NativeLayout) -> eyre::Result<Self> {
        let mut protected = layout.protected.clone();
        protected.0.extend([
            layout.chain_root.clone(),
            layout.consensus_root.clone(),
            layout.ocomp_root.clone(),
            layout.offchain_root.clone(),
            layout.static_files_root.clone(),
            layout.execution_rocksdb_root.clone(),
        ]);
        Self::open_parts(
            &layout.chain_root,
            &layout.static_files_root,
            layout.chain.clone(),
            protected,
        )
    }

    pub fn open_requested(layout: &super::config::RequestedLayout) -> eyre::Result<Self> {
        let mut protected = layout.protected.clone();
        protected.0.extend([
            layout.chain_root.clone(),
            layout.consensus_root.clone(),
            layout.ocomp_root.clone(),
            layout.static_files_root.clone(),
            layout.execution_rocksdb_root.clone(),
        ]);
        protected.0.extend(
            layout
                .projection
                .as_ref()
                .map(|projection| projection.root.clone()),
        );
        Self::open_parts(
            &layout.chain_root,
            &layout.static_files_root,
            layout.chain.clone(),
            protected,
        )
    }

    fn open_parts(
        chain_root: &std::path::Path,
        static_root: &std::path::Path,
        chain: std::sync::Arc<reth_chainspec::ChainSpec<OutbeHeader>>,
        protected: outbe_snapshot::layout::ProtectedPaths,
    ) -> eyre::Result<Self> {
        let db = open_db_read_only(chain_root.join("db"), DatabaseArguments::default())?;
        let static_files = open_existing_static_files(static_root)?;
        let progress = read_progress(&db, &static_files, &chain, false)?;
        Ok(Self {
            db,
            static_files,
            chain,
            progress,
            protected,
        })
    }

    pub fn header(&self, number: u64) -> eyre::Result<Option<OutbeHeader>> {
        let tx = self.db.tx()?;
        match tx.get::<tables::Headers<OutbeHeader>>(number)? {
            Some(header) => Ok(Some(header)),
            None => Ok(self.static_files.header_by_number(number)?),
        }
    }

    pub fn canonical_hash(&self, number: u64) -> eyre::Result<Option<alloy_primitives::B256>> {
        let tx = self.db.tx()?;
        match tx.get::<tables::CanonicalHeaders>(number)? {
            Some(hash) => Ok(Some(hash)),
            None => Ok(self.static_files.block_hash(number)?),
        }
    }
}

pub(crate) fn ce_identity(layout: &NativeLayout) -> outbe_compressed_entities::EnvironmentIdentity {
    outbe_compressed_entities::EnvironmentIdentity {
        local_storage_schema_version: outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: layout.chain.chain().id(),
        genesis_hash: layout.chain.genesis_hash(),
        commitment_scheme_version: outbe_compressed_entities::ACTIVE_COMMITMENT_SCHEME,
        topology: outbe_compressed_entities::CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
    }
}

/// Observe the stopped execution, CE, projection and closure stores independently.
/// The caller supplies a separate disposable RocksDB secondary directory.
pub(crate) fn inspect_stopped_stores(
    layout: &NativeLayout,
    scratch: &std::path::Path,
) -> eyre::Result<outbe_snapshot::manifest::NativeProgress> {
    use outbe_compressed_entities::CeMdbxReadOnly;
    use outbe_ocomp::discovery_spool::inspect_closure_checkpoint;
    use outbe_offchain_data::{read_projection_state, ProjectionConfig};
    use outbe_offchain_storage::RocksDbReader;
    use outbe_primitives::projection::ProjectionCheckpoint;
    use outbe_snapshot::manifest::NativeProgress;
    use std::sync::Arc;

    // A RocksDB secondary writes its own files. Keep all native/config roots
    // protected even when some of their optional subdirectories are absent.
    let mut protected = layout.protected.clone();
    protected.0.extend([
        layout.chain_root.clone(),
        layout.consensus_root.clone(),
        layout.ocomp_root.clone(),
        layout.offchain_root.clone(),
        layout.static_files_root.clone(),
        layout.execution_rocksdb_root.clone(),
    ]);
    outbe_snapshot::layout::validate_layout(&[], &protected, &[scratch.to_path_buf()])?;
    let retention_root = layout.consensus_root.join("ocomp_retention");
    if retention_root.join("pin.v1").try_exists()? {
        outbe_node::ocomp::retention::inspect_retention_journal(&retention_root)
            .wrap_err("inspect existing OCOMP retention journal")?;
    }
    let reth = inspect_reth(layout)?;
    let ce = CeMdbxReadOnly::open(&layout.chain_root, ce_identity(layout))?.marker()?;
    let projection = read_projection_state(
        ProjectionConfig {
            chain_id: layout.chain.chain().id(),
            genesis_hash: layout.chain.genesis_hash(),
            start_block: layout.projection_start_block,
        },
        Arc::new(RocksDbReader::open(&layout.offchain_root, scratch)?),
    )?
    .and_then(|state| state.checkpoint)
    .ok_or_else(|| eyre::eyre!("missing initialized offchain projection checkpoint"))?;
    let closure = inspect_closure_checkpoint(
        layout
            .ocomp_root
            .join("exporter-v1/discovery/closure-checkpoint-v1"),
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: layout.chain.genesis_hash(),
        },
    )?;
    let block = |checkpoint: ProjectionCheckpoint| BlockIdentity {
        number: checkpoint.block_number,
        hash: hex::encode(checkpoint.block_hash),
    };
    Ok(NativeProgress {
        finalized: reth.finalized,
        execution: reth.execution,
        execution_stage: reth.execution_stage,
        finish_stage: reth.finish_stage,
        partial_state_trie: reth.partial_state_trie,
        unwind: reth.unwind,
        storage_version: reth.storage_version,
        ce: BlockIdentity {
            number: ce.height,
            hash: hex::encode(ce.block_hash),
        },
        projection: block(projection),
        ocomp_baseline: block(closure.baseline),
        ocomp_previous: block(closure.previous),
        ocomp_current: block(closure.current),
    })
}
