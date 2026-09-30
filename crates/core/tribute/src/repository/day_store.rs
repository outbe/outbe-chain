//! Tribute bodies live in the database of their worldwide day.

use std::sync::Arc;

use alloy_primitives::Address;
use outbe_compressed_entities::{IdPage, IdPageRequest, WwdEntityId};
use outbe_offchain_storage::{
    DayDatabases, Key, RocksDbStorage, ScanRequest, StorageReaderHandle, StorageWriterHandle, Value,
};
use outbe_primitives::time::WorldwideDay;

use super::{
    namespace, TributePage, TributePageRequest, TributeRepositoryError, TributeRepositoryReader,
    TributeRepositoryWriter, TRIBUTES_NAMESPACE,
};

const MIGRATION_NAMESPACE: &str = "tribute_day_migration";
const CURSOR_KEY: &[u8] = b"cursor";
const MIGRATION_DONE: &[u8] = b"done";
const MIGRATION_PAGE: usize = 128;

pub(super) type DayReadWrap = Arc<dyn Fn(StorageReaderHandle) -> StorageReaderHandle + Send + Sync>;

#[derive(Clone)]
pub(super) struct DayRoute {
    pub(super) shared_writer: StorageWriterHandle,
    pub(super) databases: Arc<DayDatabases>,
    pub(super) wrap: Option<DayReadWrap>,
}

pub(super) fn migrate(reader: &TributeRepositoryReader) -> Result<(), TributeRepositoryError> {
    let Some(route) = &reader.route else {
        return Ok(());
    };
    let migration = namespace(MIGRATION_NAMESPACE)?;
    let cursor_key = Key::new(CURSOR_KEY.to_vec())?;
    if reader
        .storage
        .get(migration.clone(), &cursor_key)?
        .as_ref()
        .is_some_and(|value| value.as_bytes() == MIGRATION_DONE)
    {
        return Ok(());
    }
    let tributes = namespace(TRIBUTES_NAMESPACE)?;
    let mut after = reader
        .storage
        .get(migration.clone(), &cursor_key)?
        .map(|value| Key::new(value.as_bytes().to_vec()))
        .transpose()?;
    loop {
        let page = reader.storage.scan_prefix(
            tributes.clone(),
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
            let tribute_id = WwdEntityId::try_from(entry.key.as_bytes())
                .map_err(|_| TributeRepositoryError::MalformedPrimaryKey)?;
            let Some(record) = reader.storage.get_record(tributes.clone(), &entry.key)? else {
                continue;
            };
            let day = route
                .databases
                .tribute(tribute_id.worldwide_day().value())?;
            let handle: StorageWriterHandle = day.clone();
            let day_reader: StorageReaderHandle = day;
            let mut session =
                TributeRepositoryReader::new(day_reader).projection_session(&[tribute_id])?;
            handle.apply_atomic(&session.store(tribute_id, record.value, record.metadata)?)?;
            TributeRepositoryWriter::new(reader.storage.clone(), route.shared_writer.clone())
                .delete(tribute_id)?;
        }
        route.shared_writer.put(
            migration.clone(),
            &cursor_key,
            &Value::new(last.as_bytes().to_vec())?,
        )?;
        after = Some(last);
    }
}

pub(super) fn get_with_metadata(
    reader: &TributeRepositoryReader,
    tribute_id: WwdEntityId,
) -> Result<
    Option<(
        crate::TributeData,
        Option<outbe_offchain_storage::StorageMetadata>,
    )>,
    TributeRepositoryError,
> {
    migrate(reader)?;
    let route = route(reader)?;
    let Some(day) = route
        .databases
        .tribute_if_present(tribute_id.worldwide_day().value())?
    else {
        return Ok(None);
    };
    open_day_reader(route, day).get_with_metadata(tribute_id)
}

pub(super) fn get_stored_body(
    reader: &TributeRepositoryReader,
    tribute_id: WwdEntityId,
) -> Result<Option<outbe_compressed_entities::StoredBody>, TributeRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let Some(day) = route
        .databases
        .tribute_if_present(tribute_id.worldwide_day().value())?
    else {
        return Ok(None);
    };
    open_day_reader(route, day).get_stored_body(tribute_id)
}

pub(super) fn projection_session(
    reader: &TributeRepositoryReader,
    tribute_ids: &[WwdEntityId],
) -> Result<crate::projection::TributeProjectionSession, TributeRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let mut records = Vec::with_capacity(tribute_ids.len());
    for tribute_id in tribute_ids {
        let record = match route
            .databases
            .tribute_if_present(tribute_id.worldwide_day().value())?
        {
            Some(day) => open_day_reader(route, day).storage.get_record(
                namespace(TRIBUTES_NAMESPACE)?,
                &super::primary_key(*tribute_id)?,
            )?,
            None => None,
        };
        records.push(record);
    }
    crate::projection::TributeProjectionSession::from_records(tribute_ids, records)
}

pub(super) fn list_by_owner(
    reader: &TributeRepositoryReader,
    owner: Address,
    request: TributePageRequest,
) -> Result<TributePage, TributeRepositoryError> {
    migrate(reader)?;
    super::validate_page_limit(request.limit)?;
    let route = route(reader)?;
    let start = request
        .after
        .map(|id| id.worldwide_day().value())
        .unwrap_or(0);
    let days = route.databases.directory().list_tribute_days()?;
    let mut records = Vec::new();
    let mut remaining = request.limit;
    for day in days.iter().copied().filter(|day| *day >= start) {
        let Some(storage) = route.databases.tribute_if_present(day)? else {
            continue;
        };
        let day_reader = open_day_reader(route, storage);
        let mut after = (day == start).then_some(request.after).flatten();
        loop {
            let page = day_reader.list_by_owner(
                owner,
                TributePageRequest {
                    after,
                    limit: remaining,
                },
            )?;
            let day_has_more = page.next_after.is_some();
            remaining = remaining.saturating_sub(page.records.len());
            let page_was_empty = page.records.is_empty();
            records.extend(page.records);
            if remaining == 0 {
                let more = day_has_more || later_owner(&days, day, owner, route)?;
                return Ok(TributePage {
                    next_after: more
                        .then(|| records.last().map(|record| record.tribute_id))
                        .flatten(),
                    records,
                });
            }
            if page_was_empty || !day_has_more {
                break;
            }
            after = page.next_after;
        }
    }
    Ok(TributePage {
        records,
        next_after: None,
    })
}

pub(super) fn list_ids_by_owner(
    reader: &TributeRepositoryReader,
    owner: Address,
    request: IdPageRequest,
) -> Result<IdPage, TributeRepositoryError> {
    migrate(reader)?;
    let limit = super::validate_id_page_request(request)?;
    let route = route(reader)?;
    let start = request
        .after
        .map(|id| id.worldwide_day().value())
        .unwrap_or(0);
    let days = route.databases.directory().list_tribute_days()?;
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

pub(super) fn list_ids_by_day(
    reader: &TributeRepositoryReader,
    worldwide_day: WorldwideDay,
    request: IdPageRequest,
) -> Result<IdPage, TributeRepositoryError> {
    migrate(reader)?;
    let route = route(reader)?;
    let Some(storage) = route.databases.tribute_if_present(worldwide_day.value())? else {
        super::validate_id_page_request(request)?;
        return Ok(IdPage {
            ids: Vec::new(),
            next_after: None,
        });
    };
    open_day_reader(route, storage).list_ids_by_day(worldwide_day, request)
}

pub(super) fn put(
    writer: &TributeRepositoryWriter,
    tribute: &crate::TributeData,
) -> Result<(), TributeRepositoryError> {
    migrate(&writer.reader)?;
    let day = route(&writer.reader)?
        .databases
        .tribute(tribute.worldwide_day.value())?;
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    TributeRepositoryWriter::new(reader, writer).put(tribute)
}

pub(super) fn delete(
    writer: &TributeRepositoryWriter,
    tribute_id: WwdEntityId,
) -> Result<(), TributeRepositoryError> {
    migrate(&writer.reader)?;
    let Some(day) = route(&writer.reader)?
        .databases
        .tribute_if_present(tribute_id.worldwide_day().value())?
    else {
        return Ok(());
    };
    let reader: StorageReaderHandle = day.clone();
    let writer: StorageWriterHandle = day;
    TributeRepositoryWriter::new(reader, writer).delete(tribute_id)
}

fn route(reader: &TributeRepositoryReader) -> Result<&DayRoute, TributeRepositoryError> {
    reader.route.as_ref().ok_or_else(|| {
        TributeRepositoryError::Storage(outbe_offchain_storage::StorageError::InvalidArgument(
            "Tribute day route is missing".into(),
        ))
    })
}

fn open_day_reader(route: &DayRoute, storage: Arc<RocksDbStorage>) -> TributeRepositoryReader {
    let handle: StorageReaderHandle = storage;
    let handle = match &route.wrap {
        Some(wrap) => wrap(handle),
        None => handle,
    };
    TributeRepositoryReader::new(handle)
}

fn later_owner(
    days: &[u32],
    current: u32,
    owner: Address,
    route: &DayRoute,
) -> Result<bool, TributeRepositoryError> {
    for day in days.iter().copied().filter(|day| *day > current) {
        let Some(storage) = route.databases.tribute_if_present(day)? else {
            continue;
        };
        let page = open_day_reader(route, storage).list_by_owner(
            owner,
            TributePageRequest {
                after: None,
                limit: 1,
            },
        )?;
        if !page.records.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn walk_ids(
    route: &DayRoute,
    days: &[u32],
    start: u32,
    request_after: Option<WwdEntityId>,
    limit: usize,
    mut read_page: impl FnMut(
        &TributeRepositoryReader,
        Option<WwdEntityId>,
        u32,
    ) -> Result<IdPage, TributeRepositoryError>,
) -> Result<IdPage, TributeRepositoryError> {
    let mut ids = Vec::new();
    let mut remaining = u32::try_from(limit).unwrap_or(u32::MAX);
    for day in days.iter().copied().filter(|day| *day >= start) {
        let Some(storage) = route.databases.tribute_if_present(day)? else {
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
        &TributeRepositoryReader,
        Option<WwdEntityId>,
        u32,
    ) -> Result<IdPage, TributeRepositoryError>,
) -> Result<bool, TributeRepositoryError> {
    for day in days.iter().copied().filter(|day| *day > current) {
        let Some(storage) = route.databases.tribute_if_present(day)? else {
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
