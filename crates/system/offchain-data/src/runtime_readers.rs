//! Tribute and Nod body capabilities used by runtime execution.
//! The path with no day route is read-only.
//! The day-routed path holds the shared durable writer.
//! A body read on that path can migrate legacy keys and write.

use outbe_compressed_entities::{
    EntityRef, IdPage, IdPageRequest, ParentBodySource, ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_nod::{NodRepositoryError, NodRepositoryReader};
use std::{
    collections::BTreeMap,
    error::Error as _,
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

use crate::DayDatabaseRoute;
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
    Unavailable {
        generation: u64,
        since: std::time::Instant,
    },
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

fn reader_wrap(
    budgets: Arc<ExecutionReadBudgets>,
) -> Arc<dyn Fn(StorageReaderHandle) -> StorageReaderHandle + Send + Sync> {
    Arc::new(move |inner| {
        Arc::new(BudgetedStorageReader {
            inner,
            budgets: budgets.clone(),
        })
    })
}

struct BudgetedStorageReader {
    inner: StorageReaderHandle,
    budgets: Arc<ExecutionReadBudgets>,
}

#[derive(Clone, Copy)]
struct ReadDiagnostic {
    operation: &'static str,
    started: Instant,
}

impl ReadDiagnostic {
    fn report(self, stage: &'static str, error: &StorageError) {
        // Backend messages can contain keys or record contents. Inspect only typed causes.
        let io = std::iter::successors(error.source(), |cause| (*cause).source())
            .take(8)
            .find_map(|cause| cause.downcast_ref::<std::io::Error>());
        tracing::warn!(
            target: "offchain_read",
            operation = self.operation,
            stage,
            elapsed_ms = self.started.elapsed().as_millis(),
            error_kind = ?error.kind(),
            source_io_kind = ?io.map(std::io::Error::kind),
            source_os_error = ?io.and_then(std::io::Error::raw_os_error),
            "Runtime body read failed"
        );
    }

    fn failure(self, stage: &'static str, error: StorageError) -> StorageError {
        self.report(stage, &error);
        error
    }
}

impl BudgetedStorageReader {
    fn run<T: Send + 'static>(
        &self,
        operation_name: &'static str,
        operation: impl FnOnce(StorageReaderHandle) -> Result<T, StorageError> + Send + 'static,
    ) -> Result<T, StorageError> {
        let diagnostic = ReadDiagnostic {
            operation: operation_name,
            started: Instant::now(),
        };
        if self.budgets.is_cancelled() {
            return Err(diagnostic.failure("request_cancelled", StorageError::RequestDeadline));
        }
        let permit = ExecutionReadPermit::acquire()
            .map_err(|error| diagnostic.failure("capacity", error))?;
        let result_rx = self.spawn_read(diagnostic, permit, operation)?;
        let started = Instant::now();
        loop {
            if self.budgets.is_cancelled() {
                return Err(diagnostic.failure("request_cancelled", StorageError::RequestDeadline));
            }
            let remaining = Duration::from_secs(1).saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(diagnostic.failure(
                    "operation_timeout",
                    StorageError::Unavailable {
                        source: Box::new(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "execution body read exceeded the one-second MongoDB operation limit",
                        )),
                    },
                ));
            }
            match result_rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(result) => {
                    return result.inspect_err(|error| diagnostic.report("backend", error))
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(diagnostic.failure(
                        "worker_disconnected",
                        StorageError::Backend {
                            source: Box::new(std::io::Error::other(
                                "execution body read worker exited unexpectedly",
                            )),
                        },
                    ));
                }
            }
        }
    }

    fn spawn_read<T: Send + 'static>(
        &self,
        diagnostic: ReadDiagnostic,
        permit: ExecutionReadPermit,
        operation: impl FnOnce(StorageReaderHandle) -> Result<T, StorageError> + Send + 'static,
    ) -> Result<std::sync::mpsc::Receiver<Result<T, StorageError>>, StorageError> {
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("offchain-read".to_owned())
            .spawn(move || {
                let _permit = permit;
                if let Err(result) = result_tx.send(operation(inner)) {
                    tracing::warn!(
                        target: "offchain_read",
                        operation = diagnostic.operation,
                        stage = "completed_after_receiver_dropped",
                        elapsed_ms = diagnostic.started.elapsed().as_millis(),
                        error_kind = ?result.0.as_ref().err().map(StorageError::kind),
                        "Runtime body read worker completed after its caller stopped waiting"
                    );
                }
            })
            .map_err(|error| {
                diagnostic.failure(
                    "spawn",
                    StorageError::Unavailable {
                        source: Box::new(error),
                    },
                )
            })?;
        Ok(result_rx)
    }
}

impl StorageReader for BudgetedStorageReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        let key = key.clone();
        self.run("get_record", move |inner| inner.get_record(namespace, &key))
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        let keys = keys.to_vec();
        self.run("get_records", move |inner| {
            inner.get_records(namespace, &keys)
        })
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        let prefix = request.prefix().to_vec();
        let after = request.after().cloned();
        let limit = request.limit();
        self.run("scan_prefix", move |inner| {
            let request = ScanRequest::new(&prefix, after.as_ref(), limit)?;
            inner.scan_prefix(namespace, request)
        })
    }
}

/// Cloneable runtime authority for typed Tribute and Nod body reads.
///
/// Construction takes the storage capability and does not expose it.
/// Callers receive typed readers. They cannot take the writer from this bundle.
/// On the day-routed path, a body read can still migrate legacy keys.
/// That migration writes the shared database and the day databases.
#[derive(Clone)]
pub struct RuntimeBodyReaders {
    execution: ExecutionBodyReadSession,
}

#[derive(Clone)]
struct RuntimeReaderFactory {
    storage: StorageReaderHandle,
    failure_sender: Option<tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>>,
    days: Option<DayDatabaseRoute>,
}

#[derive(Clone)]
struct ExecutionBodyReadSession {
    factory: RuntimeReaderFactory,
    tribute: TributeRepositoryReader,
    nod: NodRepositoryReader,
    budgets: Arc<ExecutionReadBudgets>,
}

/// Builds both domain readers over one shared storage adapter.
#[must_use]
pub fn runtime_body_readers(storage: StorageReaderHandle) -> RuntimeBodyReaders {
    RuntimeReaderFactory {
        storage,
        failure_sender: None,
        days: None,
    }
    .build()
}

/// Builds supervised readers that share the ExEx outage lifecycle.
#[must_use]
pub fn supervised_runtime_body_readers(
    storage: StorageReaderHandle,
    failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
) -> RuntimeBodyReaders {
    RuntimeReaderFactory {
        storage,
        failure_sender: Some(failure_sender),
        days: None,
    }
    .build()
}

/// Builds supervised readers for per-day Tribute and Nod databases.
#[must_use]
pub fn supervised_day_runtime_body_readers(
    storage: StorageReaderHandle,
    days: DayDatabaseRoute,
    failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
) -> RuntimeBodyReaders {
    RuntimeReaderFactory {
        storage,
        failure_sender: Some(failure_sender),
        days: Some(days),
    }
    .build()
}

impl RuntimeReaderFactory {
    fn build(self) -> RuntimeBodyReaders {
        let budgets = Arc::new(ExecutionReadBudgets::default());
        let (tribute, nod) = match &self.days {
            None => {
                let budgeted: StorageReaderHandle = Arc::new(BudgetedStorageReader {
                    inner: self.storage.clone(),
                    budgets: budgets.clone(),
                });
                (
                    TributeRepositoryReader::new(budgeted.clone()),
                    outbe_nod::nod_reader(budgeted),
                )
            }
            Some(route) => {
                let wrap = reader_wrap(budgets.clone());
                (
                    TributeRepositoryReader::with_days(
                        route.durable_reader.clone(),
                        route.durable_writer.clone(),
                        route.databases.clone(),
                    )
                    .with_day_read_wrap(wrap.clone()),
                    outbe_nod::nod_reader(route.durable_reader.clone())
                        .with_days(route.durable_writer.clone(), route.databases.clone())
                        .with_day_read_wrap(wrap),
                )
            }
        };
        RuntimeBodyReaders {
            execution: ExecutionBodyReadSession {
                factory: self,
                tribute,
                nod,
                budgets,
            },
        }
    }
}

impl RuntimeBodyReaders {
    /// Creates a separate execution budget over the same storage backend.
    #[must_use]
    pub fn fork_execution(&self) -> Self {
        self.execution.factory.clone().build()
    }

    /// Applies the caller's remaining execution budget to every body read in this executor.
    #[must_use]
    pub fn enter_execution_budget(&self, budget: ExecutionReadBudget) -> ExecutionReadBudgetGuard {
        self.execution.budgets.enter(budget)
    }

    /// Identifies the cancelled request in this execution-local reader scope.
    pub fn cancelled_read_budget(&self) -> Option<ExecutionReadBudget> {
        self.execution
            .budgets
            .active
            .lock()
            .ok()?
            .values()
            .find(|budget| budget.is_cancelled())
            .cloned()
    }

    /// Returns the typed Tribute body reader.
    #[must_use]
    pub const fn tribute(&self) -> &TributeRepositoryReader {
        &self.execution.tribute
    }

    /// Returns the typed Nod item and bucket reader.
    #[must_use]
    pub const fn nod(&self) -> &NodRepositoryReader {
        &self.execution.nod
    }

    /// Reports a technical read failure without exposing readiness write authority to domains.
    pub fn report_unavailable(&self) {
        if let Some(sender) = &self.execution.factory.failure_sender {
            sender.send_if_modified(|current| match current {
                Some(RuntimeBodyFailure::Fatal(_)) => false,
                Some(RuntimeBodyFailure::Unavailable { generation, .. }) => {
                    let Some(next) = generation.checked_add(1) else {
                        *current = Some(RuntimeBodyFailure::Fatal(ProjectionFailure::new(
                            ProjectionFailureClass::Other,
                            "runtime-body outage generation overflow",
                        )));
                        return true;
                    };
                    *generation = next;
                    true
                }
                None => {
                    *current = Some(RuntimeBodyFailure::Unavailable {
                        generation: 1,
                        since: std::time::Instant::now(),
                    });
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
        if let Some(sender) = &self.execution.factory.failure_sender {
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
                tracing::warn!(
                    target: "offchain_read",
                    reason = %message,
                    "Body read failed. The precompile returns a revert."
                );
            }
            _ => {}
        }
    }
}

impl ParentBodySource for RuntimeBodyReaders {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        match entity {
            EntityRef::Tribute(tribute_id) => self
                .execution
                .tribute
                .get_stored_body(tribute_id)
                .map_err(map_tribute_parent_error),
            EntityRef::NodItem(nod_id) => self
                .execution
                .nod
                .get_stored_item(nod_id)
                .map_err(map_nod_parent_error),
            EntityRef::NodBucket(bucket_id) => self
                .execution
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
                .execution
                .tribute
                .list_ids_by_owner(owner, request)
                .map_err(map_tribute_parent_error),
            QueryRef::TributeByDay(worldwide_day) => self
                .execution
                .tribute
                .list_ids_by_day(worldwide_day, request)
                .map_err(map_tribute_parent_error),
            QueryRef::NodByOwner(owner) => self
                .execution
                .nod
                .list_ids_by_owner(owner, request)
                .map_err(map_nod_parent_error),
            QueryRef::NodAll => self
                .execution
                .nod
                .list_ids_all(request)
                .map_err(map_nod_parent_error),
        }
    }
}

fn map_storage_parent_error(kind: StorageErrorKind, message: String) -> ParentBodySourceError {
    match kind {
        StorageErrorKind::RequestDeadline => ParentBodySourceError::RequestDeadline(message),
        StorageErrorKind::Unavailable => ParentBodySourceError::Unavailable(message),
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
