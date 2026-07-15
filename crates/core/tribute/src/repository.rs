//! Typed off-chain persistence boundary for Tribute bodies and indexes.

use alloy_primitives::Address;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    decode_stored_tribute_v1, encode_tribute_v1, CanonicalBodyError, EntityId36, StoredBody,
    TributeBodyV1,
};
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageError, StorageMetadata, StorageReaderHandle,
    StorageWriterHandle, Value, MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::TributeData;

pub(crate) const TRIBUTES_NAMESPACE: &str = "tributes";
pub(crate) const TRIBUTES_BY_OWNER_NAMESPACE: &str = "tributes_by_owner";
pub(crate) const TRIBUTES_BY_DAY_NAMESPACE: &str = "tributes_by_day";
const PRIMARY_KEY_LEN: usize = EntityId36::LEN;
const OWNER_INDEX_KEY_LEN: usize = 20 + PRIMARY_KEY_LEN;
const DAY_INDEX_KEY_LEN: usize = 4 + PRIMARY_KEY_LEN;

/// Domain-level request for one ascending page of Tributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributePageRequest {
    /// Exclusive Tribute ID cursor.
    pub after: Option<EntityId36>,
    /// Requested number of records, in `1..=MAX_SCAN_ENTRIES`.
    pub limit: usize,
}

/// One ascending, all-or-error page of Tribute bodies.
pub struct TributePage {
    /// Decoded Tribute bodies.
    pub records: Vec<TributeData>,
    /// Exclusive cursor for the next page, when more records exist.
    pub next_after: Option<EntityId36>,
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
    CanonicalBody(#[from] CanonicalBodyError),
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
    #[error("Tribute {index} index points to missing body {tribute_id}")]
    DanglingIndex {
        index: &'static str,
        tribute_id: EntityId36,
    },
    /// The selecting primary key and embedded body ID disagree.
    #[error("Tribute primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch {
        expected: EntityId36,
        actual: EntityId36,
    },
    /// An owner index selected a body owned by someone else.
    #[error("Tribute owner index/body mismatch for {tribute_id}")]
    IndexedOwnerMismatch { tribute_id: EntityId36 },
    /// A day index selected a body assigned to another day.
    #[error("Tribute day index/body mismatch for {tribute_id}")]
    IndexedDayMismatch { tribute_id: EntityId36 },
    /// A projection session may mutate only identities loaded into its repository snapshot.
    #[error("Tribute projection identity {tribute_id} was not loaded")]
    UntrackedProjectionIdentity { tribute_id: EntityId36 },
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
    pub fn get(
        &self,
        tribute_id: EntityId36,
    ) -> Result<Option<TributeData>, TributeRepositoryError> {
        Ok(self
            .get_with_metadata(tribute_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads one Tribute body together with optional primary provenance.
    pub fn get_with_metadata(
        &self,
        tribute_id: EntityId36,
    ) -> Result<Option<(TributeData, Option<StorageMetadata>)>, TributeRepositoryError> {
        let key = primary_key(tribute_id)?;
        let Some(record) = self
            .storage
            .get_record(namespace(TRIBUTES_NAMESPACE)?, &key)?
        else {
            return Ok(None);
        };
        decode_body(tribute_id, record.value.as_bytes()).map(|body| Some((body, record.metadata)))
    }

    /// Batch-loads bodies and metadata in the same order as the supplied identities.
    pub fn get_many_with_metadata(
        &self,
        tribute_ids: &[EntityId36],
    ) -> Result<Vec<Option<TributeRecordWithMetadata>>, TributeRepositoryError> {
        let keys = tribute_ids
            .iter()
            .copied()
            .map(primary_key)
            .collect::<Result<Vec<_>, _>>()?;
        self.storage
            .get_records(namespace(TRIBUTES_NAMESPACE)?, &keys)?
            .into_iter()
            .zip(tribute_ids.iter().copied())
            .map(|(record, tribute_id)| {
                record
                    .map(|record| {
                        decode_body(tribute_id, record.value.as_bytes())
                            .map(|body| (body, record.metadata))
                    })
                    .transpose()
            })
            .collect()
    }

    /// Loads an opaque repository-owned snapshot for projection planning and in-block overlay.
    pub fn projection_session(
        &self,
        tribute_ids: &[EntityId36],
    ) -> Result<crate::projection::TributeProjectionSession, TributeRepositoryError> {
        let records = self.get_many_with_metadata(tribute_ids)?;
        Ok(crate::projection::TributeProjectionSession::from_records(
            tribute_ids,
            records,
        ))
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
            let tribute_id = parse_owner_index(&entry, owner)?;
            let body = self
                .get(tribute_id)?
                .ok_or(TributeRepositoryError::DanglingIndex {
                    index: "owner",
                    tribute_id,
                })?;
            if body.owner != owner {
                return Err(TributeRepositoryError::IndexedOwnerMismatch { tribute_id });
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
            let tribute_id = parse_day_index(&entry, worldwide_day)?;
            let body = self
                .get(tribute_id)?
                .ok_or(TributeRepositoryError::DanglingIndex {
                    index: "day",
                    tribute_id,
                })?;
            if body.worldwide_day != worldwide_day {
                return Err(TributeRepositoryError::IndexedDayMismatch { tribute_id });
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
        let mut session = self.reader.projection_session(&[tribute.tribute_id])?;
        let batch = session.store(tribute.tribute_id, encode_body(tribute)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes a body and its derived indexes. Missing bodies are a success.
    pub fn delete(&self, tribute_id: EntityId36) -> Result<(), TributeRepositoryError> {
        let mut session = self.reader.projection_session(&[tribute_id])?;
        let batch = session.delete(tribute_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }
}

pub(crate) fn namespace(name: &'static str) -> Result<Namespace, TributeRepositoryError> {
    Ok(Namespace::new(name)?)
}

pub(crate) fn encode_body(tribute: &TributeData) -> Result<Value, TributeRepositoryError> {
    let payload = encode_tribute_v1(&canonical_body(tribute))?;
    Ok(Value::new(StoredBody::new_v1(payload)?.encode())?)
}

pub(crate) fn decode_body(
    tribute_id: EntityId36,
    bytes: &[u8],
) -> Result<TributeData, TributeRepositoryError> {
    let body = from_canonical_body(decode_stored_tribute_v1(bytes)?);
    if body.tribute_id != tribute_id {
        return Err(TributeRepositoryError::PrimaryKeyBodyMismatch {
            expected: tribute_id,
            actual: body.tribute_id,
        });
    }
    Ok(body)
}

pub(crate) fn primary_key(tribute_id: EntityId36) -> Result<Key, TributeRepositoryError> {
    Ok(Key::new(tribute_id.as_bytes().to_vec())?)
}

pub(crate) fn owner_index_key(
    owner: Address,
    tribute_id: EntityId36,
) -> Result<Key, TributeRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(tribute_id.as_bytes());
    Ok(Key::new(bytes)?)
}

pub(crate) fn day_index_key(
    worldwide_day: WorldwideDay,
    tribute_id: EntityId36,
) -> Result<Key, TributeRepositoryError> {
    let mut bytes = Vec::with_capacity(DAY_INDEX_KEY_LEN);
    bytes.extend_from_slice(&worldwide_day.value().to_be_bytes());
    bytes.extend_from_slice(tribute_id.as_bytes());
    Ok(Key::new(bytes)?)
}

fn parse_owner_index(
    entry: &ScanEntry,
    owner: Address,
) -> Result<EntityId36, TributeRepositoryError> {
    validate_empty_index(entry, "owner")?;
    let bytes = entry.key.as_bytes();
    if bytes.len() != OWNER_INDEX_KEY_LEN || &bytes[..20] != owner.as_slice() {
        return Err(TributeRepositoryError::MalformedIndexKey { index: "owner" });
    }
    parse_id_suffix(&bytes[20..], "owner")
}

fn parse_day_index(
    entry: &ScanEntry,
    day: WorldwideDay,
) -> Result<EntityId36, TributeRepositoryError> {
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

fn parse_id_suffix(
    bytes: &[u8],
    index: &'static str,
) -> Result<EntityId36, TributeRepositoryError> {
    EntityId36::try_from(bytes).map_err(|_| TributeRepositoryError::MalformedIndexKey { index })
}

fn validate_page_limit(limit: usize) -> Result<(), TributeRepositoryError> {
    if !(1..=MAX_SCAN_ENTRIES).contains(&limit) {
        return Err(TributeRepositoryError::InvalidPageLimit { limit });
    }
    Ok(())
}

fn next_cursor(has_more: bool, records: &[TributeData]) -> Option<EntityId36> {
    has_more
        .then(|| records.last().map(|record| record.tribute_id))
        .flatten()
}

/// Converts the runtime body into its normative v1 payload model.
pub fn canonical_body(body: &TributeData) -> TributeBodyV1 {
    TributeBodyV1 {
        tribute_id: body.tribute_id,
        owner: body.owner,
        worldwide_day: body.worldwide_day,
        issuance_amount_minor: body.issuance_amount_minor,
        issuance_currency: body.issuance_currency,
        nominal_amount_minor: body.nominal_amount_minor,
        reference_currency: body.reference_currency,
        tribute_price_minor: body.tribute_price_minor,
        exclude_from_intex_issuance: body.exclude_from_intex_issuance,
    }
}

/// Converts a validated normative v1 payload into the runtime body type.
pub fn from_canonical_body(body: TributeBodyV1) -> TributeData {
    TributeData {
        tribute_id: body.tribute_id,
        owner: body.owner,
        worldwide_day: body.worldwide_day,
        issuance_amount_minor: body.issuance_amount_minor,
        issuance_currency: body.issuance_currency,
        nominal_amount_minor: body.nominal_amount_minor,
        reference_currency: body.reference_currency,
        tribute_price_minor: body.tribute_price_minor,
        exclude_from_intex_issuance: body.exclude_from_intex_issuance,
    }
}
