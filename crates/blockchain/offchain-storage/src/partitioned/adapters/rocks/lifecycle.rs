use super::RocksPartitionDataSource;
use crate::{RocksDbCloseWaiter, StorageError, StorageLifecycle};
use std::{sync::Arc, time::Duration};

pub(crate) struct RocksPartitionLifecycle {
    source: Option<Arc<RocksPartitionDataSource>>,
    waiter: RocksDbCloseWaiter,
    partitions: Arc<parking_lot::Mutex<Vec<RocksDbCloseWaiter>>>,
}
impl RocksPartitionLifecycle {
    pub(crate) fn new(source: Arc<RocksPartitionDataSource>) -> Self {
        let waiter = source.close_waiter();
        let partitions = source.close_waiters.clone();
        Self {
            source: Some(source),
            waiter,
            partitions,
        }
    }
}
impl StorageLifecycle for RocksPartitionLifecycle {
    fn activate(&self) -> Result<(), StorageError> {
        self.source
            .as_ref()
            .expect("live ownership")
            .complete_recovery()
    }
    fn close(&mut self) -> Result<(), StorageError> {
        self.source.take();
        // System closes after the source: no further primaries can be registered.
        while !self.waiter.wait_timeout(Duration::from_secs(5)) {}
        for waiter in self.partitions.lock().clone() {
            while !waiter.wait_timeout(Duration::from_secs(5)) {}
        }
        Ok(())
    }
}
