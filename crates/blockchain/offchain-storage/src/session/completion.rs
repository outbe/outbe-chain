use crate::StorageError;
use parking_lot::{Condvar, Mutex};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Adapter failure is retained; timeout never means that resources were closed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum StorageCloseError {
    #[error("storage lifecycle completion deadline exceeded")]
    Timeout,
    #[error("storage ownership cleanup failed: {0}")]
    Cleanup(#[source] Arc<StorageError>),
}

#[derive(Default)]
struct State {
    closed: bool,
    failure: Option<Arc<StorageError>>,
}

#[derive(Default)]
pub(super) struct CompletionState {
    state: Mutex<State>,
    changed: Condvar,
}

/// Observes ownership release without retaining any reader, writer or session.
#[derive(Clone)]
pub struct StorageCompletion(pub(super) Arc<CompletionState>);

impl StorageCompletion {
    pub fn wait_timeout(&self, timeout: Duration) -> Result<(), StorageCloseError> {
        let started = Instant::now();
        let mut state = self.0.state.lock();
        loop {
            if state.closed {
                return Ok(());
            }
            if let Some(error) = &state.failure {
                return Err(StorageCloseError::Cleanup(error.clone()));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(StorageCloseError::Timeout);
            }
            self.0.changed.wait_for(&mut state, remaining);
        }
    }
}

impl CompletionState {
    pub(super) fn failed(&self, error: StorageError) {
        self.state.lock().failure = Some(Arc::new(error));
        self.changed.notify_all();
    }
    pub(super) fn closed(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.failure = None;
        self.changed.notify_all();
    }
}
