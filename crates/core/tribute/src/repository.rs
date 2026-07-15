//! Typed off-chain persistence boundary for Tribute bodies and indexes.

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageError, StorageMetadata, StorageReaderHandle,
    StorageWriterHandle, Value, MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::TributeData;

pub(crate) const TRIBUTES_NAMESPACE: &str = "tributes";
pub(crate) const TRIBUTES_BY_OWNER_NAMESPACE: &str = "tributes_by_owner";
pub(crate) const TRIBUTES_BY_DAY_NAMESPACE: &str = "tributes_by_day";
const PRIMARY_KEY_LEN: usize = 32;
const OWNER_INDEX_KEY_LEN: usize = 20 + PRIMARY_KEY_LEN;
const DAY_INDEX_KEY_LEN: usize = 4 + PRIMARY_KEY_LEN;

/// Domain-level request for one ascending page of Tributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributePageRequest {
    /// Exclusive Tribute ID cursor.
    pub after: Option<U256>,
    /// Requested number of records, in `1..=MAX_SCAN_ENTRIES`.
    pub limit: usize,
}

/// One ascending, all-or-error page of Tribute bodies.
pub struct TributePage {
    /// Decoded Tribute bodies.
    pub records: Vec<TributeData>,
    /// Exclusive cursor for the next page, when more records exist.
    pub next_after: Option<U256>,
}

/// One decoded Tribute body and optional primary storage metadata.
pub type TributeRecordWithMetadata = (TributeData, Option<StorageMetadata>);

/// Failure at the typed Tribute persistence boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TributeRepositoryError {
    /// Backend-neutral storage failure.
    #[error("off-chain storage failure: {0}")]
    Storage(#[from] StorageError),
    /// A Tribute body could not be encoded.
    #[error("failed to encode Tribute body")]
    Encode(#[source] postcard::Error),
    /// A stored Tribute body is not valid postcard data.
    #[error("failed to decode Tribute body")]
    Decode(#[source] postcard::Error),
    /// Page bounds are outside the shared storage contract.
    #[error("page limit {limit} is outside 1..={MAX_SCAN_ENTRIES}")]
    InvalidPageLimit { limit: usize },
    /// A primary key returned by a scan is not one big-endian U256.
    #[error("malformed Tribute primary key")]
    MalformedPrimaryKey,
    /// A secondary-index key violates its fixed binary layout.
    #[error("malformed Tribute {index} index key")]
    MalformedIndexKey { index: &'static str },
    /// Secondary-index values must be exactly empty.
    #[error("Tribute {index} index value is not empty")]
    NonEmptyIndexValue { index: &'static str },
    /// Secondary-index documents must not carry primary provenance.
    #[error("Tribute {index} index unexpectedly carries metadata")]
    IndexMetadata { index: &'static str },
    /// An index selects a missing primary body.
    #[error("Tribute {index} index points to missing body {token_id}")]
    DanglingIndex { index: &'static str, token_id: U256 },
    /// The selecting primary key and embedded body ID disagree.
    #[error("Tribute primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch { expected: U256, actual: U256 },
    /// An owner index selected a body owned by someone else.
    #[error("Tribute owner index/body mismatch for {token_id}")]
    IndexedOwnerMismatch { token_id: U256 },
    /// A day index selected a body assigned to another day.
    #[error("Tribute day index/body mismatch for {token_id}")]
    IndexedDayMismatch { token_id: U256 },
}

/// Cloneable read authority for Tribute bodies and typed indexes.
#[derive(Clone)]
pub struct TributeRepositoryReader {
    storage: StorageReaderHandle,
}

impl TributeRepositoryReader {
    /// Creates a typed Tribute reader over a backend-neutral storage handle.
    #[must_use]
    pub fn new(storage: StorageReaderHandle) -> Self {
        Self { storage }
    }

    /// Loads one Tribute body and verifies its embedded identity.
    pub fn get(&self, token_id: U256) -> Result<Option<TributeData>, TributeRepositoryError> {
        Ok(self
            .get_with_metadata(token_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads one Tribute body together with optional primary provenance.
    pub fn get_with_metadata(
        &self,
        token_id: U256,
    ) -> Result<Option<(TributeData, Option<StorageMetadata>)>, TributeRepositoryError> {
        let key = primary_key(token_id)?;
        let Some(record) = self
            .storage
            .get_record(namespace(TRIBUTES_NAMESPACE)?, &key)?
        else {
            return Ok(None);
        };
        decode_body(token_id, record.value.as_bytes()).map(|body| Some((body, record.metadata)))
    }

    /// Batch-loads bodies and metadata in the same order as the supplied identities.
    pub fn get_many_with_metadata(
        &self,
        token_ids: &[U256],
    ) -> Result<Vec<Option<TributeRecordWithMetadata>>, TributeRepositoryError> {
        let keys = token_ids
            .iter()
            .copied()
            .map(primary_key)
            .collect::<Result<Vec<_>, _>>()?;
        self.storage
            .get_records(namespace(TRIBUTES_NAMESPACE)?, &keys)?
            .into_iter()
            .zip(token_ids.iter().copied())
            .map(|(record, token_id)| {
                record
                    .map(|record| {
                        decode_body(token_id, record.value.as_bytes())
                            .map(|body| (body, record.metadata))
                    })
                    .transpose()
            })
            .collect()
    }

    /// Lists one owner's Tributes in ascending ID order.
    pub fn list_by_owner(
        &self,
        owner: Address,
        request: TributePageRequest,
    ) -> Result<TributePage, TributeRepositoryError> {
        validate_page_limit(request.limit)?;
        let prefix = owner.as_slice();
        let after = request
            .after
            .map(|id| owner_index_key(owner, id))
            .transpose()?;
        let scan = ScanRequest::new(prefix, after.as_ref(), request.limit)?;
        let page = self
            .storage
            .scan_prefix(namespace(TRIBUTES_BY_OWNER_NAMESPACE)?, scan)?;
        let has_more = page.next_after.is_some();
        let mut records = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let token_id = parse_owner_index(&entry, owner)?;
            let body = self
                .get(token_id)?
                .ok_or(TributeRepositoryError::DanglingIndex {
                    index: "owner",
                    token_id,
                })?;
            if body.owner != owner {
                return Err(TributeRepositoryError::IndexedOwnerMismatch { token_id });
            }
            records.push(body);
        }
        Ok(TributePage {
            next_after: next_cursor(has_more, &records),
            records,
        })
    }

    /// Lists one worldwide day's Tributes in ascending ID order.
    pub fn list_by_day(
        &self,
        worldwide_day: WorldwideDay,
        request: TributePageRequest,
    ) -> Result<TributePage, TributeRepositoryError> {
        validate_page_limit(request.limit)?;
        let prefix = worldwide_day.value().to_be_bytes();
        let after = request
            .after
            .map(|id| day_index_key(worldwide_day, id))
            .transpose()?;
        let scan = ScanRequest::new(&prefix, after.as_ref(), request.limit)?;
        let page = self
            .storage
            .scan_prefix(namespace(TRIBUTES_BY_DAY_NAMESPACE)?, scan)?;
        let has_more = page.next_after.is_some();
        let mut records = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let token_id = parse_day_index(&entry, worldwide_day)?;
            let body = self
                .get(token_id)?
                .ok_or(TributeRepositoryError::DanglingIndex {
                    index: "day",
                    token_id,
                })?;
            if body.worldwide_day != worldwide_day {
                return Err(TributeRepositoryError::IndexedDayMismatch { token_id });
            }
            records.push(body);
        }
        Ok(TributePage {
            next_after: next_cursor(has_more, &records),
            records,
        })
    }
}

/// Cloneable write authority for Tribute bodies and derived indexes.
///
/// Callers must serialize mutations of the same Tribute identity. Each resulting body/index batch
/// is atomic, but the old-body read used to plan replacement or deletion precedes that batch.
pub struct TributeRepositoryWriter {
    reader: TributeRepositoryReader,
    writer: StorageWriterHandle,
}

impl TributeRepositoryWriter {
    /// Creates a writer. Both handles must address the same adapter instance.
    ///
    /// The read handle is required for replacement and deletion.
    #[must_use]
    pub fn new(reader: StorageReaderHandle, writer: StorageWriterHandle) -> Self {
        Self {
            reader: TributeRepositoryReader::new(reader),
            writer,
        }
    }

    /// Inserts or replaces one body and its owner/day indexes.
    pub fn put(&self, tribute: &TributeData) -> Result<(), TributeRepositoryError> {
        let old = self.reader.get(tribute.token_id)?;
        let batch = crate::projection::plan_tribute_mutation(
            old.as_ref(),
            crate::projection::TributeMutation::Store {
                token_id: tribute.token_id,
                body: tribute,
                metadata: None,
            },
        )?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes a body and its derived indexes. Missing bodies are a success.
    pub fn delete(&self, token_id: U256) -> Result<(), TributeRepositoryError> {
        let old = self.reader.get(token_id)?;
        let batch = crate::projection::plan_tribute_mutation(
            old.as_ref(),
            crate::projection::TributeMutation::Delete { token_id },
        )?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }
}

pub(crate) fn namespace(name: &'static str) -> Result<Namespace, TributeRepositoryError> {
    Ok(Namespace::new(name)?)
}

pub(crate) fn encode_body(tribute: &TributeData) -> Result<Value, TributeRepositoryError> {
    let bytes = postcard::to_stdvec(tribute).map_err(TributeRepositoryError::Encode)?;
    Ok(Value::new(bytes)?)
}

fn decode_body(token_id: U256, bytes: &[u8]) -> Result<TributeData, TributeRepositoryError> {
    let (body, remainder): (TributeData, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(TributeRepositoryError::Decode)?;
    if !remainder.is_empty() {
        return Err(TributeRepositoryError::Decode(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    if body.token_id != token_id {
        return Err(TributeRepositoryError::PrimaryKeyBodyMismatch {
            expected: token_id,
            actual: body.token_id,
        });
    }
    Ok(body)
}

pub(crate) fn primary_key(token_id: U256) -> Result<Key, TributeRepositoryError> {
    Ok(Key::new(token_id.to_be_bytes::<PRIMARY_KEY_LEN>())?)
}

pub(crate) fn owner_index_key(
    owner: Address,
    token_id: U256,
) -> Result<Key, TributeRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(&token_id.to_be_bytes::<PRIMARY_KEY_LEN>());
    Ok(Key::new(bytes)?)
}

pub(crate) fn day_index_key(
    worldwide_day: WorldwideDay,
    token_id: U256,
) -> Result<Key, TributeRepositoryError> {
    let mut bytes = Vec::with_capacity(DAY_INDEX_KEY_LEN);
    bytes.extend_from_slice(&worldwide_day.value().to_be_bytes());
    bytes.extend_from_slice(&token_id.to_be_bytes::<PRIMARY_KEY_LEN>());
    Ok(Key::new(bytes)?)
}

fn parse_owner_index(entry: &ScanEntry, owner: Address) -> Result<U256, TributeRepositoryError> {
    validate_empty_index(entry, "owner")?;
    let bytes = entry.key.as_bytes();
    if bytes.len() != OWNER_INDEX_KEY_LEN || &bytes[..20] != owner.as_slice() {
        return Err(TributeRepositoryError::MalformedIndexKey { index: "owner" });
    }
    parse_id_suffix(&bytes[20..], "owner")
}

fn parse_day_index(entry: &ScanEntry, day: WorldwideDay) -> Result<U256, TributeRepositoryError> {
    validate_empty_index(entry, "day")?;
    let bytes = entry.key.as_bytes();
    if bytes.len() != DAY_INDEX_KEY_LEN || bytes[..4] != day.value().to_be_bytes() {
        return Err(TributeRepositoryError::MalformedIndexKey { index: "day" });
    }
    parse_id_suffix(&bytes[4..], "day")
}

fn validate_empty_index(
    entry: &ScanEntry,
    index: &'static str,
) -> Result<(), TributeRepositoryError> {
    if !entry.value.as_bytes().is_empty() {
        return Err(TributeRepositoryError::NonEmptyIndexValue { index });
    }
    if entry.metadata.is_some() {
        return Err(TributeRepositoryError::IndexMetadata { index });
    }
    Ok(())
}

fn parse_id_suffix(bytes: &[u8], index: &'static str) -> Result<U256, TributeRepositoryError> {
    let bytes: [u8; PRIMARY_KEY_LEN] = bytes
        .try_into()
        .map_err(|_| TributeRepositoryError::MalformedIndexKey { index })?;
    Ok(U256::from_be_bytes(bytes))
}

fn validate_page_limit(limit: usize) -> Result<(), TributeRepositoryError> {
    if !(1..=MAX_SCAN_ENTRIES).contains(&limit) {
        return Err(TributeRepositoryError::InvalidPageLimit { limit });
    }
    Ok(())
}

fn next_cursor(has_more: bool, records: &[TributeData]) -> Option<U256> {
    has_more
        .then(|| records.last().map(|record| record.token_id))
        .flatten()
}
