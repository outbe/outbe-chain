use crate::world::state::{
    SnapshotBlock, SnapshotNativeObservation, SnapshotNativeProgress, SnapshotUnwind,
};
use alloy_consensus::Sealable;
use outbe_primitives::{OutbeHeader, OutbePrimitives};
use reth_ethereum::provider::db::{
    database::Database,
    mdbx::DatabaseArguments,
    models::PartialStateTrieUnwindMarker,
    open_db_read_only,
    table::Table,
    tables::{self, ChainStateKey},
    transaction::DbTx,
};
use reth_provider::{
    providers::StaticFileProvider, BlockHashReader, HeaderProvider, StorageSettings,
};

pub(super) struct NativeOcompProgress {
    pub(super) ocomp_baseline: SnapshotBlock,
    pub(super) ocomp_previous: SnapshotBlock,
    pub(super) ocomp_current: SnapshotBlock,
}

fn observe_ocomp_progress(
    node: &std::path::Path,
    genesis_hash: alloy_primitives::B256,
) -> eyre::Result<NativeOcompProgress> {
    use outbe_primitives::projection::ProjectionCheckpoint;
    let closure = outbe_ocomp::discovery_spool::inspect_closure_checkpoint(
        node.join("ocomp/domain-v1/exporter-v1/discovery/closure-checkpoint-v1"),
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: genesis_hash,
        },
    )?;
    let block = |value: ProjectionCheckpoint| SnapshotBlock {
        number: value.block_number,
        hash: hex::encode(value.block_hash),
    };
    Ok(NativeOcompProgress {
        ocomp_baseline: block(closure.baseline),
        ocomp_previous: block(closure.previous),
        ocomp_current: block(closure.current),
    })
}

pub(super) fn observe_stopped_native(
    node_dir: &std::path::Path,
    genesis_hash: alloy_primitives::B256,
) -> eyre::Result<crate::world::state::SnapshotNativeObservation> {
    // These paths come from the harness's ordinary node layout,
    // independently of the received manifest's donor paths and expected values.
    let node = node_dir.canonicalize()?;
    let chain = node.join("data");
    let static_path = chain.join("static_files");
    let closure_path = node.join("ocomp/domain-v1/exporter-v1/discovery/closure-checkpoint-v1");
    eyre::ensure!(static_path.is_dir(), "missing placed static files");
    let native = read_native_execution_progress(&chain, &static_path, genesis_hash)?;

    let offchain = observe_ocomp_progress(&node, genesis_hash)?;
    let progress = native_progress(native, &offchain);
    Ok(SnapshotNativeObservation {
        progress,
        sources: vec![
            chain.join("db"),
            static_path,
            chain.join("rocksdb"),
            closure_path,
        ],
        observed: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .try_into()?,
    })
}

type NativeStageCheckpoint = <tables::StageCheckpoints as Table>::Value;

struct NativeExecutionProgress {
    finalized: SnapshotBlock,
    execution_identity: SnapshotBlock,
    execution: NativeStageCheckpoint,
    finish: Option<NativeStageCheckpoint>,
    storage_version: u32,
    unwind: Option<SnapshotUnwind>,
}

fn native_block_identity(
    tx: &impl DbTx,
    files: &StaticFileProvider<OutbePrimitives>,
    number: u64,
) -> eyre::Result<SnapshotBlock> {
    let header = match tx.get::<tables::Headers<OutbeHeader>>(number)? {
        Some(value) => Some(value),
        None => files.header_by_number(number)?,
    }
    .ok_or_else(|| eyre::eyre!("missing native header {number}"))?;
    let hash = match tx.get::<tables::CanonicalHeaders>(number)? {
        Some(value) => Some(value),
        None => files.block_hash(number)?,
    }
    .ok_or_else(|| eyre::eyre!("missing native canonical identity {number}"))?;
    eyre::ensure!(
        header.inner.number == number && header.hash_slow() == hash,
        "native header identity mismatch at {number}"
    );
    Ok(SnapshotBlock {
        number,
        hash: hex::encode(hash),
    })
}

fn read_native_execution_progress(
    chain: &std::path::Path,
    static_path: &std::path::Path,
    genesis_hash: alloy_primitives::B256,
) -> eyre::Result<NativeExecutionProgress> {
    let db = open_db_read_only(chain.join("db"), DatabaseArguments::default())?;
    let files = StaticFileProvider::<OutbePrimitives>::read_only(static_path)?;
    let tx = db.tx()?;

    eyre::ensure!(
        native_block_identity(&tx, &files, 0)?.hash == hex::encode(genesis_hash),
        "wrong placed genesis"
    );
    let finalized = native_block_identity(
        &tx,
        &files,
        tx.get::<tables::ChainState>(ChainStateKey::LastFinalizedBlock)?
            .ok_or_else(|| eyre::eyre!("missing native LastFinalizedBlock"))?,
    )?;
    let execution = tx
        .get::<tables::StageCheckpoints>("Execution".into())?
        .ok_or_else(|| eyre::eyre!("missing native Execution checkpoint"))?;
    let execution_identity = native_block_identity(&tx, &files, execution.block_number)?;
    let finish = tx.get::<tables::StageCheckpoints>("Finish".into())?;
    let storage_version = match tx.get::<tables::Metadata>("storage_settings".into())? {
        Some(raw) if serde_json::from_slice::<StorageSettings>(&raw)?.is_v2() => 2,
        _ => 1,
    };
    let unwind = tx
        .get::<tables::Metadata>("partial_state_trie_unwind".into())?
        .map(|raw| serde_json::from_slice::<PartialStateTrieUnwindMarker>(&raw))
        .transpose()?
        .map(|value| SnapshotUnwind {
            finish_block_number: value.finish_block_number,
            partial_state_trie: value.partial_state_trie,
        });
    tx.commit()?;
    drop(files);
    drop(db);
    Ok(NativeExecutionProgress {
        finalized,
        execution_identity,
        execution,
        finish,
        storage_version,
        unwind,
    })
}

fn native_progress(
    native: NativeExecutionProgress,
    offchain: &NativeOcompProgress,
) -> SnapshotNativeProgress {
    SnapshotNativeProgress {
        finalized: native.finalized,
        execution: native.execution_identity,
        execution_stage: Some(native.execution.block_number),
        finish_stage: native.finish.as_ref().map(|value| value.block_number),
        partial_state_trie: native
            .finish
            .as_ref()
            .and_then(|value| value.finish_stage_checkpoint())
            .and_then(|value| value.partial_state_trie),
        unwind: native.unwind,
        storage_version: native.storage_version,
        ocomp_baseline: offchain.ocomp_baseline.clone(),
        ocomp_previous: offchain.ocomp_previous.clone(),
        ocomp_current: offchain.ocomp_current.clone(),
    }
}
