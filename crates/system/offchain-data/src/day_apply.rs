//! Tribute and Nod mutations commit in the database of their worldwide day.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::Address;
use outbe_nod::clear_owner_day;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, DayDatabases, StorageError, StorageReaderHandle,
    StorageWriterHandle,
};

use super::ProjectionError;

const TRIBUTE_PRIMARY: &str = "tributes";
const TRIBUTE_BY_OWNER: &str = "tributes_by_owner";
const TRIBUTE_BY_DAY: &str = "tributes_by_day";
const NOD_ITEMS: &str = "nods";
const NOD_BUCKETS: &str = "nod_buckets";
const NOD_BY_OWNER: &str = "nods_by_owner";

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Domain {
    Tribute,
    Nod,
}

/// Writes day-scoped operations and returns the batch shared storage still commits.
pub(super) fn write_day_operations(
    databases: &DayDatabases,
    batch: AtomicWriteBatch,
) -> Result<AtomicWriteBatch, ProjectionError> {
    let mut shared = Vec::new();
    let mut by_day: BTreeMap<(Domain, u32), Vec<AtomicWriteOperation>> = BTreeMap::new();
    for operation in batch.operations() {
        match day_target(operation)? {
            Some((domain, day)) => by_day
                .entry((domain, day))
                .or_default()
                .push(operation.clone()),
            None => shared.push(operation.clone()),
        }
    }
    for ((domain, day), operations) in by_day {
        let creates = operations
            .iter()
            .any(|operation| matches!(operation, AtomicWriteOperation::Put { .. }));
        let storage = match (domain, creates) {
            (Domain::Tribute, true) => Some(databases.tribute(day)?),
            (Domain::Tribute, false) => databases.tribute_if_present(day)?,
            (Domain::Nod, true) => Some(databases.nod(day)?),
            (Domain::Nod, false) => databases.nod_if_present(day)?,
        };
        let owners = match domain {
            Domain::Nod => owner_addresses(&operations)?,
            Domain::Tribute => Vec::new(),
        };
        let Some(storage) = storage else {
            for owner in owners {
                shared.push(clear_owner_day(owner, day)?);
            }
            continue;
        };
        let handle: StorageWriterHandle = storage.clone();
        let day_batch = AtomicWriteBatch::from_operations(operations);
        day_batch.validate()?;
        handle.apply_atomic(&day_batch)?;
        if domain == Domain::Nod {
            let reader: StorageReaderHandle = storage;
            let reader = outbe_nod::nod_reader(reader);
            for owner in owners {
                shared.push(reader.owner_day_marker(owner, day)?);
            }
        }
    }
    let shared = AtomicWriteBatch::from_operations(shared);
    shared.validate()?;
    Ok(shared)
}

fn day_target(operation: &AtomicWriteOperation) -> Result<Option<(Domain, u32)>, ProjectionError> {
    let (namespace, key) = match operation {
        AtomicWriteOperation::Put { namespace, key, .. }
        | AtomicWriteOperation::Delete { namespace, key } => (namespace.as_str(), key.as_bytes()),
    };
    let (domain, offset) = match namespace {
        TRIBUTE_PRIMARY | TRIBUTE_BY_DAY => (Domain::Tribute, 0),
        TRIBUTE_BY_OWNER => (Domain::Tribute, 20),
        NOD_ITEMS | NOD_BUCKETS => (Domain::Nod, 0),
        NOD_BY_OWNER => (Domain::Nod, 20),
        _ => return Ok(None),
    };
    let bytes = key.get(offset..offset + 4).ok_or_else(|| {
        StorageError::Corruption("day key is shorter than the worldwide-day prefix".into())
    })?;
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| {
        StorageError::Corruption("day key is shorter than the worldwide-day prefix".into())
    })?;
    Ok(Some((domain, u32::from_be_bytes(bytes))))
}

fn owner_addresses(operations: &[AtomicWriteOperation]) -> Result<Vec<Address>, ProjectionError> {
    let mut owners = BTreeSet::new();
    for operation in operations {
        let (namespace, key) = match operation {
            AtomicWriteOperation::Put { namespace, key, .. }
            | AtomicWriteOperation::Delete { namespace, key } => {
                (namespace.as_str(), key.as_bytes())
            }
        };
        if namespace != NOD_BY_OWNER {
            continue;
        }
        if key.len() < Address::len_bytes() {
            return Err(StorageError::Corruption(
                "Nod owner index key is shorter than an address".into(),
            )
            .into());
        }
        owners.insert(Address::from_slice(&key[..Address::len_bytes()]));
    }
    Ok(owners.into_iter().collect())
}
