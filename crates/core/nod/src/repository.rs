//! Typed off-chain persistence boundary for Nod item and bucket bodies.

use alloy_primitives::{Address, B256, U256};
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageError, StorageMetadata, StorageReaderHandle,
    StorageWriterHandle, Value, MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::{NodBucketState, NodItemState};

pub(crate) const NODS_NAMESPACE: &str = "nods";
pub(crate) const NOD_BUCKETS_NAMESPACE: &str = "nod_buckets";
pub(crate) const NODS_BY_OWNER_NAMESPACE: &str = "nods_by_owner";
const PRIMARY_KEY_LEN: usize = 32;
const OWNER_INDEX_KEY_LEN: usize = 20 + PRIMARY_KEY_LEN;

/// Domain-level request for one ascending page of Nods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodPageRequest {
    /// Exclusive Nod ID cursor.
    pub after: Option<U256>,
    /// Requested number of records, in `1..=MAX_SCAN_ENTRIES`.
    pub limit: usize,
}

/// One ascending, all-or-error page of Nod item bodies.
pub struct NodPage {
    /// Decoded Nod item bodies.
    pub records: Vec<NodItemState>,
    /// Exclusive cursor for the next page, when more records exist.
    pub next_after: Option<U256>,
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
    /// A Nod item body could not be encoded.
    #[error("failed to encode Nod item body")]
    ItemEncode(#[source] postcard::Error),
    /// A stored Nod item body is not valid postcard data.
    #[error("failed to decode Nod item body")]
    ItemDecode(#[source] postcard::Error),
    /// A Nod bucket body could not be encoded.
    #[error("failed to encode Nod bucket body")]
    BucketEncode(#[source] postcard::Error),
    /// A stored Nod bucket body is not valid postcard data.
    #[error("failed to decode Nod bucket body")]
    BucketDecode(#[source] postcard::Error),
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
    DanglingIndex { nod_id: U256 },
    /// The selecting primary key and embedded body ID disagree.
    #[error("Nod primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch { expected: U256, actual: U256 },
    /// An owner index selected a body owned by someone else.
    #[error("Nod owner index/body mismatch for {nod_id}")]
    IndexedOwnerMismatch { nod_id: U256 },
    /// The selecting bucket key and embedded body key disagree.
    #[error("Nod bucket key/body mismatch: expected {expected}, found {actual}")]
    BucketKeyBodyMismatch { expected: B256, actual: B256 },
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
    pub fn get(&self, nod_id: U256) -> Result<Option<NodItemState>, NodRepositoryError> {
        Ok(self
            .get_with_metadata(nod_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads one Nod item together with optional primary provenance.
    pub fn get_with_metadata(
        &self,
        nod_id: U256,
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
        nod_ids: &[U256],
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
        bucket_key: B256,
    ) -> Result<Option<NodBucketState>, NodRepositoryError> {
        Ok(self
            .get_bucket_with_metadata(bucket_key)?
            .map(|(body, _metadata)| body))
    }

    /// Loads one Nod bucket together with optional primary provenance.
    pub fn get_bucket_with_metadata(
        &self,
        bucket_key: B256,
    ) -> Result<Option<(NodBucketState, Option<StorageMetadata>)>, NodRepositoryError> {
        let key = bucket_storage_key(bucket_key)?;
        let Some(record) = self
            .storage
            .get_record(namespace(NOD_BUCKETS_NAMESPACE)?, &key)?
        else {
            return Ok(None);
        };
        decode_bucket(bucket_key, record.value.as_bytes()).map(|body| Some((body, record.metadata)))
    }

    /// Batch-loads Nod buckets and metadata in the supplied key order.
    pub fn get_buckets_with_metadata(
        &self,
        bucket_keys: &[B256],
    ) -> Result<Vec<Option<NodBucketRecordWithMetadata>>, NodRepositoryError> {
        let keys = bucket_keys
            .iter()
            .copied()
            .map(bucket_storage_key)
            .collect::<Result<Vec<_>, _>>()?;
        self.storage
            .get_records(namespace(NOD_BUCKETS_NAMESPACE)?, &keys)?
            .into_iter()
            .zip(bucket_keys.iter().copied())
            .map(|(record, bucket_key)| {
                record
                    .map(|record| {
                        decode_bucket(bucket_key, record.value.as_bytes())
                            .map(|body| (body, record.metadata))
                    })
                    .transpose()
            })
            .collect()
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
        let old = self.reader.get(nod.nod_id)?;
        let batch = crate::projection::plan_nod_item_mutation(
            old.as_ref(),
            crate::projection::NodItemMutation::Store {
                nod_id: nod.nod_id,
                body: nod,
                metadata: None,
            },
        )?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes a Nod item and its owner index. Missing bodies are a success.
    pub fn delete_nod(&self, nod_id: U256) -> Result<(), NodRepositoryError> {
        let old = self.reader.get(nod_id)?;
        let batch = crate::projection::plan_nod_item_mutation(
            old.as_ref(),
            crate::projection::NodItemMutation::Delete { nod_id },
        )?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Inserts or replaces one independently stored Nod bucket.
    pub fn put_bucket(&self, bucket: &NodBucketState) -> Result<(), NodRepositoryError> {
        let old = self.reader.get_bucket(bucket.bucket_key)?;
        let batch = crate::projection::plan_nod_bucket_mutation(
            old.as_ref(),
            crate::projection::NodBucketMutation::Store {
                bucket_key: bucket.bucket_key,
                body: bucket,
                metadata: None,
            },
        )?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes one Nod bucket. Missing buckets are a success.
    pub fn delete_bucket(&self, bucket_key: B256) -> Result<(), NodRepositoryError> {
        let old = self.reader.get_bucket(bucket_key)?;
        let batch = crate::projection::plan_nod_bucket_mutation(
            old.as_ref(),
            crate::projection::NodBucketMutation::Delete { bucket_key },
        )?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }
}

pub(crate) fn namespace(name: &'static str) -> Result<Namespace, NodRepositoryError> {
    Ok(Namespace::new(name)?)
}

pub(crate) fn encode_item(nod: &NodItemState) -> Result<Value, NodRepositoryError> {
    let bytes = postcard::to_stdvec(nod).map_err(NodRepositoryError::ItemEncode)?;
    Ok(Value::new(bytes)?)
}

fn decode_item(nod_id: U256, bytes: &[u8]) -> Result<NodItemState, NodRepositoryError> {
    let (body, remainder): (NodItemState, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(NodRepositoryError::ItemDecode)?;
    if !remainder.is_empty() {
        return Err(NodRepositoryError::ItemDecode(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    if body.nod_id != nod_id {
        return Err(NodRepositoryError::PrimaryKeyBodyMismatch {
            expected: nod_id,
            actual: body.nod_id,
        });
    }
    Ok(body)
}

pub(crate) fn encode_bucket(bucket: &NodBucketState) -> Result<Value, NodRepositoryError> {
    let bytes = postcard::to_stdvec(bucket).map_err(NodRepositoryError::BucketEncode)?;
    Ok(Value::new(bytes)?)
}

fn decode_bucket(bucket_key: B256, bytes: &[u8]) -> Result<NodBucketState, NodRepositoryError> {
    let (body, remainder): (NodBucketState, &[u8]) =
        postcard::take_from_bytes(bytes).map_err(NodRepositoryError::BucketDecode)?;
    if !remainder.is_empty() {
        return Err(NodRepositoryError::BucketDecode(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    if body.bucket_key != bucket_key {
        return Err(NodRepositoryError::BucketKeyBodyMismatch {
            expected: bucket_key,
            actual: body.bucket_key,
        });
    }
    Ok(body)
}

pub(crate) fn item_key(nod_id: U256) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(nod_id.to_be_bytes::<PRIMARY_KEY_LEN>())?)
}

pub(crate) fn bucket_storage_key(bucket_key: B256) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(bucket_key.as_slice().to_vec())?)
}

pub(crate) fn owner_index_key(owner: Address, nod_id: U256) -> Result<Key, NodRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(&nod_id.to_be_bytes::<PRIMARY_KEY_LEN>());
    Ok(Key::new(bytes)?)
}

fn parse_primary_key(bytes: &[u8]) -> Result<U256, NodRepositoryError> {
    let bytes: [u8; PRIMARY_KEY_LEN] = bytes
        .try_into()
        .map_err(|_| NodRepositoryError::MalformedPrimaryKey)?;
    Ok(U256::from_be_bytes(bytes))
}

fn parse_owner_index(entry: &ScanEntry, owner: Address) -> Result<U256, NodRepositoryError> {
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

fn next_cursor(has_more: bool, records: &[NodItemState]) -> Option<U256> {
    has_more
        .then(|| records.last().map(|record| record.nod_id))
        .flatten()
}
