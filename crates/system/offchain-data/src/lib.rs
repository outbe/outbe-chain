//! Backend-neutral finalized-receipt projection for Outbe off-chain data.
//!
//! This crate deliberately knows nothing about Reth or MongoDB. A node adapter
//! normalizes finalized blocks into [`FinalizedBlock`], while the projector
//! consumes only the shared off-chain storage capabilities.

mod runtime_readers;

pub use outbe_primitives::projection::{
    projection_readiness, ProjectionCheckpoint, ProjectionFailure, ProjectionFailureClass,
    ProjectionReadinessHandle, ProjectionReadinessPublisher, ProjectionStatus, WaitOutcome,
};
pub use runtime_readers::{ExecutionReadBudgetGuard, RuntimeBodyFailure, RuntimeBodyReaders};

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, LowerHex},
    str::FromStr,
};

use alloy_primitives::{Address, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_nod::{
    precompile::INod,
    projection::{
        plan_nod_bucket_mutation, plan_nod_item_mutation, NodBucketMutation, NodItemMutation,
        NOD_PROJECTION_NAMESPACES,
    },
    NodBucketState, NodItemState, NodRepositoryError, NodRepositoryReader,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanRequest, StorageError,
    StorageMetadata, StorageReaderHandle, StorageWriterHandle, StoredValue, Value,
};
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_tribute::{
    precompile::ITribute,
    projection::{plan_tribute_mutation, TributeMutation, TRIBUTE_PROJECTION_NAMESPACES},
    TributeData, TributeRepositoryError, TributeRepositoryReader,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Local representation version owned by this projector.
pub const STORAGE_SCHEMA_VERSION: u32 = 1;
/// Namespace containing the singleton projector state.
pub const PROJECTION_STATE_NAMESPACE: &str = "projection_state";
/// Singleton projector-state key.
pub const PROJECTION_STATE_KEY: &[u8] = b"offchain_data";

const SOURCE_KEYS: [&str; 7] = [
    "block_number",
    "block_hash",
    "tx_hash",
    "transaction_index",
    "log_index",
    "emitter",
    "event_signature",
];

/// Network identity and the first block that must be projected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionConfig {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub start_block: u64,
}

/// Portable projector identity and progress persisted beside domain data.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionState {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub storage_schema_version: u32,
    pub start_block: u64,
    pub checkpoint: Option<ProjectionCheckpoint>,
}

/// Typed provenance attached to a projected primary body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionSource {
    pub block_number: u64,
    pub block_hash: B256,
    pub tx_hash: B256,
    pub transaction_index: u64,
    pub log_index: u64,
    pub emitter: Address,
    pub event_signature: B256,
}

impl ProjectionSource {
    /// Converts typed provenance into the storage facade's validated map.
    pub fn to_storage_metadata(self) -> Result<StorageMetadata, ProjectionError> {
        StorageMetadata::new(BTreeMap::from([
            ("block_number".to_owned(), self.block_number.to_string()),
            ("block_hash".to_owned(), format!("{:#x}", self.block_hash)),
            ("tx_hash".to_owned(), format!("{:#x}", self.tx_hash)),
            (
                "transaction_index".to_owned(),
                self.transaction_index.to_string(),
            ),
            ("log_index".to_owned(), self.log_index.to_string()),
            ("emitter".to_owned(), format!("{:#x}", self.emitter)),
            (
                "event_signature".to_owned(),
                format!("{:#x}", self.event_signature),
            ),
        ]))
        .map_err(ProjectionError::Storage)
    }

    /// Strictly decodes the fixed metadata schema.
    pub fn from_storage_metadata(metadata: &StorageMetadata) -> Result<Self, ProjectionError> {
        if metadata.len() != SOURCE_KEYS.len() {
            return Err(ProjectionError::MalformedProjectionMetadata(
                "projection metadata must contain exactly seven fields".to_owned(),
            ));
        }
        for (key, _) in metadata.iter() {
            if !SOURCE_KEYS.contains(&key) {
                return Err(ProjectionError::MalformedProjectionMetadata(format!(
                    "unknown projection metadata field {key}"
                )));
            }
        }
        Ok(Self {
            block_number: parse_metadata(metadata, "block_number")?,
            block_hash: parse_fixed(metadata, "block_hash")?,
            tx_hash: parse_fixed(metadata, "tx_hash")?,
            transaction_index: parse_metadata(metadata, "transaction_index")?,
            log_index: parse_metadata(metadata, "log_index")?,
            emitter: parse_fixed(metadata, "emitter")?,
            event_signature: parse_fixed(metadata, "event_signature")?,
        })
    }
}

fn metadata_value<'a>(
    metadata: &'a StorageMetadata,
    key: &'static str,
) -> Result<&'a str, ProjectionError> {
    metadata
        .get(key)
        .ok_or_else(|| ProjectionError::MalformedProjectionMetadata(format!("missing {key}")))
}

fn parse_metadata<T>(metadata: &StorageMetadata, key: &'static str) -> Result<T, ProjectionError>
where
    T: FromStr + Display,
{
    let encoded = metadata_value(metadata, key)?;
    let parsed: T = encoded
        .parse()
        .map_err(|_| ProjectionError::MalformedProjectionMetadata(format!("invalid {key}")))?;
    if parsed.to_string() != encoded {
        return Err(ProjectionError::MalformedProjectionMetadata(format!(
            "non-canonical {key}"
        )));
    }
    Ok(parsed)
}

fn parse_fixed<T>(metadata: &StorageMetadata, key: &'static str) -> Result<T, ProjectionError>
where
    T: FromStr + LowerHex,
{
    let encoded = metadata_value(metadata, key)?;
    let parsed: T = encoded
        .parse()
        .map_err(|_| ProjectionError::MalformedProjectionMetadata(format!("invalid {key}")))?;
    if format!("{parsed:#x}") != encoded {
        return Err(ProjectionError::MalformedProjectionMetadata(format!(
            "non-canonical {key}"
        )));
    }
    Ok(parsed)
}

/// Backend-neutral finalized log, including its canonical block-global index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedLog {
    pub log_index: u64,
    pub emitter: Address,
    pub data: LogData,
}

/// Backend-neutral successful or reverted receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedReceipt {
    pub tx_hash: B256,
    pub transaction_index: u64,
    pub success: bool,
    pub logs: Vec<FinalizedLog>,
}

/// Complete normalized receipt input for one exact finalized block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedBlock {
    pub number: u64,
    pub hash: B256,
    pub receipts: Vec<FinalizedReceipt>,
}

/// Prepared mutations for one successful receipt containing projection events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedReceipt {
    tx_hash: B256,
    transaction_index: u64,
    batch: AtomicWriteBatch,
}

impl PreparedReceipt {
    #[must_use]
    pub const fn tx_hash(&self) -> B256 {
        self.tx_hash
    }

    #[must_use]
    pub const fn transaction_index(&self) -> u64 {
        self.transaction_index
    }

    #[must_use]
    pub const fn batch(&self) -> &AtomicWriteBatch {
        &self.batch
    }
}

/// A fully decoded and simulated block. Constructed only after prepare succeeds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBlock {
    checkpoint: ProjectionCheckpoint,
    receipts: Vec<PreparedReceipt>,
}

impl PreparedBlock {
    #[must_use]
    pub const fn checkpoint(&self) -> ProjectionCheckpoint {
        self.checkpoint
    }

    #[must_use]
    pub fn receipts(&self) -> &[PreparedReceipt] {
        &self.receipts
    }
}

/// Result of applying one finalized block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionOutcome {
    Applied {
        checkpoint: ProjectionCheckpoint,
        receipt_batches: usize,
    },
    AlreadyApplied(ProjectionCheckpoint),
}

/// Deterministic projector over shared backend-neutral storage capabilities.
pub struct OffchainDataProjection {
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
    state: ProjectionState,
}

impl OffchainDataProjection {
    /// Opens a managed database or initializes an empty one.
    pub fn open(
        config: ProjectionConfig,
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
    ) -> Result<Self, ProjectionError> {
        let namespace = state_namespace()?;
        let key = state_key()?;
        let state = match reader.get_record(namespace, &key)? {
            Some(record) => {
                if record.metadata.is_some() {
                    return Err(ProjectionError::CorruptProjectionState(
                        "projection state must not carry metadata".to_owned(),
                    ));
                }
                let state = decode_state(record.value.as_bytes())?;
                validate_state(&state, config)?;
                state
            }
            None => {
                if contains_unmanaged_data(&reader)? {
                    return Err(ProjectionError::UnmanagedProjectionData);
                }
                let state = ProjectionState {
                    chain_id: config.chain_id,
                    genesis_hash: config.genesis_hash,
                    storage_schema_version: STORAGE_SCHEMA_VERSION,
                    start_block: config.start_block,
                    checkpoint: None,
                };
                writer.apply_atomic(&state_batch(&state)?)?;
                state
            }
        };
        Ok(Self {
            reader,
            writer,
            state,
        })
    }

    #[must_use]
    pub const fn state(&self) -> &ProjectionState {
        &self.state
    }

    /// Decodes and simulates the entire block without performing any writes.
    pub fn prepare_block(&self, block: &FinalizedBlock) -> Result<PreparedBlock, ProjectionError> {
        self.validate_next_block(block.number, block.hash)?;
        validate_normalized_block(block)?;

        let mut decoded_receipts = Vec::with_capacity(block.receipts.len());
        for receipt in &block.receipts {
            let mut events = Vec::new();
            for log in &receipt.logs {
                let Some(signature) = log.data.topics().first().copied() else {
                    continue;
                };
                let source = ProjectionSource {
                    block_number: block.number,
                    block_hash: block.hash,
                    tx_hash: receipt.tx_hash,
                    transaction_index: receipt.transaction_index,
                    log_index: log.log_index,
                    emitter: log.emitter,
                    event_signature: signature,
                };
                let recognized = is_projection_pair(log.emitter, signature);
                if !receipt.success {
                    if recognized {
                        return Err(ProjectionError::ProjectionLogInFailedReceipt(Box::new(
                            source,
                        )));
                    }
                    continue;
                }
                if let Some(event) = decode_event(source, &log.data)? {
                    events.push(event);
                }
            }
            decoded_receipts.push((receipt, events));
        }

        let tribute_reader = TributeRepositoryReader::new(self.reader.clone());
        let nod_reader = NodRepositoryReader::new(self.reader.clone());
        let mut tribute_ids = BTreeSet::new();
        let mut nod_ids = BTreeSet::new();
        let mut bucket_keys = BTreeSet::new();

        for (_, events) in &decoded_receipts {
            for event in events {
                match event.identity() {
                    EntityIdentity::Tribute(id) => {
                        tribute_ids.insert(id);
                    }
                    EntityIdentity::Nod(id) => {
                        nod_ids.insert(id);
                    }
                    EntityIdentity::Bucket(key) => {
                        bucket_keys.insert(key);
                    }
                }
            }
        }

        let tribute_ids: Vec<_> = tribute_ids.into_iter().collect();
        let nod_ids: Vec<_> = nod_ids.into_iter().collect();
        let bucket_keys: Vec<_> = bucket_keys.into_iter().collect();
        let mut tributes = BTreeMap::<U256, Option<TributeData>>::new();
        for (token_id, record) in tribute_ids
            .iter()
            .copied()
            .zip(tribute_reader.get_many_with_metadata(&tribute_ids)?)
        {
            tributes.insert(token_id, validate_existing_record("Tribute", record)?);
        }
        let mut nods = BTreeMap::<U256, Option<NodItemState>>::new();
        for (nod_id, record) in nod_ids
            .iter()
            .copied()
            .zip(nod_reader.get_many_with_metadata(&nod_ids)?)
        {
            nods.insert(nod_id, validate_existing_record("Nod", record)?);
        }
        let mut buckets = BTreeMap::<B256, Option<NodBucketState>>::new();
        for (bucket_key, record) in bucket_keys
            .iter()
            .copied()
            .zip(nod_reader.get_buckets_with_metadata(&bucket_keys)?)
        {
            buckets.insert(bucket_key, validate_existing_record("Nod bucket", record)?);
        }

        let mut prepared_receipts = Vec::new();
        for (receipt, events) in decoded_receipts {
            let mut batch = AtomicWriteBatch::new();
            for event in events {
                match event {
                    ProjectionEvent::TributeStored { source, body } => {
                        let id = body.token_id;
                        let old = tributes.get(&id).and_then(Option::as_ref);
                        let planned = plan_tribute_mutation(
                            old,
                            TributeMutation::Store {
                                token_id: id,
                                body: &body,
                                metadata: Some(source.to_storage_metadata()?),
                            },
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                        tributes.insert(id, Some(body));
                    }
                    ProjectionEvent::TributeDeleted { token_id } => {
                        let old = tributes.get(&token_id).and_then(Option::as_ref);
                        let planned =
                            plan_tribute_mutation(old, TributeMutation::Delete { token_id })?;
                        batch.extend(planned.operations().iter().cloned());
                        tributes.insert(token_id, None);
                    }
                    ProjectionEvent::NodStored { source, body } => {
                        let id = body.nod_id;
                        let old = nods.get(&id).and_then(Option::as_ref);
                        let planned = plan_nod_item_mutation(
                            old,
                            NodItemMutation::Store {
                                nod_id: id,
                                body: &body,
                                metadata: Some(source.to_storage_metadata()?),
                            },
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                        nods.insert(id, Some(body));
                    }
                    ProjectionEvent::NodDeleted { nod_id } => {
                        let old = nods.get(&nod_id).and_then(Option::as_ref);
                        let planned =
                            plan_nod_item_mutation(old, NodItemMutation::Delete { nod_id })?;
                        batch.extend(planned.operations().iter().cloned());
                        nods.insert(nod_id, None);
                    }
                    ProjectionEvent::BucketStored { source, body } => {
                        let key = body.bucket_key;
                        let old = buckets.get(&key).and_then(Option::as_ref);
                        let planned = plan_nod_bucket_mutation(
                            old,
                            NodBucketMutation::Store {
                                bucket_key: key,
                                body: &body,
                                metadata: Some(source.to_storage_metadata()?),
                            },
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                        buckets.insert(key, Some(body));
                    }
                    ProjectionEvent::BucketDeleted { bucket_key } => {
                        let old = buckets.get(&bucket_key).and_then(Option::as_ref);
                        let planned = plan_nod_bucket_mutation(
                            old,
                            NodBucketMutation::Delete { bucket_key },
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                        buckets.insert(bucket_key, None);
                    }
                }
            }
            if !batch.is_empty() {
                batch.validate()?;
                prepared_receipts.push(PreparedReceipt {
                    tx_hash: receipt.tx_hash,
                    transaction_index: receipt.transaction_index,
                    batch,
                });
            }
        }

        Ok(PreparedBlock {
            checkpoint: ProjectionCheckpoint {
                block_number: block.number,
                block_hash: block.hash,
            },
            receipts: prepared_receipts,
        })
    }

    /// Applies receipt batches sequentially and writes the checkpoint last.
    pub fn apply_prepared(
        &mut self,
        prepared: PreparedBlock,
    ) -> Result<ProjectionOutcome, ProjectionError> {
        match self.validate_next_block(
            prepared.checkpoint.block_number,
            prepared.checkpoint.block_hash,
        )? {
            NextBlock::AlreadyApplied(checkpoint) => {
                return Ok(ProjectionOutcome::AlreadyApplied(checkpoint));
            }
            NextBlock::Apply => {}
        }
        for receipt in &prepared.receipts {
            self.writer.apply_atomic(&receipt.batch)?;
        }
        let next_state = ProjectionState {
            checkpoint: Some(prepared.checkpoint),
            ..self.state.clone()
        };
        self.writer.apply_atomic(&state_batch(&next_state)?)?;
        self.state = next_state;
        Ok(ProjectionOutcome::Applied {
            checkpoint: prepared.checkpoint,
            receipt_batches: prepared.receipts.len(),
        })
    }

    /// Prepares and applies one exact finalized block.
    pub fn project_block(
        &mut self,
        block: &FinalizedBlock,
    ) -> Result<ProjectionOutcome, ProjectionError> {
        if let NextBlock::AlreadyApplied(checkpoint) =
            self.validate_next_block(block.number, block.hash)?
        {
            return Ok(ProjectionOutcome::AlreadyApplied(checkpoint));
        }
        let prepared = self.prepare_block(block)?;
        self.apply_prepared(prepared)
    }

    fn validate_next_block(&self, number: u64, hash: B256) -> Result<NextBlock, ProjectionError> {
        match self.state.checkpoint {
            Some(checkpoint) if checkpoint.block_number == number => {
                if checkpoint.block_hash == hash {
                    Ok(NextBlock::AlreadyApplied(checkpoint))
                } else {
                    Err(ProjectionError::CheckpointMismatch {
                        block_number: number,
                        expected: checkpoint.block_hash,
                        actual: hash,
                    })
                }
            }
            Some(checkpoint) => {
                let expected = checkpoint.block_number.checked_add(1).ok_or(
                    ProjectionError::NonSequentialBlock {
                        expected: checkpoint.block_number,
                        actual: number,
                    },
                )?;
                if number != expected {
                    return Err(ProjectionError::NonSequentialBlock {
                        expected,
                        actual: number,
                    });
                }
                Ok(NextBlock::Apply)
            }
            None if number == self.state.start_block => Ok(NextBlock::Apply),
            None => Err(ProjectionError::NonSequentialBlock {
                expected: self.state.start_block,
                actual: number,
            }),
        }
    }
}

fn validate_existing_record<T>(
    entity: &'static str,
    record: Option<(T, Option<StorageMetadata>)>,
) -> Result<Option<T>, ProjectionError> {
    let Some((body, metadata)) = record else {
        return Ok(None);
    };
    let metadata = metadata.ok_or(ProjectionError::MissingProjectionMetadata { entity })?;
    ProjectionSource::from_storage_metadata(&metadata)?;
    Ok(Some(body))
}

#[derive(Clone, Copy)]
enum NextBlock {
    Apply,
    AlreadyApplied(ProjectionCheckpoint),
}

#[derive(Clone, Copy)]
enum EntityIdentity {
    Tribute(U256),
    Nod(U256),
    Bucket(B256),
}

enum ProjectionEvent {
    TributeStored {
        source: ProjectionSource,
        body: TributeData,
    },
    TributeDeleted {
        token_id: U256,
    },
    NodStored {
        source: ProjectionSource,
        body: NodItemState,
    },
    NodDeleted {
        nod_id: U256,
    },
    BucketStored {
        source: ProjectionSource,
        body: NodBucketState,
    },
    BucketDeleted {
        bucket_key: B256,
    },
}

impl ProjectionEvent {
    fn identity(&self) -> EntityIdentity {
        match self {
            Self::TributeStored { body, .. } => EntityIdentity::Tribute(body.token_id),
            Self::TributeDeleted { token_id, .. } => EntityIdentity::Tribute(*token_id),
            Self::NodStored { body, .. } => EntityIdentity::Nod(body.nod_id),
            Self::NodDeleted { nod_id, .. } => EntityIdentity::Nod(*nod_id),
            Self::BucketStored { body, .. } => EntityIdentity::Bucket(body.bucket_key),
            Self::BucketDeleted { bucket_key, .. } => EntityIdentity::Bucket(*bucket_key),
        }
    }
}

fn is_projection_pair(emitter: Address, signature: B256) -> bool {
    (emitter == TRIBUTE_ADDRESS
        && (signature == ITribute::TributeBodyStored::SIGNATURE_HASH
            || signature == ITribute::TributeBodyDeleted::SIGNATURE_HASH))
        || (emitter == NOD_ADDRESS
            && (signature == INod::NodBodyStored::SIGNATURE_HASH
                || signature == INod::NodBodyDeleted::SIGNATURE_HASH
                || signature == INod::NodBucketBodyStored::SIGNATURE_HASH
                || signature == INod::NodBucketBodyDeleted::SIGNATURE_HASH))
}

fn decode_event(
    source: ProjectionSource,
    data: &LogData,
) -> Result<Option<ProjectionEvent>, ProjectionError> {
    let decoded = if source.emitter == TRIBUTE_ADDRESS
        && source.event_signature == ITribute::TributeBodyStored::SIGNATURE_HASH
    {
        let event = ITribute::TributeBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::TributeStored {
            source,
            body: TributeData {
                token_id: event.tokenId,
                owner: event.owner,
                worldwide_day: WorldwideDay::new(event.worldwideDay),
                issuance_amount_minor: event.issuanceAmountMinor,
                issuance_currency: event.issuanceCurrency,
                nominal_amount_minor: event.nominalAmountMinor,
                reference_currency: event.referenceCurrency,
                tribute_price_minor: event.tributePriceMinor,
                exclude_from_intex_issuance: event.excludeFromIntexIssuance,
            },
        })
    } else if source.emitter == TRIBUTE_ADDRESS
        && source.event_signature == ITribute::TributeBodyDeleted::SIGNATURE_HASH
    {
        let event = ITribute::TributeBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::TributeDeleted {
            token_id: event.tokenId,
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBodyStored::SIGNATURE_HASH
    {
        let event = INod::NodBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::NodStored {
            source,
            body: NodItemState {
                nod_id: event.nodId,
                owner: event.owner,
                gratis_load_minor: event.gratisLoadMinor,
                worldwide_day: WorldwideDay::new(event.worldwideDay),
                league_id: event.leagueId,
                floor_price_minor: event.floorPriceMinor,
                bucket_key: event.bucketKey,
                cost_amount_minor: event.costAmountMinor,
                issuance_currency: event.issuanceCurrency,
                reference_currency: event.referenceCurrency,
                issued_at: event.issuedAt,
            },
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBodyDeleted::SIGNATURE_HASH
    {
        let event = INod::NodBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::NodDeleted {
            nod_id: event.nodId,
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBucketBodyStored::SIGNATURE_HASH
    {
        let event = INod::NodBucketBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::BucketStored {
            source,
            body: NodBucketState {
                bucket_key: event.bucketKey,
                worldwide_day: WorldwideDay::new(event.worldwideDay),
                floor_price_minor: event.floorPriceMinor,
                is_qualified: event.isQualified,
                total_nods: event.totalNods,
                entry_price_minor: event.entryPriceMinor,
            },
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBucketBodyDeleted::SIGNATURE_HASH
    {
        let event = INod::NodBucketBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::BucketDeleted {
            bucket_key: event.bucketKey,
        })
    } else {
        None
    };
    Ok(decoded)
}

fn malformed_event(source: ProjectionSource, error: impl std::fmt::Display) -> ProjectionError {
    ProjectionError::MalformedProjectionEvent {
        event_source: Box::new(source),
        reason: error.to_string(),
    }
}

fn validate_normalized_block(block: &FinalizedBlock) -> Result<(), ProjectionError> {
    let mut expected_log_index = 0_u64;
    for (expected_index, receipt) in block.receipts.iter().enumerate() {
        let expected_index =
            u64::try_from(expected_index).map_err(|_| ProjectionError::TransactionIndexOverflow)?;
        if receipt.transaction_index != expected_index {
            return Err(ProjectionError::InvalidTransactionOrder {
                expected: expected_index,
                actual: receipt.transaction_index,
            });
        }
        for log in &receipt.logs {
            if log.log_index != expected_log_index {
                return Err(ProjectionError::InvalidLogOrder {
                    expected: expected_log_index,
                    actual: log.log_index,
                });
            }
            expected_log_index = expected_log_index
                .checked_add(1)
                .ok_or(ProjectionError::LogIndexOverflow)?;
        }
    }
    Ok(())
}

fn contains_unmanaged_data(reader: &StorageReaderHandle) -> Result<bool, ProjectionError> {
    for name in TRIBUTE_PROJECTION_NAMESPACES
        .iter()
        .chain(NOD_PROJECTION_NAMESPACES.iter())
    {
        let namespace = Namespace::new(*name)?;
        let request = ScanRequest::new(&[], None, 1)?;
        if !reader.scan_prefix(namespace, request)?.entries.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_state(
    state: &ProjectionState,
    config: ProjectionConfig,
) -> Result<(), ProjectionError> {
    if state.storage_schema_version != STORAGE_SCHEMA_VERSION {
        return Err(ProjectionError::ProjectionSchemaMismatch {
            expected: STORAGE_SCHEMA_VERSION,
            actual: state.storage_schema_version,
        });
    }
    if state.chain_id != config.chain_id
        || state.genesis_hash != config.genesis_hash
        || state.start_block != config.start_block
    {
        return Err(ProjectionError::ProjectionIdentityMismatch {
            expected: config,
            actual_chain_id: state.chain_id,
            actual_genesis_hash: state.genesis_hash,
            actual_start_block: state.start_block,
        });
    }
    if state
        .checkpoint
        .is_some_and(|checkpoint| checkpoint.block_number < state.start_block)
    {
        return Err(ProjectionError::CorruptProjectionState(
            "checkpoint precedes configured start block".to_owned(),
        ));
    }
    Ok(())
}

fn state_namespace() -> Result<Namespace, ProjectionError> {
    Ok(Namespace::new(PROJECTION_STATE_NAMESPACE)?)
}

fn state_key() -> Result<Key, ProjectionError> {
    Ok(Key::new(PROJECTION_STATE_KEY.to_vec())?)
}

fn state_batch(state: &ProjectionState) -> Result<AtomicWriteBatch, ProjectionError> {
    let bytes = postcard::to_stdvec(state).map_err(ProjectionError::StateEncode)?;
    let operation = AtomicWriteOperation::put_record(
        state_namespace()?,
        state_key()?,
        StoredValue::plain(Value::new(bytes)?),
    );
    Ok(AtomicWriteBatch::from_operations(vec![operation]))
}

fn decode_state(bytes: &[u8]) -> Result<ProjectionState, ProjectionError> {
    let (state, remainder): (ProjectionState, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(ProjectionError::StateDecode)?;
    if !remainder.is_empty() {
        return Err(ProjectionError::CorruptProjectionState(
            "projection state has trailing bytes".to_owned(),
        ));
    }
    Ok(state)
}

/// Stable projector failures; no backend-specific type crosses this boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProjectionError {
    #[error("off-chain storage failure: {0}")]
    Storage(#[from] StorageError),
    #[error("Tribute repository failure: {0}")]
    Tribute(#[from] TributeRepositoryError),
    #[error("Nod repository failure: {0}")]
    Nod(#[from] NodRepositoryError),
    #[error("failed to encode projection state")]
    StateEncode(#[source] postcard::Error),
    #[error("failed to decode projection state")]
    StateDecode(#[source] postcard::Error),
    #[error("corrupt projection state: {0}")]
    CorruptProjectionState(String),
    #[error("projection schema mismatch: expected {expected}, found {actual}")]
    ProjectionSchemaMismatch { expected: u32, actual: u32 },
    #[error("projection identity does not match configured chain")]
    ProjectionIdentityMismatch {
        expected: ProjectionConfig,
        actual_chain_id: u64,
        actual_genesis_hash: B256,
        actual_start_block: u64,
    },
    #[error("body/index records exist without projection state")]
    UnmanagedProjectionData,
    #[error(
        "checkpoint hash mismatch at block {block_number}: expected {expected}, found {actual}"
    )]
    CheckpointMismatch {
        block_number: u64,
        expected: B256,
        actual: B256,
    },
    #[error("non-sequential block: expected {expected}, found {actual}")]
    NonSequentialBlock { expected: u64, actual: u64 },
    #[error("invalid transaction order: expected {expected}, found {actual}")]
    InvalidTransactionOrder { expected: u64, actual: u64 },
    #[error("transaction index does not fit u64")]
    TransactionIndexOverflow,
    #[error("invalid block-global log order: expected {expected}, found {actual}")]
    InvalidLogOrder { expected: u64, actual: u64 },
    #[error("block-global log index overflow")]
    LogIndexOverflow,
    #[error("recognized projection log appears in a failed receipt at {0:?}")]
    ProjectionLogInFailedReceipt(Box<ProjectionSource>),
    #[error("malformed recognized projection event at {event_source:?}: {reason}")]
    MalformedProjectionEvent {
        event_source: Box<ProjectionSource>,
        reason: String,
    },
    #[error("malformed projection metadata: {0}")]
    MalformedProjectionMetadata(String),
    #[error("managed {entity} primary record has no projection metadata")]
    MissingProjectionMetadata { entity: &'static str },
}
