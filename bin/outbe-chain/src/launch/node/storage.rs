use outbe_offchain_storage::{StorageCloseError, StorageCompletion};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const CLOSE_WAIT_INTERVAL: Duration = Duration::from_secs(5);

/// Outlives Reth teardown and observes release of every offchain storage owner.
#[derive(Default)]
pub(super) struct StorageExitGuard(Arc<Mutex<Vec<StorageCompletion>>>);
impl StorageExitGuard {
    pub(super) fn observer(&self) -> impl Fn(StorageCompletion) + Send + Sync + 'static {
        let completions = self.0.clone();
        move |completion| {
            completions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(completion)
        }
    }
}
impl Drop for StorageExitGuard {
    fn drop(&mut self) {
        // Observers keep registration open for detached startup attempts.
        // When this is the sole registry owner, no further completion can arrive.
        while Arc::strong_count(&self.0) > 1 {
            tracing::warn!("waiting for offchain startup attempts to finish registering ownership");
            std::thread::sleep(CLOSE_WAIT_INTERVAL);
        }
        let completions = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        for completion in completions {
            loop {
                match completion.wait_timeout(CLOSE_WAIT_INTERVAL) {
                    Ok(()) => break,
                    Err(StorageCloseError::Timeout) => {
                        tracing::warn!("waiting for offchain storage ownership release")
                    }
                    Err(error) => {
                        tracing::error!(%error, "offchain storage cleanup failed; waiting for acknowledged release");
                        std::thread::sleep(CLOSE_WAIT_INTERVAL);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_offchain_storage::{RocksDbConfig, StorageBackend, StorageConfig, StorageProvider};
    use std::sync::mpsc;

    #[test]
    fn node_exit_guard_waits_for_storage_handles_and_native_close() {
        let root = tempfile::tempdir().unwrap();
        let config = StorageConfig {
            start_block: 1,
            backend: StorageBackend::RocksDb(RocksDbConfig {
                path: root.path().join("offchain"),
                secondary_path: root.path().join("secondary"),
            }),
        };
        let provider = StorageProvider::new(config).unwrap();
        let opened = provider.open_writer().unwrap();
        let guard = StorageExitGuard::default();
        guard.observer()(opened.ownership.completion());
        let reader = opened.reader.clone();
        drop(opened);
        let (exited, observed) = mpsc::channel();
        let closing = std::thread::spawn(move || {
            drop(guard);
            exited.send(()).unwrap();
        });
        assert!(observed.recv_timeout(Duration::from_millis(50)).is_err());
        drop(reader);
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let reopened = provider.open_writer().unwrap();
        let completion = reopened.ownership.completion();
        drop(reopened);
        completion.wait_timeout(Duration::from_secs(5)).unwrap();
        closing.join().unwrap();
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[test]
    fn node_exit_waits_for_a_startup_attempt_to_finish_registering_resources() {
        let guard = StorageExitGuard::default();
        let observer = guard.observer();
        let (exited, observed) = std::sync::mpsc::channel();
        let closing = std::thread::spawn(move || {
            drop(guard);
            exited.send(()).unwrap();
        });
        assert!(observed.recv_timeout(Duration::from_millis(50)).is_err());
        drop(observer);
        observed.recv_timeout(Duration::from_secs(6)).unwrap();
        closing.join().unwrap();
    }
}
