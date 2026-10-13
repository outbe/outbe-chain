//! Typed off-chain persistence boundary for gem item and bucket bodies.

pub use crate::errors::GemRepositoryError;
use crate::identity::bucket_id;
use crate::schema::{GemBucketState, GemItemState};
use alloy_primitives::Address;
use outbe_compressed_entities::{
    decode_stored_body, decode_stored_gem_bucket_v1, decode_stored_gem_v1, encode_gem_bucket_v1,
    encode_gem_v1, CeAuditError, CeAuditWork, EntityRef, GemBodyV1, GemBucketBodyV1, IdPage,
    IdPageRequest, ParentBodySource, ParentBodySourceError, QueryRef, StoredBody, StoredBodyPage,
    WwdEntityId, BODY_SCHEMA_V1,
};
use outbe_offchain_storage::{
    Key, Namespace, ScanEntry, ScanRequest, StorageMetadata, StorageReaderHandle,
    StorageWriterHandle, StoredValue, Value, MAX_SCAN_ENTRIES,
};

pub(crate) const GEMS_NAMESPACE: &str = "gems";
pub(crate) const GEM_BUCKETS_NAMESPACE: &str = "gem_buckets";
pub(crate) const GEMS_BY_OWNER_NAMESPACE: &str = "gems_by_owner";
const PRIMARY_KEY_LEN: usize = WwdEntityId::len_bytes();
const OWNER_INDEX_KEY_LEN: usize = 20 + PRIMARY_KEY_LEN;

/// Domain-level request for one ascending page of gems.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GemPageRequest {
    /// Exclusive gem id cursor.
    pub after: Option<WwdEntityId>,
    /// Requested number of records, in `1..=MAX_SCAN_ENTRIES`.
    pub limit: usize,
}

/// One ascending, all-or-error page of gem item bodies.
pub struct GemPage {
    /// Decoded gem item bodies.
    pub records: Vec<GemItemState>,
    /// Exclusive cursor for the next page, when more records exist.
    pub next_after: Option<WwdEntityId>,
}

/// One decoded gem item and optional primary storage metadata.
pub type GemItemRecordWithMetadata = (GemItemState, Option<StorageMetadata>);
/// One decoded gem bucket and optional primary storage metadata.
pub type GemBucketRecordWithMetadata = (GemBucketState, Option<StorageMetadata>);

/// Cloneable read authority for gem item and bucket bodies.
#[derive(Clone)]
pub struct GemRepositoryReader {
    storage: StorageReaderHandle,
}

/// Creates a gem read capability over the supplied backend.
#[must_use]
pub fn gem_reader(storage: StorageReaderHandle) -> GemRepositoryReader {
    GemRepositoryReader { storage }
}

impl GemRepositoryReader {
    /// Scans every primary item independently of owner-index membership.
    pub fn scan_stored_items(
        &self,
        request: IdPageRequest,
    ) -> Result<StoredBodyPage, GemRepositoryError> {
        self.scan_stored_primary(request, GEMS_NAMESPACE, decode_stored_item)
    }

    /// Scans the shared bucket primary namespace, including buckets with no items.
    pub fn scan_stored_buckets(
        &self,
        request: IdPageRequest,
    ) -> Result<StoredBodyPage, GemRepositoryError> {
        self.scan_stored_primary(request, GEM_BUCKETS_NAMESPACE, decode_stored_bucket)
    }

    /// Verifies exact item/owner-index membership using bounded pages and scratch.
    pub fn audit_indexes(&self, work: &CeAuditWork) -> Result<(), CeAuditError> {
        let expected = GemAuditEntries::new(&self.storage, GEMS_NAMESPACE)
            .map_err(index_audit_error)?
            .map(primary_owner_audit_record);
        let actual = GemAuditEntries::new(&self.storage, GEMS_BY_OWNER_NAMESPACE)
            .map_err(index_audit_error)?
            .map(indexed_owner_audit_record);
        work.compare_records(PRIMARY_KEY_LEN, expected, actual)
    }

    /// Checks every physical primary against its id-derived shard and the owner index.
    pub fn audit_partition_locations(&self, work: &CeAuditWork) -> Result<(), CeAuditError> {
        for entry in
            GemAuditEntries::new(&self.storage, GEMS_NAMESPACE).map_err(index_audit_error)?
        {
            let entry = entry.map_err(index_audit_error)?;
            let id = parse_primary_key(entry.key.as_bytes()).map_err(index_audit_error)?;
            decode_item(id, entry.value.as_bytes()).map_err(index_audit_error)?;
            let correct = self
                .storage
                .get_record(
                    namespace(GEMS_NAMESPACE)
                        .map_err(index_audit_error)?
                        .with_scope(
                            crate::partitioning::item_scope(id).map_err(index_audit_error)?,
                        ),
                    &entry.key,
                )
                .map_err(index_audit_error)?;
            if correct.as_ref().is_none_or(|record| {
                record.value != entry.value || record.metadata != entry.metadata
            }) {
                return Err(CeAuditError::Invalid(
                    "gem primary is outside its id partition".into(),
                ));
            }
        }
        self.audit_indexes(work)
    }

    /// Loads one gem item and verifies its embedded identity.
    pub fn get_item(
        &self,
        gem_id: WwdEntityId,
    ) -> Result<Option<GemItemState>, GemRepositoryError> {
        Ok(self
            .get_with_metadata(gem_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads the exact canonical item StoredBody used by the execution parent seam.
    pub fn get_stored_item(
        &self,
        gem_id: WwdEntityId,
    ) -> Result<Option<StoredBody>, GemRepositoryError> {
        self.primary_record(GEMS_NAMESPACE, item_key(gem_id)?)?
            .map(|record| decode_stored_item(gem_id, record.value.as_bytes()))
            .transpose()
    }

    /// Loads one gem item together with optional primary provenance.
    pub fn get_with_metadata(
        &self,
        gem_id: WwdEntityId,
    ) -> Result<Option<GemItemRecordWithMetadata>, GemRepositoryError> {
        self.primary_record(GEMS_NAMESPACE, item_key(gem_id)?)?
            .map(|record| decode_record(gem_id, record, decode_item))
            .transpose()
    }

    /// Batch-loads gem items and metadata in the same order as the supplied identities.
    pub fn get_many_with_metadata(
        &self,
        gem_ids: &[WwdEntityId],
    ) -> Result<Vec<Option<GemItemRecordWithMetadata>>, GemRepositoryError> {
        outbe_offchain_storage::repository_pages::read_records(
            &self.storage,
            namespace(GEMS_NAMESPACE)?,
            gem_ids,
            item_key,
            |id, record| decode_record(id, record, decode_item),
        )
    }

    /// Loads one gem bucket and verifies its embedded key.
    pub fn get_bucket(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<GemBucketState>, GemRepositoryError> {
        Ok(self
            .get_bucket_with_metadata(bucket_id)?
            .map(|(body, _metadata)| body))
    }

    /// Loads the exact canonical bucket StoredBody used by the execution parent seam.
    pub fn get_stored_bucket(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<StoredBody>, GemRepositoryError> {
        self.primary_record(GEM_BUCKETS_NAMESPACE, bucket_storage_key(bucket_id)?)?
            .map(|record| decode_stored_bucket(bucket_id, record.value.as_bytes()))
            .transpose()
    }

    /// Loads one gem bucket together with optional primary provenance.
    pub fn get_bucket_with_metadata(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<GemBucketRecordWithMetadata>, GemRepositoryError> {
        self.primary_record(GEM_BUCKETS_NAMESPACE, bucket_storage_key(bucket_id)?)?
            .map(|record| decode_record(bucket_id, record, decode_bucket))
            .transpose()
    }

    /// Batch-loads gem buckets and metadata in the supplied key order.
    pub fn get_buckets_with_metadata(
        &self,
        bucket_ids: &[WwdEntityId],
    ) -> Result<Vec<Option<GemBucketRecordWithMetadata>>, GemRepositoryError> {
        outbe_offchain_storage::repository_pages::read_records(
            &self.storage,
            namespace(GEM_BUCKETS_NAMESPACE)?,
            bucket_ids,
            bucket_storage_key,
            |id, record| decode_record(id, record, decode_bucket),
        )
    }

    /// Loads an opaque repository-owned snapshot for item/bucket planning and in-block overlay.
    pub fn projection_session(
        &self,
        gem_ids: &[WwdEntityId],
        bucket_ids: &[WwdEntityId],
    ) -> Result<crate::projection::GemProjectionSession, GemRepositoryError> {
        let items = self.get_many_with_metadata(gem_ids)?;
        let buckets = self.get_buckets_with_metadata(bucket_ids)?;
        Ok(crate::projection::session_from_records(
            gem_ids, items, bucket_ids, buckets,
        ))
    }

    /// Lists one owner's gems in ascending numeric id order.
    pub fn list_by_owner(
        &self,
        owner: Address,
        request: GemPageRequest,
    ) -> Result<GemPage, GemRepositoryError> {
        validate_page_limit(request.limit)?;
        let after = request
            .after
            .map(|id| owner_index_key(owner, id))
            .transpose()?;
        let page = self.scan(
            GEMS_BY_OWNER_NAMESPACE,
            owner.as_slice(),
            after.as_ref(),
            request.limit,
        )?;
        let has_more = page.next_after.is_some();
        let mut records = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let gem_id = parse_owner_index(&entry, owner)?;
            let body = self
                .get_item(gem_id)?
                .ok_or(GemRepositoryError::DanglingIndex { gem_id })?;
            if body.owner != owner {
                return Err(GemRepositoryError::IndexedOwnerMismatch { gem_id });
            }
            records.push(body);
        }
        Ok(GemPage {
            next_after: next_cursor(has_more, &records),
            records,
        })
    }

    /// Lists only one owner's canonical gem identities for overlay merging.
    pub fn list_ids_by_owner(
        &self,
        owner: Address,
        request: IdPageRequest,
    ) -> Result<IdPage, GemRepositoryError> {
        let limit = validate_id_page_request(request)?;
        let after = request
            .after
            .map(|id| owner_index_key(owner, id))
            .transpose()?;
        let page = self.scan(
            GEMS_BY_OWNER_NAMESPACE,
            owner.as_slice(),
            after.as_ref(),
            limit,
        )?;
        id_page_from_entries(page, request.after, "owner", |entry| {
            parse_owner_index(entry, owner)
        })
    }

    fn primary_record(
        &self,
        name: &'static str,
        key: Key,
    ) -> Result<Option<StoredValue>, GemRepositoryError> {
        Ok(self.storage.get_record(namespace(name)?, &key)?)
    }

    fn scan(
        &self,
        name: &'static str,
        prefix: &[u8],
        after: Option<&Key>,
        limit: usize,
    ) -> Result<outbe_offchain_storage::ScanPage, GemRepositoryError> {
        let request = ScanRequest::new(prefix, after, limit)?;
        Ok(self.storage.scan_prefix(namespace(name)?, request)?)
    }
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
) -> Result<(), GemRepositoryError> {
    page_rules(index).validate(page, after, limit)
}

/// The iterator retains at most one native bounded page.
struct GemAuditEntries<'a> {
    storage: &'a StorageReaderHandle,
    namespace: Namespace,
    name: &'static str,
    after: Option<Key>,
    entries: std::vec::IntoIter<ScanEntry>,
    done: bool,
}

impl<'a> GemAuditEntries<'a> {
    fn new(
        storage: &'a StorageReaderHandle,
        name: &'static str,
    ) -> Result<Self, GemRepositoryError> {
        Ok(Self {
            storage,
            namespace: namespace(name)?,
            name,
            after: None,
            entries: Vec::new().into_iter(),
            done: false,
        })
    }

    fn read_page(&mut self) -> Result<(), GemRepositoryError> {
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

    fn next_page_entry(&mut self) -> Option<Result<ScanEntry, GemRepositoryError>> {
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

impl Iterator for GemAuditEntries<'_> {
    type Item = Result<ScanEntry, GemRepositoryError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.entries.next() {
            Some(entry) => Some(Ok(entry)),
            None => self.next_page_entry(),
        }
    }
}

impl ParentBodySource for GemRepositoryReader {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        match entity {
            EntityRef::Gem(gem_id) => self.get_stored_item(gem_id),
            EntityRef::GemBucket(bucket_id) => self.get_stored_bucket(bucket_id),
            EntityRef::Tribute(_) | EntityRef::NodItem(_) | EntityRef::NodBucket(_) => {
                return Err(ParentBodySourceError::Corruption(
                    "gem repository cannot serve a non-gem entity".into(),
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
            QueryRef::GemByOwner(owner) => self.list_ids_by_owner(owner, request),
            QueryRef::TributeByOwner(_)
            | QueryRef::TributeByDay(_)
            | QueryRef::NodByOwner(_)
            | QueryRef::NodAll => {
                return Err(ParentBodySourceError::Corruption(
                    "gem repository cannot serve a non-gem query".into(),
                ));
            }
        }
        .map_err(map_parent_source_error)
    }
}

/// Cloneable write authority for gem item/bucket bodies and the owner index.
///
/// Callers must serialize mutations of the same gem or bucket identity. Each resulting body/index
/// batch is atomic, but the old-body read used to plan replacement or deletion precedes that batch.
pub struct GemRepositoryWriter {
    reader: GemRepositoryReader,
    writer: StorageWriterHandle,
}

/// Creates a writer. Both handles must address the same adapter instance.
#[must_use]
pub fn gem_writer(reader: StorageReaderHandle, writer: StorageWriterHandle) -> GemRepositoryWriter {
    GemRepositoryWriter {
        reader: gem_reader(reader),
        writer,
    }
}

impl GemRepositoryWriter {
    /// Inserts or replaces one gem item and its owner index.
    pub fn put_item(&self, item: &GemItemState) -> Result<(), GemRepositoryError> {
        let mut session = self.reader.projection_session(&[item.gem_id], &[])?;
        let batch = session.store_item(item.gem_id, encode_item(item)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes a gem item and its owner index. Missing bodies are a success.
    pub fn delete_item(&self, gem_id: WwdEntityId) -> Result<(), GemRepositoryError> {
        let mut session = self.reader.projection_session(&[gem_id], &[])?;
        let batch = session.delete_item(gem_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Inserts or replaces one independently stored gem bucket.
    pub fn put_bucket(&self, bucket: &GemBucketState) -> Result<(), GemRepositoryError> {
        let bucket_id = canonical_bucket_id(bucket);
        let mut session = self.reader.projection_session(&[], &[bucket_id])?;
        let batch = session.store_bucket(bucket_id, encode_bucket(bucket)?, None)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }

    /// Deletes one gem bucket. Missing buckets are a success.
    pub fn delete_bucket(&self, bucket_id: WwdEntityId) -> Result<(), GemRepositoryError> {
        let mut session = self.reader.projection_session(&[], &[bucket_id])?;
        let batch = session.delete_bucket(bucket_id)?;
        self.writer.apply_atomic(&batch)?;
        Ok(())
    }
}

pub(crate) fn namespace(name: &'static str) -> Result<Namespace, GemRepositoryError> {
    Ok(Namespace::new(name)?)
}

pub(crate) fn encode_item(item: &GemItemState) -> Result<Value, GemRepositoryError> {
    let payload = encode_gem_v1(&canonical_item(item))?;
    Ok(Value::new(
        StoredBody::new(BODY_SCHEMA_V1, payload)?.encode(),
    )?)
}

pub(crate) fn decode_item(
    gem_id: WwdEntityId,
    bytes: &[u8],
) -> Result<GemItemState, GemRepositoryError> {
    let body = from_canonical_item(decode_stored_gem_v1(bytes)?)?;
    if body.gem_id != gem_id {
        return Err(GemRepositoryError::PrimaryKeyBodyMismatch {
            expected: gem_id,
            actual: body.gem_id,
        });
    }
    Ok(body)
}

fn decode_stored_item(gem_id: WwdEntityId, bytes: &[u8]) -> Result<StoredBody, GemRepositoryError> {
    let stored = decode_stored_body(bytes)?;
    let body = decode_stored_gem_v1(bytes)?;
    if body.gem_id != gem_id {
        return Err(GemRepositoryError::PrimaryKeyBodyMismatch {
            expected: gem_id,
            actual: body.gem_id,
        });
    }
    Ok(stored)
}

pub(crate) fn encode_bucket(bucket: &GemBucketState) -> Result<Value, GemRepositoryError> {
    let payload = encode_gem_bucket_v1(&canonical_bucket(bucket))?;
    Ok(Value::new(
        StoredBody::new(BODY_SCHEMA_V1, payload)?.encode(),
    )?)
}

pub(crate) fn decode_bucket(
    bucket_id: WwdEntityId,
    bytes: &[u8],
) -> Result<GemBucketState, GemRepositoryError> {
    let body = from_canonical_bucket(decode_stored_gem_bucket_v1(bytes)?)?;
    let actual = canonical_bucket_id(&body);
    if actual != bucket_id {
        return Err(GemRepositoryError::BucketIdBodyMismatch {
            expected: bucket_id,
            actual,
        });
    }
    Ok(body)
}

fn decode_stored_bucket(
    bucket_id: WwdEntityId,
    bytes: &[u8],
) -> Result<StoredBody, GemRepositoryError> {
    let stored = decode_stored_body(bytes)?;
    let body = decode_stored_gem_bucket_v1(bytes)?;
    let actual = body.entity_id();
    if actual != bucket_id {
        return Err(GemRepositoryError::BucketIdBodyMismatch {
            expected: bucket_id,
            actual,
        });
    }
    Ok(stored)
}

type BodyDecoder<T> = fn(WwdEntityId, &[u8]) -> Result<T, GemRepositoryError>;
type RecordWithMetadata<T> = (T, Option<StorageMetadata>);

fn decode_record<T>(
    id: WwdEntityId,
    record: StoredValue,
    decode: BodyDecoder<T>,
) -> Result<RecordWithMetadata<T>, GemRepositoryError> {
    let body = decode(id, record.value.as_bytes())?;
    Ok((body, record.metadata))
}

pub(crate) fn item_key(gem_id: WwdEntityId) -> Result<Key, GemRepositoryError> {
    Ok(Key::new(gem_id.as_slice().to_vec())?)
}

pub(crate) fn bucket_storage_key(bucket_id: WwdEntityId) -> Result<Key, GemRepositoryError> {
    Ok(Key::new(bucket_id.as_slice().to_vec())?)
}

pub(crate) fn owner_index_key(
    owner: Address,
    gem_id: WwdEntityId,
) -> Result<Key, GemRepositoryError> {
    let mut bytes = Vec::with_capacity(OWNER_INDEX_KEY_LEN);
    bytes.extend_from_slice(owner.as_slice());
    bytes.extend_from_slice(gem_id.as_slice());
    Ok(Key::new(bytes)?)
}

fn parse_primary_key(bytes: &[u8]) -> Result<WwdEntityId, GemRepositoryError> {
    WwdEntityId::try_from(bytes).map_err(|_| GemRepositoryError::MalformedPrimaryKey)
}

fn parse_owner_index(entry: &ScanEntry, owner: Address) -> Result<WwdEntityId, GemRepositoryError> {
    if !entry.value.as_bytes().is_empty() {
        return Err(GemRepositoryError::NonEmptyIndexValue);
    }
    if entry.metadata.is_some() {
        return Err(GemRepositoryError::IndexMetadata);
    }
    let bytes = entry.key.as_bytes();
    if bytes.len() != OWNER_INDEX_KEY_LEN || &bytes[..20] != owner.as_slice() {
        return Err(GemRepositoryError::MalformedIndexKey);
    }
    parse_primary_key(&bytes[20..]).map_err(|_| GemRepositoryError::MalformedIndexKey)
}

fn validate_page_limit(limit: usize) -> Result<(), GemRepositoryError> {
    if !(1..=MAX_SCAN_ENTRIES).contains(&limit) {
        return Err(GemRepositoryError::InvalidPageLimit {
            limit,
            max: MAX_SCAN_ENTRIES,
        });
    }
    Ok(())
}

fn map_parent_source_error(error: GemRepositoryError) -> ParentBodySourceError {
    use outbe_offchain_storage::StorageErrorKind;

    let message = error.to_string();
    let kind = match &error {
        GemRepositoryError::Storage(storage) => Some(storage.kind()),
        _ => None,
    };
    match kind {
        Some(StorageErrorKind::RequestDeadline) => ParentBodySourceError::RequestDeadline(message),
        Some(StorageErrorKind::Unavailable) => ParentBodySourceError::Unavailable(message),
        _ => ParentBodySourceError::Corruption(message),
    }
}

fn validate_id_page_request(request: IdPageRequest) -> Result<usize, GemRepositoryError> {
    let limit =
        usize::try_from(request.limit).map_err(|_| GemRepositoryError::InvalidPageLimit {
            limit: usize::MAX,
            max: MAX_SCAN_ENTRIES,
        })?;
    validate_page_limit(limit)?;
    Ok(limit)
}

fn id_page_from_entries(
    page: outbe_offchain_storage::ScanPage,
    after: Option<WwdEntityId>,
    index: &'static str,
    parse: impl FnMut(&ScanEntry) -> Result<WwdEntityId, GemRepositoryError>,
) -> Result<IdPage, GemRepositoryError> {
    let (ids, next_after) = page_rules(index).id_page(page, after, parse)?;
    Ok(IdPage { ids, next_after })
}

fn next_cursor(has_more: bool, records: &[GemItemState]) -> Option<WwdEntityId> {
    has_more
        .then(|| records.last().map(|record| record.gem_id))
        .flatten()
}

fn narrow_u8<T>(value: T, field: &'static str) -> Result<u8, GemRepositoryError>
where
    T: TryInto<u8>,
{
    value
        .try_into()
        .map_err(|_| GemRepositoryError::CanonicalFieldOutOfRange { field })
}

/// Converts one runtime gem item into its normative v1 payload model.
pub fn canonical_item(body: &GemItemState) -> GemBodyV1 {
    GemBodyV1 {
        gem_id: body.gem_id,
        owner: body.owner,
        gem_type: u32::from(body.gem_type),
        promis_load_minor: body.promis_load_minor,
        floor_price_minor: body.floor_price_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
        bucket_key: body.bucket_key,
        state: u32::from(body.state),
        called_at: body.called_at,
        settled_at: body.settled_at,
    }
}

/// Converts one runtime gem bucket into its normative v1 payload model.
pub fn canonical_bucket(body: &GemBucketState) -> GemBucketBodyV1 {
    GemBucketBodyV1 {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
        settled_gems: body.settled_gems,
    }
}

/// Reconstructs the canonical bucket identity from the body.
#[must_use]
pub fn canonical_bucket_id(body: &GemBucketState) -> WwdEntityId {
    bucket_id(body.worldwide_day, body.bucket_key)
}

/// Converts a validated normative v1 payload into the runtime item type.
pub fn from_canonical_item(body: GemBodyV1) -> Result<GemItemState, GemRepositoryError> {
    Ok(GemItemState {
        gem_id: body.gem_id,
        owner: body.owner,
        gem_type: narrow_u8(body.gem_type, "gem_type")?,
        promis_load_minor: body.promis_load_minor,
        floor_price_minor: body.floor_price_minor,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        issued_at: body.issued_at,
        bucket_key: body.bucket_key,
        state: narrow_u8(body.state, "state")?,
        called_at: body.called_at,
        settled_at: body.settled_at,
    })
}

/// Converts a validated normative v1 payload into the runtime bucket type.
pub fn from_canonical_bucket(body: GemBucketBodyV1) -> Result<GemBucketState, GemRepositoryError> {
    Ok(GemBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
        settled_gems: body.settled_gems,
    })
}

fn page_rules(
    index: &'static str,
) -> outbe_offchain_storage::repository_pages::PageRules<GemRepositoryError> {
    outbe_offchain_storage::repository_page_rules!(GemRepositoryError, index, "gem")
}

fn primary_owner_audit_record(
    entry: Result<ScanEntry, GemRepositoryError>,
) -> Result<[u8; OWNER_INDEX_KEY_LEN], CeAuditError> {
    let entry = entry.map_err(index_audit_error)?;
    let id = parse_primary_key(entry.key.as_bytes()).map_err(index_audit_error)?;
    let body = decode_item(id, entry.value.as_bytes()).map_err(index_audit_error)?;
    Ok(owner_audit_record(id, body.owner))
}

fn indexed_owner_audit_record(
    entry: Result<ScanEntry, GemRepositoryError>,
) -> Result<[u8; OWNER_INDEX_KEY_LEN], CeAuditError> {
    let entry = entry.map_err(index_audit_error)?;
    if entry.key.as_bytes().len() != OWNER_INDEX_KEY_LEN {
        return Err(index_audit_error(GemRepositoryError::MalformedIndexKey));
    }
    let owner = Address::from_slice(&entry.key.as_bytes()[..20]);
    let id = parse_owner_index(&entry, owner).map_err(index_audit_error)?;
    Ok(owner_audit_record(id, owner))
}

outbe_offchain_storage::repository_primary_scan!(
    GemRepositoryReader,
    GemRepositoryError,
    WwdEntityId,
    IdPageRequest,
    StoredBody,
    StoredBodyPage,
    crate::repository
);
