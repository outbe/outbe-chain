//! Open day databases kept for the life of one projection process.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{DayDirectory, RocksDbStorage, StorageError};

/// Per-day databases under one off-chain root.
///
/// Each day is opened once. Later reads and writes reuse that primary handle.
pub struct DayDatabases {
    directory: DayDirectory,
    tribute: Mutex<HashMap<u32, Arc<RocksDbStorage>>>,
    nod: Mutex<HashMap<u32, Arc<RocksDbStorage>>>,
}

impl DayDatabases {
    /// Prepare the root layout. Day databases open on first use.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        Ok(Self {
            directory: DayDirectory::open(root)?,
            tribute: Mutex::new(HashMap::new()),
            nod: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn directory(&self) -> &DayDirectory {
        &self.directory
    }

    /// Opens the day's Tribute database, creating it when absent.
    pub fn tribute(&self, day: u32) -> Result<Arc<RocksDbStorage>, StorageError> {
        open_cached(&self.tribute, day, || self.directory.open_tribute_day(day))
    }

    /// Opens an existing Tribute day. A missing directory stays missing.
    pub fn tribute_if_present(
        &self,
        day: u32,
    ) -> Result<Option<Arc<RocksDbStorage>>, StorageError> {
        present(
            &self.tribute,
            day,
            self.directory.tribute_day_path(day).exists(),
            || self.directory.open_tribute_day(day),
        )
    }

    /// Drops the cached Tribute primary so its directory can be removed.
    pub fn forget_tribute_day(&self, day: u32) -> Result<(), StorageError> {
        forget_cached(&self.tribute, day)
    }

    /// Drops the cached Nod primary so its directory can be removed.
    pub fn forget_nod_day(&self, day: u32) -> Result<(), StorageError> {
        forget_cached(&self.nod, day)
    }

    /// Opens the day's Nod database, creating it when absent.
    pub fn nod(&self, day: u32) -> Result<Arc<RocksDbStorage>, StorageError> {
        open_cached(&self.nod, day, || self.directory.open_nod_day(day))
    }

    /// Opens an existing Nod day. A missing directory stays missing.
    pub fn nod_if_present(&self, day: u32) -> Result<Option<Arc<RocksDbStorage>>, StorageError> {
        present(
            &self.nod,
            day,
            self.directory.nod_day_path(day).exists(),
            || self.directory.open_nod_day(day),
        )
    }
}

fn present(
    slots: &Mutex<HashMap<u32, Arc<RocksDbStorage>>>,
    day: u32,
    exists: bool,
    open: impl FnOnce() -> Result<RocksDbStorage, StorageError>,
) -> Result<Option<Arc<RocksDbStorage>>, StorageError> {
    if let Some(storage) = cached(slots, day) {
        return Ok(Some(storage));
    }
    if !exists {
        return Ok(None);
    }
    open_cached(slots, day, open).map(Some)
}

fn forget_cached(
    slots: &Mutex<HashMap<u32, Arc<RocksDbStorage>>>,
    day: u32,
) -> Result<(), StorageError> {
    let removed = {
        let mut slots = slots.lock().unwrap_or_else(|error| error.into_inner());
        slots.remove(&day)
    };
    let Some(storage) = removed else {
        return Ok(());
    };
    let waiter = storage.close_waiter();
    drop(storage);
    if waiter.wait_timeout(Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(StorageError::unavailable(std::io::Error::other(
            "day database is still open",
        )))
    }
}

fn cached(
    slots: &Mutex<HashMap<u32, Arc<RocksDbStorage>>>,
    day: u32,
) -> Option<Arc<RocksDbStorage>> {
    let slots = slots.lock().unwrap_or_else(|error| error.into_inner());
    slots.get(&day).map(Arc::clone)
}

fn open_cached(
    slots: &Mutex<HashMap<u32, Arc<RocksDbStorage>>>,
    day: u32,
    open: impl FnOnce() -> Result<RocksDbStorage, StorageError>,
) -> Result<Arc<RocksDbStorage>, StorageError> {
    let mut slots = slots.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(storage) = slots.get(&day) {
        return Ok(Arc::clone(storage));
    }
    let storage = Arc::new(open()?);
    slots.insert(day, Arc::clone(&storage));
    Ok(storage)
}
