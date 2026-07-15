//! Typed off-chain persistence boundary for Tribute bodies and indexes.

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageError, StorageReaderHandle, StorageWriterHandle,
    Value, MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::TributeData;

const TRIBUTES_NAMESPACE: &str = "tributes";
const TRIBUTES_BY_OWNER_NAMESPACE: &str = "tributes_by_owner";
const TRIBUTES_BY_DAY_NAMESPACE: &str = "tributes_by_day";
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
        let key = primary_key(token_id)?;
        let Some(value) = self.storage.get(namespace(TRIBUTES_NAMESPACE)?, &key)? else {
            return Ok(None);
        };
        decode_body(token_id, value.as_bytes()).map(Some)
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
pub struct TributeRepositoryWriter {
    reader: TributeRepositoryReader,
    writer: StorageWriterHandle,
}

impl TributeRepositoryWriter {
    /// Creates a writer. Both handles must address the same adapter instance.
    ///
    /// The read handle is required for replacement and deletion. Until the
    /// transactional projection introduced by ADR-004, callers must also
    /// serialize concurrent mutations of the same Tribute ID.
    #[must_use]
    pub fn new(reader: StorageReaderHandle, writer: StorageWriterHandle) -> Self {
        Self {
            reader: TributeRepositoryReader::new(reader),
            writer,
        }
    }

    /// Inserts or replaces one body and its owner/day indexes.
    ///
    /// Multi-key writes are intentionally non-atomic: the first storage failure
    /// is returned and already-completed steps are not rolled back.
    pub fn put(&self, tribute: &TributeData) -> Result<(), TributeRepositoryError> {
        let old = self.reader.get(tribute.token_id)?;
        let primary = primary_key(tribute.token_id)?;
        let encoded = encode_body(tribute)?;
        let owner_index = owner_index_key(tribute.owner, tribute.token_id)?;
        let day_index = day_index_key(tribute.worldwide_day, tribute.token_id)?;
        let empty = Value::new(Vec::new())?;

        self.writer
            .put(namespace(TRIBUTES_NAMESPACE)?, &primary, &encoded)?;
        self.writer.put(
            namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
            &owner_index,
            &empty,
        )?;
        self.writer
            .put(namespace(TRIBUTES_BY_DAY_NAMESPACE)?, &day_index, &empty)?;

        if let Some(old) = old {
            if old.owner != tribute.owner {
                self.writer.delete(
                    namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                    &owner_index_key(old.owner, old.token_id)?,
                )?;
            }
            if old.worldwide_day != tribute.worldwide_day {
                self.writer.delete(
                    namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                    &day_index_key(old.worldwide_day, old.token_id)?,
                )?;
            }
        }
        Ok(())
    }

    /// Deletes a body and its derived indexes. Missing bodies are a success.
    pub fn delete(&self, token_id: U256) -> Result<(), TributeRepositoryError> {
        let Some(old) = self.reader.get(token_id)? else {
            return Ok(());
        };
        self.writer.delete(
            namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
            &owner_index_key(old.owner, token_id)?,
        )?;
        self.writer.delete(
            namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
            &day_index_key(old.worldwide_day, token_id)?,
        )?;
        self.writer
            .delete(namespace(TRIBUTES_NAMESPACE)?, &primary_key(token_id)?)?;
        Ok(())
    }
}

fn namespace(name: &'static str) -> Result<Namespace, TributeRepositoryError> {
    Ok(Namespace::new(name)?)
}

fn encode_body(tribute: &TributeData) -> Result<Value, TributeRepositoryError> {
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

fn primary_key(token_id: U256) -> Result<Key, TributeRepositoryError> {
    Ok(Key::new(token_id.to_be_bytes::<PRIMARY_KEY_LEN>())?)
}

fn owner_index_key(owner: Address, token_id: U256) -> Result<Key, TributeRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(&token_id.to_be_bytes::<PRIMARY_KEY_LEN>());
    Ok(Key::new(bytes)?)
}

fn day_index_key(
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
