//! Nod items and buckets live in the database of their worldwide day.

use std::sync::Arc;

use alloy_primitives::Address;
use outbe_compressed_entities::{IdPage, IdPageRequest, WwdEntityId};
use outbe_offchain_storage::{
    DayDatabases, Key, RocksDbStorage, ScanRequest, StorageReaderHandle, StorageWriterHandle,
    StoredValue, Value,
};

use super::{
    namespace, NodRepositoryError, NodRepositoryReader, NodRepositoryWriter, NODS_NAMESPACE,
    NOD_BUCKETS_NAMESPACE,
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
    let day = route.databases.nod(id.worldwide_day().value())?;
    let handle: StorageWriterHandle = day.clone();
    let day_reader: StorageReaderHandle = day;
    let source = NodRepositoryReader::new(day_reader);
    let shared = NodRepositoryWriter::new(reader.storage.clone(), route.shared_writer.clone());
    if items {
        let mut session = source.projection_session(&[id], &[])?;
        handle.apply_atomic(&session.store_item(id, record.value, record.metadata)?)?;
        shared.delete_nod(id)?;
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
    migrate(reader)?;
    let route = route(reader)?;
    let Some(day) = route
        .databases
        .nod_if_present(nod_id.worldwide_day().value())?
    else {
        return Ok(None);
    };
    open_day_reader(route, day).get_with_metadata(nod_id)
}

pub(super) fn get_stored_item(
    reader: &NodRepositoryReader,
    nod_id: WwdEntityId,
) -> Result<Option<outbe_compressed_entities::StoredBody>, NodRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let Some(day) = route
        .databases
        .nod_if_present(nod_id.worldwide_day().value())?
    else {
        return Ok(None);
    };
    open_day_reader(route, day).get_stored_item(nod_id)
}

pub(super) fn get_bucket_with_metadata(
    reader: &NodRepositoryReader,
    bucket_id: WwdEntityId,
) -> Result<Option<super::NodBucketRecordWithMetadata>, NodRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let Some(day) = route
        .databases
        .nod_if_present(bucket_id.worldwide_day().value())?
    else {
        return Ok(None);
    };
    open_day_reader(route, day).get_bucket_with_metadata(bucket_id)
}

pub(super) fn get_stored_bucket(
    reader: &NodRepositoryReader,
    bucket_id: WwdEntityId,
) -> Result<Option<outbe_compressed_entities::StoredBody>, NodRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let Some(day) = route
        .databases
        .nod_if_present(bucket_id.worldwide_day().value())?
    else {
        return Ok(None);
    };
    open_day_reader(route, day).get_stored_bucket(bucket_id)
}

pub(super) fn projection_session(
    reader: &NodRepositoryReader,
    nod_ids: &[WwdEntityId],
    bucket_ids: &[WwdEntityId],
) -> Result<crate::projection::NodProjectionSession, NodRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let mut items = Vec::with_capacity(nod_ids.len());
    for nod_id in nod_ids {
        let item = match route
            .databases
            .nod_if_present(nod_id.worldwide_day().value())?
        {
            Some(day) => open_day_reader(route, day).get_with_metadata(*nod_id)?,
            None => None,
        };
        items.push(item);
    }
    let mut buckets = Vec::with_capacity(bucket_ids.len());
    for bucket_id in bucket_ids {
        let bucket = match route
            .databases
            .nod_if_present(bucket_id.worldwide_day().value())?
        {
            Some(day) => open_day_reader(route, day).get_bucket_with_metadata(*bucket_id)?,
            None => None,
        };
        buckets.push(bucket);
    }
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
    let start = request
        .after
        .map(|id| id.worldwide_day().value())
        .unwrap_or(0);
    let days = route.databases.directory().list_nod_days()?;
    walk_ids(
        route,
        &days,
        start,
        request.after,
        limit,
        |day_reader, after, limit| day_reader.list_ids_all(IdPageRequest { after, limit }),
    )
}

pub(super) fn list_ids_by_owner(
    reader: &NodRepositoryReader,
    owner: Address,
    request: IdPageRequest,
) -> Result<IdPage, NodRepositoryError> {
    migrate(reader)?;
    let limit = super::validate_id_page_request(request)?;
    let route = route(reader)?;
    let start = request
        .after
        .map(|id| id.worldwide_day().value())
        .unwrap_or(0);
    let days = route.databases.directory().list_nod_days()?;
    walk_ids(
        route,
        &days,
        start,
        request.after,
        limit,
        |day_reader, after, limit| {
            day_reader.list_ids_by_owner(owner, IdPageRequest { after, limit })
        },
    )
}

pub(super) fn put_nod(
    writer: &NodRepositoryWriter,
    nod: &crate::NodItemState,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let day = route(&writer.reader)?
        .databases
        .nod(nod.worldwide_day.value())?;
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    NodRepositoryWriter::new(reader, writer).put_nod(nod)
}

pub(super) fn delete_nod(
    writer: &NodRepositoryWriter,
    nod_id: WwdEntityId,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let Some(day) = route(&writer.reader)?
        .databases
        .nod_if_present(nod_id.worldwide_day().value())?
    else {
        return Ok(());
    };
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    NodRepositoryWriter::new(reader, writer).delete_nod(nod_id)
}

pub(super) fn put_bucket(
    writer: &NodRepositoryWriter,
    bucket: &crate::NodBucketState,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let day = route(&writer.reader)?
        .databases
        .nod(bucket.worldwide_day.value())?;
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    NodRepositoryWriter::new(reader, writer).put_bucket(bucket)
}

pub(super) fn delete_bucket(
    writer: &NodRepositoryWriter,
    bucket_id: WwdEntityId,
) -> Result<(), NodRepositoryError> {
    migrate(&writer.reader)?;
    let Some(day) = route(&writer.reader)?
        .databases
        .nod_if_present(bucket_id.worldwide_day().value())?
    else {
        return Ok(());
    };
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    NodRepositoryWriter::new(reader, writer).delete_bucket(bucket_id)
}

fn route(reader: &NodRepositoryReader) -> Result<&DayRoute, NodRepositoryError> {
    reader.route.as_ref().ok_or_else(|| {
        NodRepositoryError::Storage(outbe_offchain_storage::StorageError::InvalidArgument(
            "Nod day route is missing".into(),
        ))
    })
}

fn open_day_reader(route: &DayRoute, storage: Arc<RocksDbStorage>) -> NodRepositoryReader {
    let handle: StorageReaderHandle = storage;
    let handle = match &route.wrap {
        Some(wrap) => wrap(handle),
        None => handle,
    };
    NodRepositoryReader::new(handle)
}

fn walk_ids(
    route: &DayRoute,
    days: &[u32],
    start: u32,
    request_after: Option<WwdEntityId>,
    limit: usize,
    mut read_page: impl FnMut(
        &NodRepositoryReader,
        Option<WwdEntityId>,
        u32,
    ) -> Result<IdPage, NodRepositoryError>,
) -> Result<IdPage, NodRepositoryError> {
    let mut ids = Vec::new();
    let mut remaining = u32::try_from(limit).unwrap_or(u32::MAX);
    for day in days.iter().copied().filter(|day| *day >= start) {
        let Some(storage) = route.databases.nod_if_present(day)? else {
            continue;
        };
        let day_reader = open_day_reader(route, storage);
        let mut after = (day == start).then_some(request_after).flatten();
        loop {
            let page = read_page(&day_reader, after, remaining)?;
            let day_has_more = page.next_after.is_some();
            let page_was_empty = page.ids.is_empty();
            remaining = remaining.saturating_sub(u32::try_from(page.ids.len()).unwrap_or(u32::MAX));
            ids.extend(page.ids);
            if remaining == 0 {
                let more = day_has_more || later_id(days, day, route, &mut read_page)?;
                return Ok(IdPage {
                    next_after: more.then(|| ids.last().copied()).flatten(),
                    ids,
                });
            }
            if page_was_empty || !day_has_more {
                break;
            }
            after = page.next_after;
        }
    }
    Ok(IdPage {
        ids,
        next_after: None,
    })
}

fn later_id(
    days: &[u32],
    current: u32,
    route: &DayRoute,
    read_page: &mut impl FnMut(
        &NodRepositoryReader,
        Option<WwdEntityId>,
        u32,
    ) -> Result<IdPage, NodRepositoryError>,
) -> Result<bool, NodRepositoryError> {
    for day in days.iter().copied().filter(|day| *day > current) {
        let Some(storage) = route.databases.nod_if_present(day)? else {
            continue;
        };
        let day_reader = open_day_reader(route, storage);
        let page = read_page(&day_reader, None, 1)?;
        if !page.ids.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}
