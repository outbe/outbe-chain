//! Native owner observations from one immutable, independently verified E state.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_primitives::{
    error::{PrecompileError, Result as StorageResult},
    storage::{
        readonly::{ReadOnlyBlockContext, ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};
use reth_ethereum::provider::db::{
    cursor::DbDupCursorRO, database::Database, tables, transaction::DbTx,
};

use super::{evm::VerifiedState, headers::HeaderAudit};
use crate::snapshot::native::RethReadOnlyView;

#[derive(Clone, Copy)]
struct ScratchStorageReader<'a> {
    state: &'a VerifiedState,
    source: &'a RethReadOnlyView,
    headers: &'a HeaderAudit,
}

impl StorageReader for ScratchStorageReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> StorageResult<U256> {
        let hashed_key = keccak256(key);
        let read = || -> eyre::Result<U256> {
            let tx = self.state.db.tx()?;
            let entry = tx
                .cursor_dup_read::<tables::HashedStorages>()?
                .seek_by_key_subkey(keccak256(address), hashed_key)?;
            // MDBX seeks to the first duplicate at or beyond the requested key.
            // A neighboring slot is not evidence that this slot exists.
            Ok(entry
                .filter(|entry| entry.key == hashed_key)
                .map_or(U256::ZERO, |entry| entry.value))
        };
        read().map_err(|error| PrecompileError::Fatal(format!("read verified storage: {error:#}")))
    }

    fn read_canonical_block_hash(&self, number: u64) -> StorageResult<Option<B256>> {
        if !self
            .headers
            .intervals
            .iter()
            .any(|range| range.contains(&number))
        {
            return Ok(None);
        }
        self.source.canonical_hash(number).map_err(|error| {
            PrecompileError::Fatal(format!("read verified retained header {number}: {error:#}"))
        })
    }
}

pub(crate) struct CanonicalState<'a> {
    reader: ScratchStorageReader<'a>,
}

impl<'a> CanonicalState<'a> {
    pub(crate) fn new(
        state: &'a VerifiedState,
        source: &'a RethReadOnlyView,
        headers: &'a HeaderAudit,
    ) -> Self {
        Self {
            reader: ScratchStorageReader {
                state,
                source,
                headers,
            },
        }
    }

    pub(crate) fn with_storage<T>(
        &self,
        read: impl FnOnce(StorageHandle<'_>) -> StorageResult<T>,
    ) -> eyre::Result<T> {
        let header = &self.reader.state.header.inner;
        let context = ReadOnlyBlockContext {
            chain_id: self.reader.source.chain.chain().id(),
            genesis_hash: self.reader.source.chain.genesis_hash(),
            block_number: header.number,
            timestamp: header.timestamp,
        };
        let mut provider = ReadOnlyStorageProvider::new_with_block_context(self.reader, context);
        read(StorageHandle::new(&mut provider))
            .map_err(|error| eyre::eyre!("canonical owner state at E={}: {error}", header.number))
    }

    pub(crate) fn live_ocomp_jobs(&self) -> eyre::Result<Vec<(B256, OcompJobRecordV1)>> {
        self.with_storage(outbe_metadosis::api::read_live_ocomp_jobs)
    }
}
