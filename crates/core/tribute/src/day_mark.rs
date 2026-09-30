//! Shared-database mark for one Tribute worldwide-day directory.
//!
//! `drop_pending` is written before the directory is removed. `retired` is written
//! after the directory is gone. `retained` keeps the directory until the lease releases.

use alloy_primitives::B256;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanRequest, StorageError,
    StorageReader, StorageWriter, Value,
};

use crate::repository::namespace;
use crate::TributeRepositoryError;

pub const TRIBUTE_DAY_MARK_NAMESPACE: &str = "tribute_day_mark";

const DROP_PENDING: u8 = 1;
const RETIRED: u8 = 2;
const RETAINED: u8 = 3;

/// Lifecycle of one `tribute-days/<day>/` directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TributeDayMark {
    DropPending,
    Retired,
    Retained(B256),
}

pub fn read_tribute_day_mark(
    storage: &dyn StorageReader,
    day: u32,
) -> Result<Option<TributeDayMark>, TributeRepositoryError> {
    let record = storage.get_record(mark_namespace()?, &day_key(day)?)?;
    record
        .map(|record| decode_mark(record.value.as_bytes()))
        .transpose()
}

pub fn write_tribute_day_mark(
    writer: &dyn StorageWriter,
    day: u32,
    mark: TributeDayMark,
) -> Result<(), TributeRepositoryError> {
    let batch = AtomicWriteBatch::from_operations(vec![tribute_day_mark_operation(day, mark)?]);
    batch.validate()?;
    writer.apply_atomic(&batch)?;
    Ok(())
}

pub fn list_tribute_day_marks(
    storage: &dyn StorageReader,
) -> Result<Vec<(u32, TributeDayMark)>, TributeRepositoryError> {
    let mut after = None;
    let mut marks = Vec::new();
    loop {
        let page = storage.scan_prefix(
            mark_namespace()?,
            ScanRequest::new(&[], after.as_ref(), 1_024)?,
        )?;
        let next = page.next_after.clone();
        for entry in page.entries {
            let day = day_from_key(entry.key.as_bytes())?;
            marks.push((day, decode_mark(entry.value.as_bytes())?));
        }
        match next {
            Some(key) => after = Some(key),
            None => break,
        }
    }
    Ok(marks)
}

pub fn tribute_day_mark_operation(
    day: u32,
    mark: TributeDayMark,
) -> Result<AtomicWriteOperation, TributeRepositoryError> {
    Ok(AtomicWriteOperation::put(
        mark_namespace()?,
        day_key(day)?,
        Value::new(encode_mark(mark))?,
    ))
}

fn mark_namespace() -> Result<Namespace, TributeRepositoryError> {
    namespace(TRIBUTE_DAY_MARK_NAMESPACE)
}

fn day_key(day: u32) -> Result<Key, TributeRepositoryError> {
    Ok(Key::new(day.to_be_bytes().to_vec())?)
}

fn day_from_key(bytes: &[u8]) -> Result<u32, TributeRepositoryError> {
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| {
        TributeRepositoryError::Storage(StorageError::Corruption(
            "tribute day mark key is not a worldwide day".into(),
        ))
    })?;
    Ok(u32::from_be_bytes(bytes))
}

fn encode_mark(mark: TributeDayMark) -> Vec<u8> {
    match mark {
        TributeDayMark::DropPending => vec![DROP_PENDING],
        TributeDayMark::Retired => vec![RETIRED],
        TributeDayMark::Retained(lease) => {
            let mut bytes = Vec::with_capacity(33);
            bytes.push(RETAINED);
            bytes.extend_from_slice(lease.as_slice());
            bytes
        }
    }
}

fn decode_mark(bytes: &[u8]) -> Result<TributeDayMark, TributeRepositoryError> {
    match bytes {
        [DROP_PENDING] => Ok(TributeDayMark::DropPending),
        [RETIRED] => Ok(TributeDayMark::Retired),
        [RETAINED, lease @ ..] if lease.len() == 32 => {
            Ok(TributeDayMark::Retained(B256::from_slice(lease)))
        }
        _ => Err(TributeRepositoryError::Storage(StorageError::Corruption(
            "tribute day mark is not a known lifecycle".into(),
        ))),
    }
}
