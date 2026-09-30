use std::{
    collections::BTreeMap,
    fmt::{Display, LowerHex},
    str::FromStr,
};

use alloy_primitives::{Address, B256};
use outbe_nod::projection::NOD_PROJECTION_NAMESPACES;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanRequest, StorageMetadata,
    StorageReaderHandle, StoredValue, Value,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{projection::TRIBUTE_PROJECTION_NAMESPACES, RetainedTributePin};
use serde::{Deserialize, Serialize};

use super::{ProjectionCheckpoint, ProjectionError};

/// Local representation version owned by this projector.
///
/// Version 2 adds the job-scoped retained Tribute body and day-index
/// namespaces. Version 1 nodes must fail closed instead of silently ignoring
/// those records during OCOMP retention and release.
pub const STORAGE_SCHEMA_VERSION: u32 = 2;
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

/// Node-owned, read-only selector for the one active PoC retention pin.
///
/// The projector supplies the partition day discovered from the finalized
/// Tribute event. Callers cannot pass a pin through [`FinalizedBlock`].
pub trait TributeRetentionSelector: Send + Sync {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String>;
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

pub(super) fn metadata_value<'a>(
    metadata: &'a StorageMetadata,
    key: &'static str,
) -> Result<&'a str, ProjectionError> {
    metadata
        .get(key)
        .ok_or_else(|| ProjectionError::MalformedProjectionMetadata(format!("missing {key}")))
}

pub(super) fn parse_metadata<T>(
    metadata: &StorageMetadata,
    key: &'static str,
) -> Result<T, ProjectionError>
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

pub(super) fn parse_fixed<T>(
    metadata: &StorageMetadata,
    key: &'static str,
) -> Result<T, ProjectionError>
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

/// Reads and validates the managed projection state without acquiring a writer.
///
/// Snapshot exporters use this narrow surface only as an availability signal.
/// The checkpoint is never input authority; exported bodies still have to close
/// against the exact finalized compressed-entity snapshot.
pub fn read_projection_state(
    config: ProjectionConfig,
    reader: StorageReaderHandle,
) -> Result<Option<ProjectionState>, ProjectionError> {
    let namespace = state_namespace()?;
    let key = state_key()?;
    let Some(record) = reader.get_record(namespace, &key)? else {
        return Ok(None);
    };
    if record.metadata.is_some() {
        return Err(ProjectionError::CorruptProjectionState(
            "projection state must not carry metadata".to_owned(),
        ));
    }
    let state = decode_state(record.value.as_bytes())?;
    validate_state(&state, config)?;
    Ok(Some(state))
}

pub(super) fn contains_unmanaged_data(
    reader: &StorageReaderHandle,
) -> Result<bool, ProjectionError> {
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

pub(super) fn validate_state(
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

pub(super) fn state_namespace() -> Result<Namespace, ProjectionError> {
    Ok(Namespace::new(PROJECTION_STATE_NAMESPACE)?)
}

pub(super) fn state_key() -> Result<Key, ProjectionError> {
    Ok(Key::new(PROJECTION_STATE_KEY.to_vec())?)
}

pub(super) fn state_batch(state: &ProjectionState) -> Result<AtomicWriteBatch, ProjectionError> {
    let bytes = postcard::to_stdvec(state).map_err(ProjectionError::StateEncode)?;
    let operation = AtomicWriteOperation::put_record(
        state_namespace()?,
        state_key()?,
        StoredValue::plain(Value::new(bytes)?),
    );
    Ok(AtomicWriteBatch::from_operations(vec![operation]))
}

pub(super) fn decode_state(bytes: &[u8]) -> Result<ProjectionState, ProjectionError> {
    let (state, remainder): (ProjectionState, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(ProjectionError::StateDecode)?;
    if !remainder.is_empty() {
        return Err(ProjectionError::CorruptProjectionState(
            "projection state has trailing bytes".to_owned(),
        ));
    }
    Ok(state)
}
