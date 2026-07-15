//! Typed off-chain persistence boundary for Nod item and bucket bodies.

use alloy_primitives::{Address, B256, U256};
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageError, StorageReaderHandle, StorageWriterHandle,
    Value, MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::{NodBucketState, NodItemState};

const NODS_NAMESPACE: &str = "nods";
const NOD_BUCKETS_NAMESPACE: &str = "nod_buckets";
const NODS_BY_OWNER_NAMESPACE: &str = "nods_by_owner";
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
        let key = item_key(nod_id)?;
        let Some(value) = self.storage.get(namespace(NODS_NAMESPACE)?, &key)? else {
            return Ok(None);
        };
        decode_item(nod_id, value.as_bytes()).map(Some)
    }

    /// Loads one Nod bucket and verifies its embedded key.
    pub fn get_bucket(
        &self,
        bucket_key: B256,
    ) -> Result<Option<NodBucketState>, NodRepositoryError> {
        let key = bucket_storage_key(bucket_key)?;
        let Some(value) = self.storage.get(namespace(NOD_BUCKETS_NAMESPACE)?, &key)? else {
            return Ok(None);
        };
        decode_bucket(bucket_key, value.as_bytes()).map(Some)
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
pub struct NodRepositoryWriter {
    reader: NodRepositoryReader,
    writer: StorageWriterHandle,
}

impl NodRepositoryWriter {
    /// Creates a writer. Both handles must address the same adapter instance.
    ///
    /// The read handle is required for replacement and deletion. Until the
    /// transactional projection introduced by ADR-004, callers must also
    /// serialize concurrent mutations of the same Nod ID.
    #[must_use]
    pub fn new(reader: StorageReaderHandle, writer: StorageWriterHandle) -> Self {
        Self {
            reader: NodRepositoryReader::new(reader),
            writer,
        }
    }

    /// Inserts or replaces one Nod item and its owner index.
    ///
    /// Multi-key writes are intentionally non-atomic: the first storage failure
    /// is returned and already-completed steps are not rolled back.
    pub fn put_nod(&self, nod: &NodItemState) -> Result<(), NodRepositoryError> {
        let old = self.reader.get(nod.nod_id)?;
        let primary = item_key(nod.nod_id)?;
        let encoded = encode_item(nod)?;
        let owner_index = owner_index_key(nod.owner, nod.nod_id)?;
        let empty = Value::new(Vec::new())?;

        self.writer
            .put(namespace(NODS_NAMESPACE)?, &primary, &encoded)?;
        self.writer
            .put(namespace(NODS_BY_OWNER_NAMESPACE)?, &owner_index, &empty)?;
        if let Some(old) = old {
            if old.owner != nod.owner {
                self.writer.delete(
                    namespace(NODS_BY_OWNER_NAMESPACE)?,
                    &owner_index_key(old.owner, old.nod_id)?,
                )?;
            }
        }
        Ok(())
    }

    /// Deletes a Nod item and its owner index. Missing bodies are a success.
    pub fn delete_nod(&self, nod_id: U256) -> Result<(), NodRepositoryError> {
        let Some(old) = self.reader.get(nod_id)? else {
            return Ok(());
        };
        self.writer.delete(
            namespace(NODS_BY_OWNER_NAMESPACE)?,
            &owner_index_key(old.owner, nod_id)?,
        )?;
        self.writer
            .delete(namespace(NODS_NAMESPACE)?, &item_key(nod_id)?)?;
        Ok(())
    }

    /// Inserts or replaces one independently stored Nod bucket.
    pub fn put_bucket(&self, bucket: &NodBucketState) -> Result<(), NodRepositoryError> {
        self.writer.put(
            namespace(NOD_BUCKETS_NAMESPACE)?,
            &bucket_storage_key(bucket.bucket_key)?,
            &encode_bucket(bucket)?,
        )?;
        Ok(())
    }

    /// Deletes one Nod bucket. Missing buckets are a success.
    pub fn delete_bucket(&self, bucket_key: B256) -> Result<(), NodRepositoryError> {
        self.writer.delete(
            namespace(NOD_BUCKETS_NAMESPACE)?,
            &bucket_storage_key(bucket_key)?,
        )?;
        Ok(())
    }
}

fn namespace(name: &'static str) -> Result<Namespace, NodRepositoryError> {
    Ok(Namespace::new(name)?)
}

fn encode_item(nod: &NodItemState) -> Result<Value, NodRepositoryError> {
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

fn encode_bucket(bucket: &NodBucketState) -> Result<Value, NodRepositoryError> {
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

fn item_key(nod_id: U256) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(nod_id.to_be_bytes::<PRIMARY_KEY_LEN>())?)
}

fn bucket_storage_key(bucket_key: B256) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(bucket_key.as_slice().to_vec())?)
}

fn owner_index_key(owner: Address, nod_id: U256) -> Result<Key, NodRepositoryError> {
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
