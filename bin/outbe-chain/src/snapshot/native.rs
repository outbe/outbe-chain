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
    ClientVersion,
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
    let static_files = StaticFileProvider::<OutbePrimitives>::read_only(&layout.static_files_root)?;
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
    let genesis = read_identity(0)?;
    ensure!(
        genesis.hash == hex::encode(layout.chain.genesis_hash()),
        "execution database belongs to a different genesis"
    );
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
