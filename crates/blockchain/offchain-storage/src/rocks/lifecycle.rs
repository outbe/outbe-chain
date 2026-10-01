use crate::{RocksDbCloseWaiter, RocksDbStorage, StorageError, StorageLifecycle};
use std::{sync::Arc, time::Duration};

pub(crate) struct RocksLifecycle {
    storage: Option<Arc<RocksDbStorage>>,
    waiter: RocksDbCloseWaiter,
}
impl RocksLifecycle {
    pub(crate) fn new(storage: Arc<RocksDbStorage>) -> Self {
        let waiter = storage.close_waiter();
        Self {
            storage: Some(storage),
            waiter,
        }
    }
}
impl StorageLifecycle for RocksLifecycle {
    fn activate(&self) -> Result<(), StorageError> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), StorageError> {
        self.storage.take();
        while !self.waiter.wait_timeout(Duration::from_secs(5)) {}
        Ok(())
    }
}
