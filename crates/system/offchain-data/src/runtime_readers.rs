//! Read-only Tribute and Nod body capabilities used by runtime execution.

use outbe_compressed_entities::{
    EntityRef, IdPage, IdPageRequest, ParentBodySource, ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_nod::{NodRepositoryError, NodRepositoryReader};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use outbe_offchain_storage::{
    Key, Namespace, ScanPage, ScanRequest, StorageError, StorageErrorKind, StorageReader,
    StorageReaderHandle, StoredValue,
};
use outbe_primitives::projection::{
    ExecutionReadBudget, ProjectionFailure, ProjectionFailureClass,
};
use outbe_tribute::{TributeRepositoryError, TributeRepositoryReader};

const MAX_CONCURRENT_EXECUTION_READS: usize = 64;
static ACTIVE_EXECUTION_READS: AtomicUsize = AtomicUsize::new(0);

struct ExecutionReadPermit;

impl ExecutionReadPermit {
    fn acquire() -> Result<Self, StorageError> {
        ACTIVE_EXECUTION_READS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONCURRENT_EXECUTION_READS).then_some(active + 1)
            })
            .map(|_| Self)
            .map_err(|_| StorageError::Unavailable {
                source: Box::new(std::io::Error::other(
                    "execution body read worker capacity is exhausted",
                )),
            })
    }
}

impl Drop for ExecutionReadPermit {
    fn drop(&mut self) {
        ACTIVE_EXECUTION_READS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Read-side infrastructure incident sent to the projection supervisor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeBodyFailure {
    Unavailable,
    Fatal(ProjectionFailure),
}

#[derive(Default)]
struct ExecutionReadBudgets {
    next_id: AtomicU64,
    active: Mutex<BTreeMap<u64, ExecutionReadBudget>>,
}

impl ExecutionReadBudgets {
    fn enter(self: &Arc<Self>, budget: ExecutionReadBudget) -> ExecutionReadBudgetGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut active) = self.active.lock() {
            active.insert(id, budget);
        }
        ExecutionReadBudgetGuard {
            id,
            budgets: self.clone(),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.active
            .lock()
            .map(|active| active.values().any(ExecutionReadBudget::is_cancelled))
            .unwrap_or(true)
    }
}

/// Keeps one execution request's read budget active for the executor lifetime.
pub struct ExecutionReadBudgetGuard {
    id: u64,
    budgets: Arc<ExecutionReadBudgets>,
}

impl Drop for ExecutionReadBudgetGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.budgets.active.lock() {
            active.remove(&self.id);
        }
    }
}

struct BudgetedStorageReader {
    inner: StorageReaderHandle,
    budgets: Arc<ExecutionReadBudgets>,
}

impl BudgetedStorageReader {
    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(StorageReaderHandle) -> Result<T, StorageError> + Send + 'static,
    ) -> Result<T, StorageError> {
        if self.budgets.is_cancelled() {
            return Err(StorageError::RequestDeadline);
        }
        let permit = ExecutionReadPermit::acquire()?;
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("offchain-read".to_owned())
            .spawn(move || {
                let _permit = permit;
                let _ = result_tx.send(operation(inner));
            })
            .map_err(|error| StorageError::Unavailable {
                source: Box::new(error),
            })?;
        let started = Instant::now();
        loop {
            if self.budgets.is_cancelled() {
                return Err(StorageError::RequestDeadline);
            }
            let remaining = Duration::from_secs(1).saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(StorageError::Unavailable {
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "execution body read exceeded the one-second MongoDB operation limit",
                    )),
                });
            }
            match result_rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(result) => return result,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(StorageError::Backend {
                        source: Box::new(std::io::Error::other(
                            "execution body read worker exited unexpectedly",
                        )),
                    });
                }
            }
        }
    }
}

impl StorageReader for BudgetedStorageReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        let key = key.clone();
        self.run(move |inner| inner.get_record(namespace, &key))
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        let keys = keys.to_vec();
        self.run(move |inner| inner.get_records(namespace, &keys))
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        let prefix = request.prefix().to_vec();
        let after = request.after().cloned();
        let limit = request.limit();
        self.run(move |inner| {
            let request = ScanRequest::new(&prefix, after.as_ref(), limit)?;
            inner.scan_prefix(namespace, request)
        })
    }
}

/// Cloneable runtime authority for typed Tribute and Nod body reads.
///
/// The underlying storage capability is accepted only during construction and
/// is deliberately not exposed. Runtime consumers receive domain-owned typed
/// readers and cannot acquire projection write authority through this bundle.
#[derive(Clone)]
pub struct RuntimeBodyReaders {
    storage: StorageReaderHandle,
    tribute: TributeRepositoryReader,
    nod: NodRepositoryReader,
    failure_sender: Option<tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>>,
    budgets: Arc<ExecutionReadBudgets>,
}

impl RuntimeBodyReaders {
    /// Builds both domain readers over one shared storage adapter.
    #[must_use]
    pub fn new(storage: StorageReaderHandle) -> Self {
        let budgets = Arc::new(ExecutionReadBudgets::default());
        let raw_storage = storage.clone();
        let storage: StorageReaderHandle = Arc::new(BudgetedStorageReader {
            inner: storage,
            budgets: budgets.clone(),
        });
        Self {
            storage: raw_storage,
            tribute: TributeRepositoryReader::new(storage.clone()),
            nod: NodRepositoryReader::new(storage),
            failure_sender: None,
            budgets,
        }
    }

    /// Builds supervised readers whose infrastructure failures share the ExEx outage lifecycle.
    #[must_use]
    pub fn new_supervised(
        storage: StorageReaderHandle,
        failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
    ) -> Self {
        let budgets = Arc::new(ExecutionReadBudgets::default());
        let raw_storage = storage.clone();
        let storage: StorageReaderHandle = Arc::new(BudgetedStorageReader {
            inner: storage,
            budgets: budgets.clone(),
        });
        Self {
            storage: raw_storage,
            tribute: TributeRepositoryReader::new(storage.clone()),
            nod: NodRepositoryReader::new(storage),
            failure_sender: Some(failure_sender),
            budgets,
        }
    }

    /// Creates an execution-local budget scope over the same least-authority backend.
    #[must_use]
    pub fn fork_execution(&self) -> Self {
        let budgets = Arc::new(ExecutionReadBudgets::default());
        let storage: StorageReaderHandle = Arc::new(BudgetedStorageReader {
            inner: self.storage.clone(),
            budgets: budgets.clone(),
        });
        Self {
            storage: self.storage.clone(),
            tribute: TributeRepositoryReader::new(storage.clone()),
            nod: NodRepositoryReader::new(storage),
            failure_sender: self.failure_sender.clone(),
            budgets,
        }
    }

    /// Applies the caller's remaining execution budget to every body read in this executor.
    #[must_use]
    pub fn enter_execution_budget(&self, budget: ExecutionReadBudget) -> ExecutionReadBudgetGuard {
        self.budgets.enter(budget)
    }

    /// Returns the typed Tribute body reader.
    #[must_use]
    pub const fn tribute(&self) -> &TributeRepositoryReader {
        &self.tribute
    }

    /// Returns the typed Nod item and bucket reader.
    #[must_use]
    pub const fn nod(&self) -> &NodRepositoryReader {
        &self.nod
    }

    /// Reports a technical read failure without exposing readiness write authority to domains.
    pub fn report_unavailable(&self) {
        if let Some(sender) = &self.failure_sender {
            sender.send_if_modified(|current| {
                if matches!(current, Some(RuntimeBodyFailure::Fatal(_))) {
                    false
                } else {
                    *current = Some(RuntimeBodyFailure::Unavailable);
                    true
                }
            });
        }
    }

    /// Reports deterministic body/index corruption to the shared lifecycle owner.
    pub fn report_fatal(
        &self,
        class: ProjectionFailureClass,
        message: impl Into<std::sync::Arc<str>>,
    ) {
        if let Some(sender) = &self.failure_sender {
            sender.send_replace(Some(RuntimeBodyFailure::Fatal(ProjectionFailure::new(
                class, message,
            ))));
        }
    }

    /// Publishes only explicitly classified off-chain body read failures.
    pub fn report_precompile_error(&self, error: &outbe_primitives::error::PrecompileError) {
        match error {
            outbe_primitives::error::PrecompileError::BodyReadUnavailable(_) => {
                self.report_unavailable();
            }
            outbe_primitives::error::PrecompileError::BodyReadCorruption(message) => {
                self.report_fatal(ProjectionFailureClass::CorruptBody, message.clone());
            }
            _ => {}
        }
    }
}

impl ParentBodySource for RuntimeBodyReaders {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        match entity {
            EntityRef::Tribute(tribute_id) => self
                .tribute
                .get_stored_body(tribute_id)
                .map_err(map_tribute_parent_error),
            EntityRef::NodItem(nod_id) => self
                .nod
                .get_stored_item(nod_id)
                .map_err(map_nod_parent_error),
            EntityRef::NodBucket(bucket_id) => self
                .nod
                .get_stored_bucket(bucket_id)
                .map_err(map_nod_parent_error),
        }
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> Result<IdPage, ParentBodySourceError> {
        match query {
            QueryRef::TributeByOwner(owner) => self
                .tribute
                .list_ids_by_owner(owner, request)
                .map_err(map_tribute_parent_error),
            QueryRef::TributeByDay(worldwide_day) => self
                .tribute
                .list_ids_by_day(worldwide_day, request)
                .map_err(map_tribute_parent_error),
            QueryRef::NodByOwner(owner) => self
                .nod
                .list_ids_by_owner(owner, request)
                .map_err(map_nod_parent_error),
            QueryRef::NodAll => self.nod.list_ids_all(request).map_err(map_nod_parent_error),
        }
    }
}

fn map_storage_parent_error(kind: StorageErrorKind, message: String) -> ParentBodySourceError {
    match kind {
        StorageErrorKind::Unavailable | StorageErrorKind::RequestDeadline => {
            ParentBodySourceError::Unavailable(message)
        }
        StorageErrorKind::InvalidArgument
        | StorageErrorKind::Corruption
        | StorageErrorKind::Backend
        | StorageErrorKind::WriterLeaseLost => ParentBodySourceError::Corruption(message),
    }
}

fn map_tribute_parent_error(error: TributeRepositoryError) -> ParentBodySourceError {
    let message = error.to_string();
    match &error {
        TributeRepositoryError::Storage(storage) => {
            map_storage_parent_error(storage.kind(), message)
        }
        _ => ParentBodySourceError::Corruption(message),
    }
}

fn map_nod_parent_error(error: NodRepositoryError) -> ParentBodySourceError {
    let message = error.to_string();
    match &error {
        NodRepositoryError::Storage(storage) => map_storage_parent_error(storage.kind(), message),
        _ => ParentBodySourceError::Corruption(message),
    }
}
