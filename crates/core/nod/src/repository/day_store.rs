//! Nod items and buckets live in the database of their worldwide day.

use std::sync::Arc;

use alloy_primitives::Address;
use outbe_compressed_entities::{IdPage, IdPageRequest, WwdEntityId};
use outbe_offchain_storage::{
    AtomicWriteBatch, DayDatabases, Key, RocksDbStorage, ScanRequest, StorageReaderHandle,
    StorageWriterHandle, StoredValue, Value, MAX_SCAN_ENTRIES,
};

use super::{
    namespace, NodPage, NodPageRequest, NodRepositoryError, NodRepositoryReader,
    NodRepositoryWriter, NODS_NAMESPACE, NOD_BUCKETS_NAMESPACE, NOD_OWNER_DAYS_NAMESPACE,
};

const MIGRATION_NAMESPACE: &str = "nod_day_migration";
const CURSOR_NODS: &[u8] = b"nods";
const CURSOR_BUCKETS: &[u8] = b"nod_buckets";
const MIGRATION_DONE: &[u8] = b"done";
const MIGRATION_PAGE: usize = 128;

pub(super) type DayReadWrap = Arc<dyn Fn(StorageReaderHandle) -> StorageReaderHandle + Send + Sync>;

#[derive(Clone)]
pub(super) struct DayRoute {
    pub(super) shared_writer: StorageWriterHandle,
    pub(super) databases: Arc<DayDatabases>,
    pub(super) wrap: Option<DayReadWrap>,
}

pub(super) fn migrate(reader: &NodRepositoryReader) -> Result<(), NodRepositoryError> {
    let Some(route) = &reader.route else {
        return Ok(());
    };
    migrate_namespace(reader, route, NODS_NAMESPACE, CURSOR_NODS, true)?;
    migrate_namespace(reader, route, NOD_BUCKETS_NAMESPACE, CURSOR_BUCKETS, false)?;
    Ok(())
}

fn migrate_namespace(
    reader: &NodRepositoryReader,
    route: &DayRoute,
    namespace_name: &'static str,
    cursor_bytes: &[u8],
    items: bool,
) -> Result<(), NodRepositoryError> {
    let migration = namespace(MIGRATION_NAMESPACE)?;
    let cursor_key = Key::new(cursor_bytes.to_vec())?;
    if reader
        .storage
        .get(migration.clone(), &cursor_key)?
        .as_ref()
        .is_some_and(|value| value.as_bytes() == MIGRATION_DONE)
    {
        return Ok(());
    }
    let primary = namespace(namespace_name)?;
    let mut after = reader
        .storage
        .get(migration.clone(), &cursor_key)?
        .map(|value| Key::new(value.as_bytes().to_vec()))
        .transpose()?;
    loop {
        let page = reader.storage.scan_prefix(
            primary.clone(),
            ScanRequest::new(&[], after.as_ref(), MIGRATION_PAGE)?,
        )?;
        let Some(last) = page.entries.last().map(|entry| entry.key.clone()) else {
            route.shared_writer.put(
                migration,
                &cursor_key,
                &Value::new(MIGRATION_DONE.to_vec())?,
            )?;
            return Ok(());
        };
        for entry in &page.entries {
            let id = WwdEntityId::try_from(entry.key.as_bytes())
                .map_err(|_| NodRepositoryError::MalformedPrimaryKey)?;
            let Some(record) = reader.storage.get_record(primary.clone(), &entry.key)? else {
                continue;
            };
            copy_record(reader, route, id, record, items)?;
        }
        route.shared_writer.put(
            migration.clone(),
            &cursor_key,
            &Value::new(last.as_bytes().to_vec())?,
        )?;
        after = Some(last);
    }
}

fn copy_record(
    reader: &NodRepositoryReader,
    route: &DayRoute,
    id: WwdEntityId,
    record: StoredValue,
    items: bool,
) -> Result<(), NodRepositoryError> {
    let day_number = id.worldwide_day().value();
    let day = route.databases.nod(day_number)?;
    let handle: StorageWriterHandle = day.clone();
    let day_reader: StorageReaderHandle = day.clone();
    let source = NodRepositoryReader::new(day_reader);
    let shared = NodRepositoryWriter::new(reader.storage.clone(), route.shared_writer.clone());
    if items {
        let owner = super::decode_item(id, record.value.as_bytes())?.owner;
        let mut session = source.projection_session(&[id], &[])?;
        handle.apply_atomic(&session.store_item(id, record.value, record.metadata)?)?;
        shared.delete_nod(id)?;
        write_owner_day(route, day, owner, day_number)?;
    } else {
        let mut session = source.projection_session(&[], &[id])?;
        handle.apply_atomic(&session.store_bucket(id, record.value, record.metadata)?)?;
        shared.delete_bucket(id)?;
    }
    Ok(())
}

pub(super) fn get_with_metadata(
    reader: &NodRepositoryReader,
    nod_id: WwdEntityId,
) -> Result<Option<super::NodItemRecordWithMetadata>, NodRepositoryError> {
    routed_day(reader, nod_id)?.map_or(Ok(None), |day| day.get_with_metadata(nod_id))
}

pub(super) fn get_stored_item(
    reader: &NodRepositoryReader,
    nod_id: WwdEntityId,
) -> Result<Option<outbe_compressed_entities::StoredBody>, NodRepositoryError> {
    routed_day(reader, nod_id)?.map_or(Ok(None), |day| day.get_stored_item(nod_id))
}

pub(super) fn get_bucket_with_metadata(
    reader: &NodRepositoryReader,
    bucket_id: WwdEntityId,
) -> Result<Option<super::NodBucketRecordWithMetadata>, NodRepositoryError> {
    routed_day(reader, bucket_id)?.map_or(Ok(None), |day| day.get_bucket_with_metadata(bucket_id))
}

pub(super) fn get_stored_bucket(
    reader: &NodRepositoryReader,
    bucket_id: WwdEntityId,
) -> Result<Option<outbe_compressed_entities::StoredBody>, NodRepositoryError> {
    routed_day(reader, bucket_id)?.map_or(Ok(None), |day| day.get_stored_bucket(bucket_id))
}

pub(super) fn projection_session(
    reader: &NodRepositoryReader,
    nod_ids: &[WwdEntityId],
    bucket_ids: &[WwdEntityId],
) -> Result<crate::projection::NodProjectionSession, NodRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let items = nod_ids
        .iter()
        .map(|nod_id| match open_day(route, *nod_id)? {
            Some(day) => day.get_with_metadata(*nod_id),
            None => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let buckets = bucket_ids
        .iter()
        .map(|bucket_id| match open_day(route, *bucket_id)? {
            Some(day) => day.get_bucket_with_metadata(*bucket_id),
            None => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(crate::projection::NodProjectionSession::from_records(
        nod_ids, items, bucket_ids, buckets,
    ))
}

pub(super) fn list_ids_all(
    reader: &NodRepositoryReader,
    request: IdPageRequest,
) -> Result<IdPage, NodRepositoryError> {
    migrate(reader)?;
    let limit = super::validate_id_page_request(request)?;
    let route = route(reader)?;
    let days = route.databases.directory().list_nod_days()?;
    let walk = DayWalk {
        route,
        days: &days,
        after: request.after,
    };
    walk.ids(limit, |day_reader, request| {
        day_reader.list_ids_all(request)
    })
}

pub(super) fn list_ids_by_owner(
    reader: &NodRepositoryReader,
    owner: Address,
    request: IdPageRequest,
) -> Result<IdPage, NodRepositoryError> {
    migrate(reader)?;
    let limit = super::validate_id_page_request(request)?;
    let route = route(reader)?;
    let days = owner_days(reader, owner)?;
    let walk = DayWalk {
        route,
        days: &days,
        after: request.after,
    };
    walk.ids(limit, |day_reader, request| {
        day_reader.list_ids_by_owner(owner, request)
    })
}

pub(super) fn list_by_owner(
    reader: &NodRepositoryReader,
    owner: Address,
    request: NodPageRequest,
) -> Result<NodPage, NodRepositoryError> {
    migrate(reader)?;
    super::validate_page_limit(request.limit)?;
    let route = route(reader)?;
    let days = owner_days(reader, owner)?;
    let walk = DayWalk {
        route,
        days: &days,
        after: request.after,
    };
    let (records, more) = walk.collect(request.limit, |day_reader, after, limit| {
        let page = day_reader.list_by_owner(owner, NodPageRequest { after, limit })?;
        Ok((page.records, page.next_after))
    })?;
    Ok(NodPage {
        next_after: more
            .then(|| records.last().map(|record| record.nod_id))
            .flatten(),
        records,
    })
}

pub(super) fn put_nod(
    writer: &NodRepositoryWriter,
    nod: &crate::NodItemState,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let route = route(&writer.reader)?;
    let day_number = nod.worldwide_day.value();
    let day = route.databases.nod(day_number)?;
    let old_owner = NodRepositoryReader::new(day.clone())
        .get(nod.nod_id)?
        .map(|body| body.owner);
    day_writer(day.clone()).put_nod(nod)?;
    write_owner_day(route, day.clone(), nod.owner, day_number)?;
    if let Some(old_owner) = old_owner.filter(|old_owner| *old_owner != nod.owner) {
        write_owner_day(route, day, old_owner, day_number)?;
    }
    Ok(())
}

pub(super) fn delete_nod(
    writer: &NodRepositoryWriter,
    nod_id: WwdEntityId,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let route = route(&writer.reader)?;
    let day_number = nod_id.worldwide_day().value();
    let Some(day) = route.databases.nod_if_present(day_number)? else {
        return Ok(());
    };
    let owner = NodRepositoryReader::new(day.clone())
        .get(nod_id)?
        .map(|body| body.owner);
    day_writer(day.clone()).delete_nod(nod_id)?;
    if let Some(owner) = owner {
        write_owner_day(route, day, owner, day_number)?;
    }
    Ok(())
}

pub(super) fn put_bucket(
    writer: &NodRepositoryWriter,
    bucket: &crate::NodBucketState,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let day = route(&writer.reader)?
        .databases
        .nod(bucket.worldwide_day.value())?;
    day_writer(day).put_bucket(bucket)
}

pub(super) fn delete_bucket(
    writer: &NodRepositoryWriter,
    bucket_id: WwdEntityId,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let route = route(&writer.reader)?;
    match route
        .databases
        .nod_if_present(bucket_id.worldwide_day().value())?
    {
        Some(day) => day_writer(day).delete_bucket(bucket_id),
        None => Ok(()),
    }
}

fn route(reader: &NodRepositoryReader) -> Result<&DayRoute, NodRepositoryError> {
    reader.route.as_ref().ok_or_else(|| {
        NodRepositoryError::Storage(outbe_offchain_storage::StorageError::InvalidArgument(
            "Nod day route is missing".into(),
        ))
    })
}

fn owner_days(
    reader: &NodRepositoryReader,
    owner: Address,
) -> Result<Vec<u32>, NodRepositoryError> {
    let namespace = namespace(NOD_OWNER_DAYS_NAMESPACE)?;
    let mut days = Vec::new();
    let mut after = None;
    loop {
        let page = reader.storage.scan_prefix(
            namespace.clone(),
            ScanRequest::new(owner.as_slice(), after.as_ref(), MAX_SCAN_ENTRIES)?,
        )?;
        for entry in &page.entries {
            let key = entry.key.as_bytes();
            if key.len() != super::OWNER_DAY_KEY_LEN || &key[..20] != owner.as_slice() {
                return Err(NodRepositoryError::MalformedIndexKey);
            }
            if !entry.value.as_bytes().is_empty() {
                return Err(NodRepositoryError::NonEmptyIndexValue);
            }
            if entry.metadata.is_some() {
                return Err(NodRepositoryError::IndexMetadata);
            }
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&key[20..24]);
            days.push(u32::from_be_bytes(bytes));
        }
        match page.next_after {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(days)
}

fn write_owner_day(
    route: &DayRoute,
    day: Arc<RocksDbStorage>,
    owner: Address,
    day_number: u32,
) -> Result<(), NodRepositoryError> {
    let handle: StorageReaderHandle = day;
    let operation = NodRepositoryReader::new(handle).owner_day_marker(owner, day_number)?;
    route
        .shared_writer
        .apply_atomic(&AtomicWriteBatch::from_operations(vec![operation]))?;
    Ok(())
}

fn open_day_reader(route: &DayRoute, storage: Arc<RocksDbStorage>) -> NodRepositoryReader {
    let handle: StorageReaderHandle = storage;
    let handle = match &route.wrap {
        Some(wrap) => wrap(handle),
        None => handle,
    };
    NodRepositoryReader::new(handle)
}

/// Reads the day database of `id` through the route, or `None` when that day has none.
fn routed_day(
    reader: &NodRepositoryReader,
    id: WwdEntityId,
) -> Result<Option<NodRepositoryReader>, NodRepositoryError> {
    migrate(reader)?;
    open_day(route(reader)?, id)
}

fn open_day(
    route: &DayRoute,
    id: WwdEntityId,
) -> Result<Option<NodRepositoryReader>, NodRepositoryError> {
    open_day_number(route, id.worldwide_day().value())
}

fn open_day_number(
    route: &DayRoute,
    day: u32,
) -> Result<Option<NodRepositoryReader>, NodRepositoryError> {
    Ok(route
        .databases
        .nod_if_present(day)?
        .map(|storage| open_day_reader(route, storage)))
}

fn day_writer(day: Arc<RocksDbStorage>) -> NodRepositoryWriter {
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    NodRepositoryWriter::new(reader, writer)
}

/// Entries of one day page and the cursor of that day's next page.
type DayPage<T> = (Vec<T>, Option<WwdEntityId>);

/// Ascending pages across day databases, from the day of the `after` cursor.
struct DayWalk<'a> {
    route: &'a DayRoute,
    days: &'a [u32],
    after: Option<WwdEntityId>,
}

impl DayWalk<'_> {
    fn ids(
        &self,
        limit: usize,
        mut read_page: impl FnMut(
            &NodRepositoryReader,
            IdPageRequest,
        ) -> Result<IdPage, NodRepositoryError>,
    ) -> Result<IdPage, NodRepositoryError> {
        let (ids, more) = self.collect(limit, |day_reader, after, limit| {
            let limit = u32::try_from(limit).unwrap_or(u32::MAX);
            let page = read_page(day_reader, IdPageRequest { after, limit })?;
            Ok((page.ids, page.next_after))
        })?;
        Ok(IdPage {
            next_after: more.then(|| ids.last().copied()).flatten(),
            ids,
        })
    }

    /// Collects up to `limit` entries. Also returns whether more entries follow them.
    fn collect<T>(
        &self,
        limit: usize,
        mut read_page: impl FnMut(
            &NodRepositoryReader,
            Option<WwdEntityId>,
            usize,
        ) -> Result<DayPage<T>, NodRepositoryError>,
    ) -> Result<(Vec<T>, bool), NodRepositoryError> {
        let start = self.after.map(|id| id.worldwide_day().value()).unwrap_or(0);
        let mut entries = Vec::new();
        let mut remaining = limit;
        for day in self.days.iter().copied().filter(|day| *day >= start) {
            let Some(day_reader) = open_day_number(self.route, day)? else {
                continue;
            };
            let mut after = (day == start).then_some(self.after).flatten();
            loop {
                let (page, next_after) = read_page(&day_reader, after, remaining)?;
                remaining = remaining.saturating_sub(page.len());
                let page_was_empty = page.is_empty();
                entries.extend(page);
                if remaining == 0 {
                    let more = next_after.is_some() || self.later(day, &mut read_page)?;
                    return Ok((entries, more));
                }
                if page_was_empty || next_after.is_none() {
                    break;
                }
                after = next_after;
            }
        }
        Ok((entries, false))
    }

    /// Whether a day after `current` holds an entry.
    fn later<T>(
        &self,
        current: u32,
        read_page: &mut impl FnMut(
            &NodRepositoryReader,
            Option<WwdEntityId>,
            usize,
        ) -> Result<DayPage<T>, NodRepositoryError>,
    ) -> Result<bool, NodRepositoryError> {
        for day in self.days.iter().copied().filter(|day| *day > current) {
            let Some(day_reader) = open_day_number(self.route, day)? else {
                continue;
            };
            if !read_page(&day_reader, None, 1)?.0.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
