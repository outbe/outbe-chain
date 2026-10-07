//! Typed off-chain persistence boundary for Nod item and bucket bodies.

mod day_store;
mod partition_audit;

use std::sync::Arc;

use alloy_primitives::Address;
use outbe_compressed_entities::{
    decode_stored_nod_bucket_v1, decode_stored_nod_item_v1, encode_nod_bucket_v1,
    encode_nod_item_v1, CanonicalBodyError, CeAuditError, CeAuditWork, EntityRef, IdPage,
    IdPageRequest, NodBucketBodyV1, NodItemBodyV1, ParentBodySource, ParentBodySourceError,
    QueryRef, StoredBody, StoredBodyPage, WwdEntityId,
};
use outbe_offchain_storage::{
    AtomicWriteOperation, DayDatabases, Key, Namespace, ScanEntry, ScanRequest, StorageError,
    StorageMetadata, StorageReaderHandle, StorageWriterHandle, StoredValue, Value,
    MAX_SCAN_ENTRIES,
};
use thiserror::Error;

use crate::{NodBucketState, NodItemState};

pub(crate) const NODS_NAMESPACE: &str = "nods";
pub(crate) const NOD_BUCKETS_NAMESPACE: &str = "nod_buckets";
pub(crate) const NODS_BY_OWNER_NAMESPACE: &str = "nods_by_owner";
/// Shared index of worldwide days on which an owner still has a Nod.
pub(crate) const NOD_OWNER_DAYS_NAMESPACE: &str = "nod_owner_days";
const PRIMARY_KEY_LEN: usize = WwdEntityId::len_bytes();
const OWNER_INDEX_KEY_LEN: usize = 20 + PRIMARY_KEY_LEN;
const OWNER_DAY_KEY_LEN: usize = 24;

/// Domain-level request for one ascending page of Nods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodPageRequest {
    /// Exclusive Nod ID cursor.
    pub after: Option<WwdEntityId>,
    /// Requested number of records, in `1..=MAX_SCAN_ENTRIES`.
    pub limit: usize,
}

/// One ascending, all-or-error page of Nod item bodies.
pub struct NodPage {
    /// Decoded Nod item bodies.
    pub records: Vec<NodItemState>,
    /// Exclusive cursor for the next page, when more records exist.
    pub next_after: Option<WwdEntityId>,
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
    DanglingIndex { nod_id: WwdEntityId },
    /// The selecting primary key and embedded body ID disagree.
    #[error("Nod primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch {
        expected: WwdEntityId,
        actual: WwdEntityId,
    },
    /// An owner index selected a body owned by someone else.
    #[error("Nod owner index/body mismatch for {nod_id}")]
    IndexedOwnerMismatch { nod_id: WwdEntityId },
    /// An ID-only repository page is not strictly ascending after its cursor.
    #[error("Nod {index} ID page is not strictly ascending")]
    NonAscendingIdPage { index: &'static str },
    /// The storage adapter returned a continuation that is not the last page key.
    #[error("Nod {index} ID page has an invalid continuation")]
    InvalidPageContinuation { index: &'static str },
    /// The selecting bucket key and embedded body key disagree.
    #[error("Nod bucket ID/body mismatch: expected {expected}, found {actual}")]
    BucketIdBodyMismatch {
        expected: WwdEntityId,
        actual: WwdEntityId,
    },
    /// A projection session may mutate only identities loaded into its repository snapshot.
    #[error("{entity} projection identity {identity} was not loaded")]
    UntrackedProjectionIdentity {
        entity: &'static str,
        identity: WwdEntityId,
    },
}

/// Cloneable read authority for Nod item and bucket bodies.
#[derive(Clone)]
pub struct NodRepositoryReader {
    storage: StorageReaderHandle,
    route: Option<day_store::DayRoute>,
}

impl NodRepositoryReader {
    /// Creates a typed Nod reader over a backend-neutral storage handle.
    #[must_use]
    pub fn new(storage: StorageReaderHandle) -> Self {
        Self {
            storage,
            route: None,
        }
    }

    /// Reads and writes through one database per worldwide day.
    ///
    /// `shared` keeps the migration cursor. New bodies are stored in the day database.
    #[must_use]
    pub fn with_days(
        shared: StorageReaderHandle,
        shared_writer: StorageWriterHandle,
        databases: Arc<DayDatabases>,
    ) -> Self {
        Self {
            storage: shared,
            route: Some(day_store::DayRoute {
                shared_writer,
                databases,
                wrap: None,
            }),
        }
    }

    /// Reads each day database through `wrap` (execution budgets, diagnostics).
    #[must_use]
    pub fn with_day_read_wrap(
        mut self,
        wrap: Arc<dyn Fn(StorageReaderHandle) -> StorageReaderHandle + Send + Sync>,
    ) -> Self {
        if let Some(route) = &mut self.route {
            route.wrap = Some(wrap);
        }
        self
    }

    /// Scans every primary item independently of owner-index membership.
    pub fn scan_stored_items(
        &self,
        request: IdPageRequest,
    ) -> Result<StoredBodyPage, NodRepositoryError> {
        self.scan_stored_primary(request, NODS_NAMESPACE, decode_stored_item)
    }

    /// Scans the separate bucket primary namespace, including buckets with no items.
    pub fn scan_stored_buckets(
        &self,
        request: IdPageRequest,
    ) -> Result<StoredBodyPage, NodRepositoryError> {
        self.scan_stored_primary(request, NOD_BUCKETS_NAMESPACE, decode_stored_bucket)
    }

    fn scan_stored_primary(
        &self,
        request: IdPageRequest,
        name: &'static str,
        decode: fn(WwdEntityId, &[u8]) -> Result<StoredBody, NodRepositoryError>,
    ) -> Result<StoredBodyPage, NodRepositoryError> {
        let limit = validate_id_page_request(request)?;
        let after = request.after.map(item_key).transpose()?;
        let page = self.storage.scan_prefix(
            namespace(name)?,
            ScanRequest::new(&[], after.as_ref(), limit)?,
        )?;
        validate_audit_page(&page, after.as_ref(), limit, name)?;
        let next_after = page
            .next_after
            .as_ref()
            .map(|key| parse_primary_key(key.as_bytes()))
            .transpose()?;
        let mut entries = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let id = parse_primary_key(entry.key.as_bytes())?;
            entries.push((id, decode(id, entry.value.as_bytes())?));
        }
        Ok(StoredBodyPage {
            entries,
            next_after,
        })
    }

    /// Verifies exact item/owner-index membership using bounded pages and scratch.
    /// The caller supplies a stable read-only storage view for the entire audit.
    pub fn audit_indexes(&self, work: &CeAuditWork) -> Result<(), CeAuditError> {
        let expected = NodAuditEntries::new(&self.storage, NODS_NAMESPACE)
            .map_err(index_audit_error)?
            .map(|entry| {
                let entry = entry.map_err(index_audit_error)?;
                let id = parse_primary_key(entry.key.as_bytes()).map_err(index_audit_error)?;
                let body = decode_item(id, entry.value.as_bytes()).map_err(index_audit_error)?;
                Ok(owner_audit_record(id, body.owner))
            });
        let actual = NodAuditEntries::new(&self.storage, NODS_BY_OWNER_NAMESPACE)
            .map_err(index_audit_error)?
            .map(|entry| {
                let entry = entry.map_err(index_audit_error)?;
                if entry.key.as_bytes().len() != OWNER_INDEX_KEY_LEN {
                    return Err(index_audit_error(NodRepositoryError::MalformedIndexKey));
                }
                let owner = Address::from_slice(&entry.key.as_bytes()[..20]);
                let id = parse_owner_index(&entry, owner).map_err(index_audit_error)?;
                Ok(owner_audit_record(id, owner))
            });
        work.compare_records(PRIMARY_KEY_LEN, expected, actual)
    }

    /// Loads one Nod item and verifies its embedded identity.
    pub fn get(&self, nod_id: WwdEntityId) -> Result<Option<NodItemState>, NodRepositoryError> {
        Ok(self
            .get_with_metadata(nod_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads the exact canonical item StoredBody used by the execution parent seam.
    pub fn get_stored_item(
        &self,
        nod_id: WwdEntityId,
    ) -> Result<Option<StoredBody>, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::get_stored_item(self, nod_id);
        }
        self.primary_record(NODS_NAMESPACE, item_key(nod_id)?)?
            .map(|record| decode_stored_item(nod_id, record.value.as_bytes()))
            .transpose()
    }

    /// Loads one Nod item together with optional primary provenance.
    pub fn get_with_metadata(
        &self,
        nod_id: WwdEntityId,
    ) -> Result<Option<(NodItemState, Option<StorageMetadata>)>, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::get_with_metadata(self, nod_id);
        }
        self.primary_record(NODS_NAMESPACE, item_key(nod_id)?)?
            .map(|record| decode_record(nod_id, record, decode_item))
            .transpose()
    }

    /// Batch-loads Nod items and metadata in the same order as the supplied identities.
    pub fn get_many_with_metadata(
        &self,
        nod_ids: &[WwdEntityId],
    ) -> Result<Vec<Option<NodItemRecordWithMetadata>>, NodRepositoryError> {
        self.primary_records(NODS_NAMESPACE, nod_ids, item_key, decode_item)
    }

    /// Loads one Nod bucket and verifies its embedded key.
    pub fn get_bucket(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<NodBucketState>, NodRepositoryError> {
        Ok(self
            .get_bucket_with_metadata(bucket_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads the exact canonical bucket StoredBody used by the execution parent seam.
    pub fn get_stored_bucket(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<StoredBody>, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::get_stored_bucket(self, bucket_id);
        }
        self.primary_record(NOD_BUCKETS_NAMESPACE, bucket_storage_key(bucket_id)?)?
            .map(|record| decode_stored_bucket(bucket_id, record.value.as_bytes()))
            .transpose()
    }

    /// Loads one Nod bucket together with optional primary provenance.
    pub fn get_bucket_with_metadata(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<(NodBucketState, Option<StorageMetadata>)>, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::get_bucket_with_metadata(self, bucket_id);
        }
        self.primary_record(NOD_BUCKETS_NAMESPACE, bucket_storage_key(bucket_id)?)?
            .map(|record| decode_record(bucket_id, record, decode_bucket))
            .transpose()
    }

    /// Batch-loads Nod buckets and metadata in the supplied key order.
    pub fn get_buckets_with_metadata(
        &self,
        bucket_ids: &[WwdEntityId],
    ) -> Result<Vec<Option<NodBucketRecordWithMetadata>>, NodRepositoryError> {
        self.primary_records(
            NOD_BUCKETS_NAMESPACE,
            bucket_ids,
            bucket_storage_key,
            decode_bucket,
        )
    }

    fn primary_record(
        &self,
        name: &'static str,
        key: Key,
    ) -> Result<Option<StoredValue>, NodRepositoryError> {
        Ok(self.storage.get_record(namespace(name)?, &key)?)
    }

    /// Batch-loads primary records in `ids` order and decodes each one present.
    fn primary_records<T>(
        &self,
        name: &'static str,
        ids: &[WwdEntityId],
        key: fn(WwdEntityId) -> Result<Key, NodRepositoryError>,
        decode: BodyDecoder<T>,
    ) -> Result<Vec<Option<RecordWithMetadata<T>>>, NodRepositoryError> {
        let keys = ids
            .iter()
            .copied()
            .map(key)
            .collect::<Result<Vec<_>, _>>()?;
        let records = self.storage.get_records(namespace(name)?, &keys)?;
        records
            .into_iter()
            .zip(ids.iter().copied())
            .map(|(record, id)| {
                record
                    .map(|record| decode_record(id, record, decode))
                    .transpose()
            })
            .collect()
    }

    fn scan(
        &self,
        name: &'static str,
        prefix: &[u8],
        after: Option<&Key>,
        limit: usize,
    ) -> Result<outbe_offchain_storage::ScanPage, NodRepositoryError> {
        let request = ScanRequest::new(prefix, after, limit)?;
        Ok(self.storage.scan_prefix(namespace(name)?, request)?)
    }

    /// Loads an opaque repository-owned snapshot for item/bucket planning and in-block overlay.
    pub fn projection_session(
        &self,
        nod_ids: &[WwdEntityId],
        bucket_ids: &[WwdEntityId],
    ) -> Result<crate::projection::NodProjectionSession, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::projection_session(self, nod_ids, bucket_ids);
        }
        let items = self.get_many_with_metadata(nod_ids)?;
        let buckets = self.get_buckets_with_metadata(bucket_ids)?;
        Ok(crate::projection::NodProjectionSession::from_records(
            nod_ids, items, bucket_ids, buckets,
        ))
    }

    /// Lists only canonical Nod item identities for overlay merging.
    pub fn list_ids_all(&self, request: IdPageRequest) -> Result<IdPage, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::list_ids_all(self, request);
        }
        let limit = validate_id_page_request(request)?;
        let after = request.after.map(item_key).transpose()?;
        let page = self.scan(NODS_NAMESPACE, &[], after.as_ref(), limit)?;
        id_page_from_entries(page, request.after, "all", |entry| {
            parse_primary_key(entry.key.as_bytes())
        })
    }

    /// Lists one owner's Nod items in ascending numeric ID order.
    ///
    /// With day databases, the shared owner-day index selects which days to open.
    pub fn list_by_owner(
        &self,
        owner: Address,
        request: NodPageRequest,
    ) -> Result<NodPage, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::list_by_owner(self, owner, request);
        }
        validate_page_limit(request.limit)?;
        let after = request
            .after
            .map(|id| owner_index_key(owner, id))
            .transpose()?;
        let page = self.scan(
            NODS_BY_OWNER_NAMESPACE,
            owner.as_slice(),
            after.as_ref(),
            request.limit,
        )?;
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

    /// Lists only one owner's canonical Nod item identities for overlay merging.
    pub fn list_ids_by_owner(
        &self,
        owner: Address,
        request: IdPageRequest,
    ) -> Result<IdPage, NodRepositoryError> {
        if self.route.is_some() {
            return day_store::list_ids_by_owner(self, owner, request);
        }
        let limit = validate_id_page_request(request)?;
        let after = request
            .after
            .map(|id| owner_index_key(owner, id))
            .transpose()?;
        let page = self.scan(
            NODS_BY_OWNER_NAMESPACE,
            owner.as_slice(),
            after.as_ref(),
            limit,
        )?;
        id_page_from_entries(page, request.after, "owner", |entry| {
            parse_owner_index(entry, owner)
        })
    }

    /// Shared put or delete for whether `owner` still has a Nod in this day database.
    ///
    /// The reader is the day database. The operation is applied to the shared database.
    pub fn owner_day_marker(
        &self,
        owner: Address,
        day: u32,
    ) -> Result<AtomicWriteOperation, NodRepositoryError> {
        let present = !self
            .list_ids_by_owner(
                owner,
                IdPageRequest {
                    after: None,
                    limit: 1,
                },
            )?
            .ids
            .is_empty();
        let key = owner_day_key(owner, day)?;
        let namespace = namespace(NOD_OWNER_DAYS_NAMESPACE)?;
        Ok(if present {
            AtomicWriteOperation::put(namespace, key, Value::new(Vec::new())?)
        } else {
            AtomicWriteOperation::delete(namespace, key)
        })
    }
}

/// Deletes one owner-day index entry. Used when that day's database is already gone.
pub fn clear_owner_day(
    owner: Address,
    day: u32,
) -> Result<AtomicWriteOperation, NodRepositoryError> {
    Ok(AtomicWriteOperation::delete(
        namespace(NOD_OWNER_DAYS_NAMESPACE)?,
        owner_day_key(owner, day)?,
    ))
}

fn owner_audit_record(id: WwdEntityId, owner: Address) -> [u8; OWNER_INDEX_KEY_LEN] {
    let mut record = [0; OWNER_INDEX_KEY_LEN];
    record[..PRIMARY_KEY_LEN].copy_from_slice(id.as_slice());
    record[PRIMARY_KEY_LEN..].copy_from_slice(owner.as_slice());
    record
}

fn index_audit_error(error: impl std::fmt::Display) -> CeAuditError {
    CeAuditError::Invalid(error.to_string())
}

fn validate_audit_page(
    page: &outbe_offchain_storage::ScanPage,
    after: Option<&Key>,
    limit: usize,
    index: &'static str,
) -> Result<(), NodRepositoryError> {
    if page.entries.len() > limit {
        return Err(
            StorageError::Corruption(format!("Nod {index} page exceeds requested limit")).into(),
        );
    }
    check_continuation(page, index)?;
    let mut previous = after;
    for entry in &page.entries {
        if previous.is_some_and(|previous| previous.as_bytes() >= entry.key.as_bytes()) {
            return Err(NodRepositoryError::NonAscendingIdPage { index });
        }
        previous = Some(&entry.key);
    }
    Ok(())
}

/// The iterator retains at most one native bounded page. Only an absent continuation
/// ends scanning. Adapters may return a short page because of byte limits.
struct NodAuditEntries<'a> {
    storage: &'a StorageReaderHandle,
    namespace: Namespace,
    name: &'static str,
    after: Option<Key>,
    entries: std::vec::IntoIter<ScanEntry>,
    done: bool,
}

impl<'a> NodAuditEntries<'a> {
    fn new(
        storage: &'a StorageReaderHandle,
        name: &'static str,
    ) -> Result<Self, NodRepositoryError> {
        Ok(Self {
            storage,
            namespace: namespace(name)?,
            name,
            after: None,
            entries: Vec::new().into_iter(),
            done: false,
        })
    }

    fn read_page(&mut self) -> Result<(), NodRepositoryError> {
        let page = self.storage.scan_prefix(
            self.namespace.clone(),
            ScanRequest::new(&[], self.after.as_ref(), MAX_SCAN_ENTRIES)?,
        )?;
        validate_audit_page(&page, self.after.as_ref(), MAX_SCAN_ENTRIES, self.name)?;
        self.done = page.next_after.is_none();
        self.after = page.next_after;
        self.entries = page.entries.into_iter();
        Ok(())
    }

    /// Reads the next page and yields its first entry. A read error ends the iteration.
    fn next_page_entry(&mut self) -> Option<Result<ScanEntry, NodRepositoryError>> {
        if self.done {
            return None;
        }
        match self.read_page() {
            Ok(()) => self.entries.next().map(Ok),
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

impl Iterator for NodAuditEntries<'_> {
    type Item = Result<ScanEntry, NodRepositoryError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.entries.next() {
            Some(entry) => Some(Ok(entry)),
            None => self.next_page_entry(),
        }
    }
}

impl ParentBodySource for NodRepositoryReader {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        match entity {
            EntityRef::NodItem(nod_id) => self.get_stored_item(nod_id),
            EntityRef::NodBucket(bucket_id) => self.get_stored_bucket(bucket_id),
            EntityRef::Tribute(_) => {
                return Err(ParentBodySourceError::Corruption(
                    "Nod repository cannot serve a Tribute entity".into(),
                ));
            }
        }
        .map_err(map_parent_source_error)
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> Result<IdPage, ParentBodySourceError> {
        match query {
            QueryRef::NodByOwner(owner) => self.list_ids_by_owner(owner, request),
            QueryRef::NodAll => self.list_ids_all(request),
            QueryRef::TributeByOwner(_) | QueryRef::TributeByDay(_) => {
                return Err(ParentBodySourceError::Corruption(
                    "Nod repository cannot serve a Tribute query".into(),
                ));
            }
        }
        .map_err(map_parent_source_error)
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
    /// Replacement and deletion require the read handle.
    #[must_use]
    pub fn new(reader: StorageReaderHandle, writer: StorageWriterHandle) -> Self {
        Self {
            reader: NodRepositoryReader::new(reader),
            writer,
        }
    }

    /// Writes each Nod into the database of its worldwide day.
    #[must_use]
    pub fn with_days(
        shared_reader: StorageReaderHandle,
        shared_writer: StorageWriterHandle,
        databases: Arc<DayDatabases>,
    ) -> Self {
        Self {
            reader: NodRepositoryReader::with_days(shared_reader, shared_writer.clone(), databases),
            writer: shared_writer,
        }
    }

    /// Inserts or replaces one Nod item and its owner index.
    pub fn put_nod(&self, nod: &NodItemState) -> Result<(), NodRepositoryError> {
        if self.reader.route.is_some() {
            return day_store::put_nod(self, nod);
        }
        let mut session = self.reader.projection_session(&[nod.nod_id], &[])?;
        let batch = session.store_item(nod.nod_id, encode_item(nod)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes a Nod item and its owner index. Missing bodies are a success.
    pub fn delete_nod(&self, nod_id: WwdEntityId) -> Result<(), NodRepositoryError> {
        if self.reader.route.is_some() {
            return day_store::delete_nod(self, nod_id);
        }
        let mut session = self.reader.projection_session(&[nod_id], &[])?;
        let batch = session.delete_item(nod_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Inserts or replaces one independently stored Nod bucket.
    pub fn put_bucket(&self, bucket: &NodBucketState) -> Result<(), NodRepositoryError> {
        if self.reader.route.is_some() {
            return day_store::put_bucket(self, bucket);
        }
        let bucket_id = canonical_bucket_id(bucket);
        let mut session = self.reader.projection_session(&[], &[bucket_id])?;
        let batch = session.store_bucket(bucket_id, encode_bucket(bucket)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes one Nod bucket. Missing buckets are a success.
    pub fn delete_bucket(&self, bucket_id: WwdEntityId) -> Result<(), NodRepositoryError> {
        if self.reader.route.is_some() {
            return day_store::delete_bucket(self, bucket_id);
        }
        let mut session = self.reader.projection_session(&[], &[bucket_id])?;
        let batch = session.delete_bucket(bucket_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Moves Nod keys already stored in the shared database into their day databases.
    pub fn migrate_legacy_keys(&self) -> Result<(), NodRepositoryError> {
        day_store::migrate(&self.reader)
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
    nod_id: WwdEntityId,
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

fn decode_stored_item(nod_id: WwdEntityId, bytes: &[u8]) -> Result<StoredBody, NodRepositoryError> {
    let stored = StoredBody::decode(bytes)?;
    let body = decode_stored_nod_item_v1(bytes)?;
    if body.nod_id != nod_id {
        return Err(NodRepositoryError::PrimaryKeyBodyMismatch {
            expected: nod_id,
            actual: body.nod_id,
        });
    }
    Ok(stored)
}

pub(crate) fn encode_bucket(bucket: &NodBucketState) -> Result<Value, NodRepositoryError> {
    let payload = encode_nod_bucket_v1(&canonical_bucket(bucket))?;
    Ok(Value::new(StoredBody::new_v1(payload)?.encode())?)
}

pub(crate) fn decode_bucket(
    bucket_id: WwdEntityId,
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

fn decode_stored_bucket(
    bucket_id: WwdEntityId,
    bytes: &[u8],
) -> Result<StoredBody, NodRepositoryError> {
    let stored = StoredBody::decode(bytes)?;
    let body = decode_stored_nod_bucket_v1(bytes)?;
    let actual = body.entity_id();
    if actual != bucket_id {
        return Err(NodRepositoryError::BucketIdBodyMismatch {
            expected: bucket_id,
            actual,
        });
    }
    Ok(stored)
}

type BodyDecoder<T> = fn(WwdEntityId, &[u8]) -> Result<T, NodRepositoryError>;
type RecordWithMetadata<T> = (T, Option<StorageMetadata>);

/// Decodes one primary record and keeps its metadata.
fn decode_record<T>(
    id: WwdEntityId,
    record: StoredValue,
    decode: BodyDecoder<T>,
) -> Result<RecordWithMetadata<T>, NodRepositoryError> {
    let body = decode(id, record.value.as_bytes())?;
    Ok((body, record.metadata))
}

pub(crate) fn item_key(nod_id: WwdEntityId) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(nod_id.as_slice().to_vec())?)
}

pub(crate) fn bucket_storage_key(bucket_id: WwdEntityId) -> Result<Key, NodRepositoryError> {
    Ok(Key::new(bucket_id.as_slice().to_vec())?)
}

pub(crate) fn owner_index_key(
    owner: Address,
    nod_id: WwdEntityId,
) -> Result<Key, NodRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(nod_id.as_slice());
    Ok(Key::new(bytes)?)
}

pub(crate) fn owner_day_key(owner: Address, day: u32) -> Result<Key, NodRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_DAY_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(&day.to_be_bytes());
    Ok(Key::new(bytes)?)
}

fn parse_primary_key(bytes: &[u8]) -> Result<WwdEntityId, NodRepositoryError> {
    WwdEntityId::try_from(bytes).map_err(|_| NodRepositoryError::MalformedPrimaryKey)
}

fn parse_owner_index(entry: &ScanEntry, owner: Address) -> Result<WwdEntityId, NodRepositoryError> {
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

fn map_parent_source_error(error: NodRepositoryError) -> ParentBodySourceError {
    use outbe_offchain_storage::StorageErrorKind;

    let message = error.to_string();
    let kind = match &error {
        NodRepositoryError::Storage(storage) => Some(storage.kind()),
        _ => None,
    };
    match kind {
        Some(StorageErrorKind::RequestDeadline) => ParentBodySourceError::RequestDeadline(message),
        Some(StorageErrorKind::Unavailable) => ParentBodySourceError::Unavailable(message),
        _ => ParentBodySourceError::Corruption(message),
    }
}

fn validate_id_page_request(request: IdPageRequest) -> Result<usize, NodRepositoryError> {
    let limit = usize::try_from(request.limit)
        .map_err(|_| NodRepositoryError::InvalidPageLimit { limit: usize::MAX })?;
    validate_page_limit(limit)?;
    Ok(limit)
}

fn id_page_from_entries(
    page: outbe_offchain_storage::ScanPage,
    after: Option<WwdEntityId>,
    index: &'static str,
    parse: impl FnMut(&ScanEntry) -> Result<WwdEntityId, NodRepositoryError>,
) -> Result<IdPage, NodRepositoryError> {
    check_continuation(&page, index)?;
    let ids = ascending_ids(&page.entries, after, index, parse)?;
    let next_after = page
        .next_after
        .is_some()
        .then(|| {
            ids.last()
                .copied()
                .ok_or(NodRepositoryError::InvalidPageContinuation { index })
        })
        .transpose()?;
    Ok(IdPage { ids, next_after })
}

/// Rejects a continuation that is not the key of the page's last entry.
fn check_continuation(
    page: &outbe_offchain_storage::ScanPage,
    index: &'static str,
) -> Result<(), NodRepositoryError> {
    if page
        .next_after
        .as_ref()
        .is_some_and(|next| page.entries.last().map(|entry| &entry.key) != Some(next))
    {
        return Err(NodRepositoryError::InvalidPageContinuation { index });
    }
    Ok(())
}

fn ascending_ids(
    entries: &[ScanEntry],
    after: Option<WwdEntityId>,
    index: &'static str,
    mut parse: impl FnMut(&ScanEntry) -> Result<WwdEntityId, NodRepositoryError>,
) -> Result<Vec<WwdEntityId>, NodRepositoryError> {
    let mut ids = Vec::with_capacity(entries.len());
    let mut previous = after;
    for entry in entries {
        let id = parse(entry)?;
        if previous.is_some_and(|previous| id <= previous) {
            return Err(NodRepositoryError::NonAscendingIdPage { index });
        }
        ids.push(id);
        previous = Some(id);
    }
    Ok(ids)
}

fn next_cursor(has_more: bool, records: &[NodItemState]) -> Option<WwdEntityId> {
    has_more
        .then(|| records.last().map(|record| record.nod_id))
        .flatten()
}

/// Converts one runtime Nod item into its normative v1 payload model.
pub fn canonical_item(body: &NodItemState) -> NodItemBodyV1 {
    NodItemBodyV1 {
        is_settled: body.is_settled,
        nod_id: body.nod_id,
        owner: body.owner,
        gratis_load_minor: body.gratis_load_minor,
        worldwide_day: body.worldwide_day,
        league_id: body.league_id,
        bucket_key: body.bucket_key,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
    }
}

/// Converts one runtime Nod bucket into its normative v1 payload model.
pub fn canonical_bucket(body: &NodBucketState) -> NodBucketBodyV1 {
    NodBucketBodyV1 {
        settled_nods: body.settled_nods,
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
    }
}

/// Reconstructs the canonical bucket identity from the body.
pub fn canonical_bucket_id(body: &NodBucketState) -> WwdEntityId {
    canonical_bucket(body).entity_id()
}

/// Converts a validated normative v1 payload into the runtime item type.
pub fn from_canonical_item(body: NodItemBodyV1) -> NodItemState {
    NodItemState {
        is_settled: body.is_settled,
        nod_id: body.nod_id,
        owner: body.owner,
        gratis_load_minor: body.gratis_load_minor,
        worldwide_day: body.worldwide_day,
        league_id: body.league_id,
        bucket_key: body.bucket_key,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
    }
}

/// Converts a validated normative v1 payload into the runtime bucket type.
pub fn from_canonical_bucket(body: NodBucketBodyV1) -> NodBucketState {
    NodBucketState {
        settled_nods: body.settled_nods,
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
    }
}
