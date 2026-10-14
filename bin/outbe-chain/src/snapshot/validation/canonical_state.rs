//! Native owner observations from one immutable, independently verified E state.

use alloy_consensus::Sealable;
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_intex::schema::{IntexContract, SeriesId, SeriesRecord};
use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
#[cfg(all(test, feature = "snapshot-integration"))]
use outbe_ocomp_protocol::receipts::AggregateActivationReceiptV1;
use outbe_ocomp_protocol::{
    nod_materialization::NodMaterializationHeadV1, profile::poc_schema_limits,
    state::OcompJobRecordV1,
};
use outbe_primitives::{
    error::{PrecompileError, Result as StorageResult},
    storage::{
        readonly::{ReadOnlyBlockContext, ReadOnlyStorageProvider, StorageReader},
        types::Storable,
        StorageHandle,
    },
    time::WorldwideDay,
};
use reth_ethereum::provider::db::{
    cursor::DbDupCursorRO, database::Database, tables, transaction::DbTx,
};

use super::{evm::VerifiedState, headers::HeaderAudit, Incomplete};
use crate::{
    errors::{CanonicalStatePrecompileError, SnapshotError},
    snapshot::native::RethReadOnlyView,
};

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
        read().map_err(|error| {
            PrecompileError::from(CanonicalStatePrecompileError::ReadVerifiedStorage {
                details: format!("{error:#}"),
            })
        })
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
            PrecompileError::from(CanonicalStatePrecompileError::ReadVerifiedRetainedHeader {
                number,
                details: format!("{error:#}"),
            })
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
        read(StorageHandle::new(&mut provider)).map_err(|error| {
            SnapshotError::CanonicalOwnerState {
                number: header.number,
                error,
            }
            .into()
        })
    }

    pub(crate) fn live_ocomp_jobs(&self) -> eyre::Result<Vec<(B256, OcompJobRecordV1)>> {
        self.with_storage(outbe_metadosis::api::read_live_ocomp_jobs)
    }

    pub(crate) fn metadosis_job(
        &self,
        intent_id: B256,
        expected_day: WorldwideDay,
        expected_job_id: Option<B256>,
    ) -> eyre::Result<OcompJobRecordV1> {
        let bytes = self
            .with_storage(|storage| outbe_metadosis::api::get_offchain_job(storage, intent_id))?;
        let limits = poc_schema_limits();
        let record = OcompJobRecordV1::decode_canonical(&bytes, &limits)?;
        if record.intent.intent_id(&limits)? != intent_id
            || record.intent.wwd != expected_day.value()
        {
            return Err(SnapshotError::MetadosisJobIntentDayMismatch.into());
        }
        if let Some(expected) = expected_job_id {
            if !record
                .finalized
                .as_ref()
                .is_some_and(|job| job.job_id == expected)
            {
                return Err(SnapshotError::MetadosisJobIdMismatch.into());
            }
        }
        if let Some(finalized) = &record.finalized {
            let number = record.intent_height;
            let missing = || {
                Incomplete(format!(
                    "missing verified retained request header B={number}"
                ))
            };
            let hash = self
                .with_storage(|storage| storage.canonical_block_hash(number))?
                .ok_or_else(missing)?;
            let header = self.reader.source.header(number)?.ok_or_else(missing)?;
            if header.inner.number != number
                || header.hash_slow() != hash
                || finalized.finalized_request_block_hash != hash
                || finalized.finalized_request_state_root != header.inner.state_root
            {
                return Err(SnapshotError::MetadosisRequestBindingMismatch { number }.into());
            }
        }
        Ok(record)
    }

    #[cfg(all(test, feature = "snapshot-integration"))]
    pub(crate) fn metadosis_terminal_receipt(
        &self,
        intent_id: B256,
        expected_day: WorldwideDay,
        expected_job_id: B256,
    ) -> eyre::Result<AggregateActivationReceiptV1> {
        self.metadosis_job(intent_id, expected_day, Some(expected_job_id))?;
        let bytes = self.with_storage(|storage| {
            outbe_metadosis::api::get_lysis_terminal_receipt(storage, intent_id)
        })?;
        let receipt = AggregateActivationReceiptV1::decode_canonical(&bytes, &poc_schema_limits())?;
        if receipt.binding.intent_id != intent_id || receipt.binding.job_id != expected_job_id {
            return Err(SnapshotError::LysisTerminalReceiptMismatch.into());
        }
        Ok(receipt)
    }

    pub(crate) fn nod_materialization_bounds(&self) -> eyre::Result<(u64, u64)> {
        self.with_storage(|storage| {
            let nod = NodContract::new(storage);
            Ok((
                nod.ocomp_materialization_head_sequence.read()?,
                nod.ocomp_materialization_tail_sequence.read()?,
            ))
        })
    }

    pub(crate) fn nod_materialization_day(&self, sequence: u64) -> eyre::Result<WorldwideDay> {
        self.with_storage(|storage| {
            NodContract::new(storage)
                .ocomp_materialization_queue_wwd
                .read(&sequence)
        })
    }

    pub(crate) fn nod_materialization_head(
        &self,
    ) -> eyre::Result<Option<NodMaterializationHeadV1>> {
        self.with_storage(|storage| NodContract::new(storage).ocomp_materialization_head())
    }

    pub(crate) fn nod_certified_generation(
        &self,
        day: WorldwideDay,
    ) -> eyre::Result<Option<NodCertifiedGenerationProjection>> {
        self.with_storage(|storage| NodContract::new(storage).ocomp_certified_generation(day))
    }

    pub(crate) fn intex_total_series(&self) -> eyre::Result<u64> {
        self.with_storage(|storage| outbe_intex::api::total_series(&storage))
    }

    pub(crate) fn intex_series_id_at(&self, index: u64) -> eyre::Result<SeriesId> {
        self.with_storage(|storage| {
            let id = outbe_intex::api::series_id_at(&storage, index)?;
            let raw = IntexContract::new(storage)
                .series_id_at_index
                .read(&index)?;
            if raw != id.to_word() {
                return Err(
                    CanonicalStatePrecompileError::IntexSeriesIndexNoncanonicalPadding.into(),
                );
            }
            Ok(id)
        })
    }

    pub(crate) fn intex_read_series(&self, id: SeriesId) -> eyre::Result<SeriesRecord> {
        self.with_storage(|storage| outbe_intex::api::read_series(&storage, id))
    }
}
