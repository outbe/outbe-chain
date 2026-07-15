//! Typed off-chain persistence boundary for Nod item and bucket bodies.

use alloy_primitives::Address;
use outbe_compressed_entities::{
    decode_stored_nod_bucket_v1, decode_stored_nod_item_v1, encode_nod_bucket_v1,
    encode_nod_item_v1, CanonicalBodyError, EntityId36, NodBucketBodyV1, NodItemBodyV1, StoredBody,
};
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageError, StorageMetadata, StorageReaderHandle,
    StorageWriterHandle, Value, MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::{NodBucketState, NodItemState};

pub(crate) const NODS_NAMESPACE: &str = "nods";
pub(crate) const NOD_BUCKETS_NAMESPACE: &str = "nod_buckets";
pub(crate) const NODS_BY_OWNER_NAMESPACE: &str = "nods_by_owner";
const PRIMARY_KEY_LEN: usize = EntityId36::LEN;
const OWNER_INDEX_KEY_LEN: usize = 20 + PRIMARY_KEY_LEN;

/// Domain-level request for one ascending page of Nods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodPageRequest {
    /// Exclusive Nod ID cursor.
    pub after: Option<EntityId36>,
    /// Requested number of records, in `1..=MAX_SCAN_ENTRIES`.
    pub limit: usize,
}

/// One ascending, all-or-error page of Nod item bodies.
pub struct NodPage {
    /// Decoded Nod item bodies.
    pub records: Vec<NodItemState>,
    /// Exclusive cursor for the next page, when more records exist.
    pub next_after: Option<EntityId36>,
}

/// One decoded Nod item and optional primary storage metadata.
pub type NodItemRecordWithMetadata = (NodItemState, Option<StorageMetadata>);
/// One decoded Nod bucket and optional primary storage metadata.
pub type NodBucketRecordWithMetadata = (NodBucketState, Option<StorageMetadata>);

/// Failure at the typed Nod persistence boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodRepositoryError {
    /// Backend-neutral storage failure.
    #[error("off-chain storage failure: {0}")]
    Storage(#[from] StorageError),
    /// StoredBody or typed payload violates the canonical profile.
    #[error("invalid canonical Nod body: {0}")]
    CanonicalBody(#[from] CanonicalBodyError),
    /// Page bounds are outside the shared storage contract.
    #[error("page limit {limit} is outside 1..={MAX_SCAN_ENTRIES}")]
    InvalidPageLimit { limit: usize },
    /// A primary item key is not one big-endian U256.
    #[error("malformed Nod primary key")]
    MalformedPrimaryKey,
    /// An owner-index key violates its fixed binary layout.
    #[error("malformed Nod owner index key")]
    MalformedIndexKey,
    /// Owner-index values must be exactly empty.
    #[error("Nod owner index value is not empty")]
    NonEmptyIndexValue,
    /// Owner-index documents must not carry primary provenance.
    #[error("Nod owner index unexpectedly carries metadata")]
    IndexMetadata,
    /// An owner index selects a missing primary body.
    #[error("Nod owner index points to missing body {nod_id}")]
    DanglingIndex { nod_id: EntityId36 },
    /// The selecting primary key and embedded body ID disagree.
    #[error("Nod primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch {
        expected: EntityId36,
        actual: EntityId36,
    },
    /// An owner index selected a body owned by someone else.
    #[error("Nod owner index/body mismatch for {nod_id}")]
    IndexedOwnerMismatch { nod_id: EntityId36 },
    /// The selecting bucket key and embedded body key disagree.
    #[error("Nod bucket ID/body mismatch: expected {expected}, found {actual}")]
    BucketIdBodyMismatch {
        expected: EntityId36,
        actual: EntityId36,
    },
    /// A projection session may mutate only identities loaded into its repository snapshot.
    #[error("{entity} projection identity {identity} was not loaded")]
    UntrackedProjectionIdentity {
        entity: &'static str,
        identity: EntityId36,
    },
}

/// Cloneable read authority for Nod item and bucket bodies.
#[derive(Clone)]
pub struct NodRepositoryReader {
    storage: StorageReaderHandle,
}

impl NodRepositoryReader {
    /// Creates a typed Nod reader over a backend-neutral storage handle.
    #[must_use]
    pub fn new(storage: StorageReaderHandle) -> Self {
        Self { storage }
    }

    /// Loads one Nod item and verifies its embedded identity.
    pub fn get(&self, nod_id: EntityId36) -> Result<Option<NodItemState>, NodRepositoryError> {
        Ok(self
            .get_with_metadata(nod_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads one Nod item together with optional primary provenance.
    pub fn get_with_metadata(
        &self,
        nod_id: EntityId36,
    ) -> Result<Option<(NodItemState, Option<StorageMetadata>)>, NodRepositoryError> {
        let key = item_key(nod_id)?;
        let Some(record) = self.storage.get_record(namespace(NODS_NAMESPACE)?, &key)? else {
            return Ok(None);
        };
        decode_item(nod_id, record.value.as_bytes()).map(|body| Some((body, record.metadata)))
    }

    /// Batch-loads Nod items and metadata in the same order as the supplied identities.
    pub fn get_many_with_metadata(
        &self,
        nod_ids: &[EntityId36],
    ) -> Result<Vec<Option<NodItemRecordWithMetadata>>, NodRepositoryError> {
        let keys = nod_ids
            .iter()
            .copied()
            .map(item_key)
            .collect::<Result<Vec<_>, _>>()?;
        self.storage
            .get_records(namespace(NODS_NAMESPACE)?, &keys)?
            .into_iter()
            .zip(nod_ids.iter().copied())
            .map(|(record, nod_id)| {
                record
                    .map(|record| {
                        decode_item(nod_id, record.value.as_bytes())
                            .map(|body| (body, record.metadata))
                    })
                    .transpose()
            })
            .collect()
    }

    /// Loads one Nod bucket and verifies its embedded key.
    pub fn get_bucket(
        &self,
        bucket_id: EntityId36,
    ) -> Result<Option<NodBucketState>, NodRepositoryError> {
        Ok(self
            .get_bucket_with_metadata(bucket_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads one Nod bucket together with optional primary provenance.
    pub fn get_bucket_with_metadata(
        &self,
        bucket_id: EntityId36,
    ) -> Result<Option<(NodBucketState, Option<StorageMetadata>)>, NodRepositoryError> {
        let key = bucket_storage_key(bucket_id)?;
        let Some(record) = self
            .storage
            .get_record(namespace(NOD_BUCKETS_NAMESPACE)?, &key)?
        else {
            return Ok(None);
        };
        decode_bucket(bucket_id, record.value.as_bytes()).map(|body| Some((body, record.metadata)))
    }

    /// Batch-loads Nod buckets and metadata in the supplied key order.
    pub fn get_buckets_with_metadata(
        &self,
        bucket_ids: &[EntityId36],
    ) -> Result<Vec<Option<NodBucketRecordWithMetadata>>, NodRepositoryError> {
        let keys = bucket_ids
            .iter()
            .copied()
            .map(bucket_storage_key)
            .collect::<Result<Vec<_>, _>>()?;
        self.storage
            .get_records(namespace(NOD_BUCKETS_NAMESPACE)?, &keys)?
            .into_iter()
            .zip(bucket_ids.iter().copied())
            .map(|(record, bucket_id)| {
                record
                    .map(|record| {
                        decode_bucket(bucket_id, record.value.as_bytes())
                            .map(|body| (body, record.metadata))
                    })
                    .transpose()
            })
            .collect()
    }

    /// Loads an opaque repository-owned snapshot for item/bucket planning and in-block overlay.
    pub fn projection_session(
        &self,
        nod_ids: &[EntityId36],
        bucket_ids: &[EntityId36],
    ) -> Result<crate::projection::NodProjectionSession, NodRepositoryError> {
        let items = self.get_many_with_metadata(nod_ids)?;
        let buckets = self.get_buckets_with_metadata(bucket_ids)?;
        Ok(crate::projection::NodProjectionSession::from_records(
            nod_ids, items, bucket_ids, buckets,
        ))
    }

    /// Lists all Nod items by ascending numeric Nod ID.
    pub fn list_all(&self, request: NodPageRequest) -> Result<NodPage, NodRepositoryError> {
        validate_page_limit(request.limit)?;
        let after = request.after.map(item_key).transpose()?;
        let scan = ScanRequest::new(&[], after.as_ref(), request.limit)?;
        let page = self.storage.scan_prefix(namespace(NODS_NAMESPACE)?, scan)?;
        let has_more = page.next_after.is_some();
        let mut records = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let nod_id = parse_primary_key(entry.key.as_bytes())?;
            records.push(decode_item(nod_id, entry.value.as_bytes())?);
        }
        Ok(NodPage {
            next_after: next_cursor(has_more, &records),
            records,
        })
    }

    /// Lists one owner's Nod items in ascending numeric ID order.
    pub fn list_by_owner(
        &self,
        owner: Address,
        request: NodPageRequest,
    ) -> Result<NodPage, NodRepositoryError> {
        validate_page_limit(request.limit)?;
        let prefix = owner.as_slice();
        let after = request
            .after
            .map(|id| owner_index_key(owner, id))
            .transpose()?;
        let scan = ScanRequest::new(prefix, after.as_ref(), request.limit)?;
        let page = self
            .storage
            .scan_prefix(namespace(NODS_BY_OWNER_NAMESPACE)?, scan)?;
        let has_more = page.next_after.is_some();
        let mut records = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let nod_id = parse_owner_index(&entry, owner)?;
            let body = self
                .get(nod_id)?
                .ok_or(NodRepositoryError::DanglingIndex { nod_id })?;
            if body.owner != owner {
                return Err(NodRepositoryError::IndexedOwnerMismatch { nod_id });
            }
            records.push(body);
        }
        Ok(NodPage {
            next_after: next_cursor(has_more, &records),
            records,
        })
    }
}

/// Cloneable write authority for Nod item/bucket bodies and the owner index.
///
/// Callers must serialize mutations of the same Nod or bucket identity. Each resulting body/index
/// batch is atomic, but the old-body read used to plan replacement or deletion precedes that batch.
pub struct NodRepositoryWriter {
    reader: NodRepositoryReader,
    writer: StorageWriterHandle,
}

impl NodRepositoryWriter {
    /// Creates a writer. Both handles must address the same adapter instance.
    ///
    /// The read handle is required for replacement and deletion.
    #[must_use]
    pub fn new(reader: StorageReaderHandle, writer: StorageWriterHandle) -> Self {
        Self {
            reader: NodRepositoryReader::new(reader),
            writer,
        }
    }

    /// Inserts or replaces one Nod item and its owner index.
    pub fn put_nod(&self, nod: &NodItemState) -> Result<(), NodRepositoryError> {
        let mut session = self.reader.projection_session(&[nod.nod_id], &[])?;
        let batch = session.store_item(nod.nod_id, encode_item(nod)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes a Nod item and its owner index. Missing bodies are a success.
    pub fn delete_nod(&self, nod_id: EntityId36) -> Result<(), NodRepositoryError> {
        let mut session = self.reader.projection_session(&[nod_id], &[])?;
        let batch = session.delete_item(nod_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Inserts or replaces one independently stored Nod bucket.
    pub fn put_bucket(&self, bucket: &NodBucketState) -> Result<(), NodRepositoryError> {
        let bucket_id = canonical_bucket_id(bucket);
        let mut session = self.reader.projection_session(&[], &[bucket_id])?;
        let batch = session.store_bucket(bucket_id, encode_bucket(bucket)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes one Nod bucket. Missing buckets are a success.
    pub fn delete_bucket(&self, bucket_id: EntityId36) -> Result<(), NodRepositoryError> {
        let mut session = self.reader.projection_session(&[], &[bucket_id])?;
        let batch = session.delete_bucket(bucket_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }
}

pub(crate) fn namespace(name: &'static str) -> Result<Namespace, NodRepositoryError> {
    Ok(Namespace::new(name)?)
}

pub(crate) fn encode_item(nod: &NodItemState) -> Result<Value, NodRepositoryError> {
    let payload = encode_nod_item_v1(&canonical_item(nod))?;
    Ok(Value::new(StoredBody::new_v1(payload)?.encode())?)
}

pub(crate) fn decode_item(
    nod_id: EntityId36,
    bytes: &[u8],
) -> Result<NodItemState, NodRepositoryError> {
    let body = from_canonical_item(decode_stored_nod_item_v1(bytes)?);
    if body.nod_id != nod_id {
        return Err(NodRepositoryError::PrimaryKeyBodyMismatch {
            expected: nod_id,
            actual: body.nod_id,
        });
    }
    Ok(body)
}

pub(crate) fn encode_bucket(bucket: &NodBucketState) -> Result<Value, NodRepositoryError> {
    let payload = encode_nod_bucket_v1(&canonical_bucket(bucket))?;
    Ok(Value::new(StoredBody::new_v1(payload)?.encode())?)
}

pub(crate) fn decode_bucket(
    bucket_id: EntityId36,
    bytes: &[u8],
) -> Result<NodBucketState, NodRepositoryError> {
    let body = from_canonical_bucket(decode_stored_nod_bucket_v1(bytes)?);
    let actual = canonical_bucket_id(&body);
    if actual != bucket_id {
        return Err(NodRepositoryError::BucketIdBodyMismatch {
            expected: bucket_id,
            actual,
        });
    }
    Ok(body)
}

pub(crate) fn item_key(nod_id: EntityId36) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(nod_id.as_bytes().to_vec())?)
}

pub(crate) fn bucket_storage_key(bucket_id: EntityId36) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(bucket_id.as_bytes().to_vec())?)
}

pub(crate) fn owner_index_key(
    owner: Address,
    nod_id: EntityId36,
) -> Result<Key, NodRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(nod_id.as_bytes());
    Ok(Key::new(bytes)?)
}

fn parse_primary_key(bytes: &[u8]) -> Result<EntityId36, NodRepositoryError> {
    EntityId36::try_from(bytes).map_err(|_| NodRepositoryError::MalformedPrimaryKey)
}

fn parse_owner_index(entry: &ScanEntry, owner: Address) -> Result<EntityId36, NodRepositoryError> {
    if !entry.value.as_bytes().is_empty() {
        return Err(NodRepositoryError::NonEmptyIndexValue);
    }
    if entry.metadata.is_some() {
        return Err(NodRepositoryError::IndexMetadata);
    }
    let bytes = entry.key.as_bytes();
    if bytes.len() != OWNER_INDEX_KEY_LEN || &bytes[..20] != owner.as_slice() {
        return Err(NodRepositoryError::MalformedIndexKey);
    }
    parse_primary_key(&bytes[20..]).map_err(|_| NodRepositoryError::MalformedIndexKey)
}

fn validate_page_limit(limit: usize) -> Result<(), NodRepositoryError> {
    if !(1..=MAX_SCAN_ENTRIES).contains(&limit) {
        return Err(NodRepositoryError::InvalidPageLimit { limit });
    }
    Ok(())
}

fn next_cursor(has_more: bool, records: &[NodItemState]) -> Option<EntityId36> {
    has_more
        .then(|| records.last().map(|record| record.nod_id))
        .flatten()
}

/// Converts one runtime Nod item into its normative v1 payload model.
pub fn canonical_item(body: &NodItemState) -> NodItemBodyV1 {
    NodItemBodyV1 {
        nod_id: body.nod_id,
        owner: body.owner,
        gratis_load_minor: body.gratis_load_minor,
        worldwide_day: body.worldwide_day,
        league_id: body.league_id,
        floor_price_minor: body.floor_price_minor,
        bucket_key: body.bucket_key,
        cost_amount_minor: body.cost_amount_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
    }
}

/// Converts one runtime Nod bucket into its normative v1 payload model.
pub fn canonical_bucket(body: &NodBucketState) -> NodBucketBodyV1 {
    NodBucketBodyV1 {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        floor_price_minor: body.floor_price_minor,
        is_qualified: body.is_qualified,
        total_nods: body.total_nods,
        entry_price_minor: body.entry_price_minor,
    }
}

/// Reconstructs the canonical bucket identity from the body.
pub fn canonical_bucket_id(body: &NodBucketState) -> EntityId36 {
    canonical_bucket(body).entity_id()
}

/// Converts a validated normative v1 payload into the runtime item type.
pub fn from_canonical_item(body: NodItemBodyV1) -> NodItemState {
    NodItemState {
        nod_id: body.nod_id,
        owner: body.owner,
        gratis_load_minor: body.gratis_load_minor,
        worldwide_day: body.worldwide_day,
        league_id: body.league_id,
        floor_price_minor: body.floor_price_minor,
        bucket_key: body.bucket_key,
        cost_amount_minor: body.cost_amount_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
    }
}

/// Converts a validated normative v1 payload into the runtime bucket type.
pub fn from_canonical_bucket(body: NodBucketBodyV1) -> NodBucketState {
    NodBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        floor_price_minor: body.floor_price_minor,
        is_qualified: body.is_qualified,
        total_nods: body.total_nods,
        entry_price_minor: body.entry_price_minor,
    }
}
