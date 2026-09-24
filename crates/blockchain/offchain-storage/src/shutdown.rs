//! Completion of native backend destruction, independent of its read/write handles.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Does not retain the database. Completion means its native destructor returned.
#[derive(Clone, Debug)]
pub struct StorageCloseObserver(Arc<(Mutex<bool>, Condvar)>);

impl StorageCloseObserver {
    /// Wait after stopping consumers and tearing down their runtimes.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let (closed, changed) = self.0.as_ref();
        let closed = closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (closed, _) = changed
            .wait_timeout_while(closed, timeout, |closed| !*closed)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *closed
    }
}

/// Must be declared after the native DB field, so it signals only after DB::drop.
pub(crate) struct StorageCloseSignal(pub(crate) StorageCloseObserver);

impl Default for StorageCloseSignal {
    fn default() -> Self {
        Self(StorageCloseObserver(Arc::new((
            Mutex::new(false),
            Condvar::new(),
        ))))
    }
}

impl Drop for StorageCloseSignal {
    fn drop(&mut self) {
        let (closed, changed) = self.0 .0.as_ref();
        *closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        changed.notify_all();
    }
}
