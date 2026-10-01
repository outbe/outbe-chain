//! Ownership, activation and teardown independent of any physical backend.
mod completion;
mod handles;

use crate::{StorageError, StorageReaderHandle, StorageWriterHandle};
use completion::CompletionState;
pub use completion::{StorageCloseError, StorageCompletion};
use parking_lot::Mutex;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

/// Physical ownership and recovery supplied by an adapter at the composition root.
/// Close must be idempotent: failed cleanup is retried until acknowledged.
pub trait StorageLifecycle: Send + Sync {
    fn activate(&self) -> Result<(), StorageError>;
    fn close(&mut self) -> Result<(), StorageError>;
}

struct Session {
    reader: Option<StorageReaderHandle>,
    writer: Option<StorageWriterHandle>,
    lifecycle: Option<Box<dyn StorageLifecycle>>,
    completion: Arc<CompletionState>,
    activated: AtomicBool,
    preparation: Mutex<()>,
}

/// Shares ownership with every issued capability.
pub struct StorageOwnershipGuard(Arc<Session>);

impl StorageOwnershipGuard {
    pub fn activate(&self) -> Result<(), StorageError> {
        let _gate = self.0.preparation.lock();
        if !self.0.activated.load(Ordering::Acquire) {
            self.0
                .lifecycle
                .as_ref()
                .expect("live session")
                .activate()?;
            self.0.activated.store(true, Ordering::Release);
        }
        Ok(())
    }
    /// Scoped authority for initialization and capability probes before validation.
    /// Escaped bootstrap handles are revoked when the callback finishes, including unwinding.
    pub fn preflight<T>(
        &self,
        prepare: impl FnOnce(StorageReaderHandle, StorageWriterHandle) -> T,
    ) -> Result<T, StorageError> {
        let _gate = self.0.preparation.lock();
        if self.0.activated.load(Ordering::Acquire) {
            return Err(StorageError::InvalidArgument(
                "preflight requires an inactive session".into(),
            ));
        }
        let permit = handles::BootstrapPermit::new();
        let reader = Arc::new(handles::SessionReader(self.0.clone()));
        let writer = Arc::new(handles::SessionWriter(
            self.0.clone(),
            Some(permit.0.clone()),
        ));
        let result = prepare(reader, writer);
        drop(permit);
        Ok(result)
    }
    pub fn completion(&self) -> StorageCompletion {
        StorageCompletion(self.0.completion.clone())
    }
}

/// Reader and writer referring to the same session and physical storage.
pub struct OpenedStorage {
    pub reader: StorageReaderHandle,
    pub writer: StorageWriterHandle,
    pub ownership: StorageOwnershipGuard,
}

impl OpenedStorage {
    pub fn new(
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
        lifecycle: Box<dyn StorageLifecycle>,
    ) -> Self {
        let session = Arc::new(Session {
            reader: Some(reader),
            writer: Some(writer),
            lifecycle: Some(lifecycle),
            completion: Arc::default(),
            activated: AtomicBool::new(false),
            preparation: Mutex::new(()),
        });
        Self {
            reader: Arc::new(handles::SessionReader(session.clone())),
            writer: Arc::new(handles::SessionWriter(session.clone(), None)),
            ownership: StorageOwnershipGuard(session),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let reader = self.reader.take();
        let writer = self.writer.take();
        let mut lifecycle = self.lifecycle.take().expect("session owns lifecycle");
        let completion = self.completion.clone();
        // Native destructors and network cleanup cannot block the observer's timeout.
        let result = std::thread::Builder::new()
            .name("storage-ownership-close".into())
            .spawn(move || {
                drop(writer);
                drop(reader);
                loop {
                    match lifecycle.close() {
                        Ok(()) => {
                            completion.closed();
                            break;
                        }
                        Err(error) => {
                            completion.failed(error);
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                }
            });
        if let Err(error) = result {
            self.completion.failed(StorageError::unavailable(error));
        }
    }
}
