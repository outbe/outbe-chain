pub(crate) mod lifecycle;
use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

use mongodb::{
    bson::{doc, oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Document},
    error::{Error as MongoError, ErrorKind as MongoErrorKind, WriteFailure},
    options::{
        Acknowledgment, ClientOptions, Collation, DatabaseOptions, ReadConcern, ReadPreference,
        SelectionCriteria, WriteConcern,
    },
    sync::{Client, Collection, Database},
};

use crate::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanEntry, ScanPage, ScanRequest,
    StorageError, StorageErrorKind, StorageMetadata, StorageReader, StorageWriter, StoredValue,
    Value, MAX_SCAN_PAGE_VALUE_BYTES,
};

const EXECUTION_READ_TIMEOUT: Duration = Duration::from_secs(1);
const WRITER_LEASE_DURATION: Duration = Duration::from_secs(5);
const WRITER_LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(2);
const WRITER_LEASE_COLLECTION: &str = "projection_writer_lease";
const WRITER_LEASE_ID: &str = "active_projection_writer";

const WRITE_CONCERN_FAILED_CODE: i32 = 64;
const READ_CONCERN_MAJORITY_NOT_AVAILABLE_CODE: i32 = 134;
const MAX_TIME_MS_EXPIRED_CODE: i32 = 50;
const RETRYABLE_READ_CODES: &[i32] = &[
    6,
    7,
    89,
    91,
    189,
    262,
    9001,
    10_107,
    11_600,
    11_602,
    13_435,
    13_436,
    READ_CONCERN_MAJORITY_NOT_AVAILABLE_CODE,
];

/// Connection settings for the persistent adapter.
#[derive(Clone, Eq, PartialEq)]
pub struct MongoStorageConfig {
    /// MongoDB connection string.
    ///
    /// Read/write consistency options may be omitted or set to the required
    /// primary/majority contract. Conflicting URI options are rejected.
    pub uri: String,
    /// Database containing the namespace collections.
    pub database: String,
}

impl fmt::Debug for MongoStorageConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoStorageConfig")
            .field("uri", &"<redacted>")
            .field("database", &self.database)
            .finish()
    }
}

/// Persistent MongoDB storage adapter.
pub struct MongoStorage {
    client: Client,
    database: Database,
    writer_lease: parking_lot::Mutex<Option<WriterLeaseBinding>>,
}

#[derive(Clone)]
struct WriterLeaseBinding {
    owner: String,
    lost: Arc<AtomicBool>,
}

/// Process-lifetime ownership of the sole projection writer for one database.
pub struct MongoWriterLease {
    storage: Arc<MongoStorage>,
    owner: String,
    lost: Arc<AtomicBool>,
    stop: Option<mpsc::Sender<()>>,
    renewer: Option<JoinHandle<()>>,
    released: bool,
}

impl fmt::Debug for MongoStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoStorage")
            .field("database", &self.database.name())
            .finish_non_exhaustive()
    }
}

impl MongoStorage {
    /// Connects to the configured database.
    pub fn connect(config: MongoStorageConfig) -> Result<Self, StorageError> {
        let mut options = ClientOptions::parse(&config.uri)
            .run()
            .map_err(map_configuration_error)?;
        cap_execution_timeouts(&mut options);
        let client = Client::with_options(options).map_err(map_configuration_error)?;
        if !matches!(
            client.selection_criteria(),
            None | Some(SelectionCriteria::ReadPreference(ReadPreference::Primary))
        ) {
            return Err(StorageError::invalid_argument(
                "MongoDB execution storage requires primary read preference",
            ));
        }
        if client
            .read_concern()
            .is_some_and(|concern| concern != &ReadConcern::majority())
        {
            return Err(StorageError::invalid_argument(
                "MongoDB execution storage requires majority read concern",
            ));
        }
        if client
            .write_concern()
            .is_some_and(|concern| !matches!(concern.w.as_ref(), Some(Acknowledgment::Majority)))
        {
            return Err(StorageError::invalid_argument(
                "MongoDB execution storage requires majority write concern",
            ));
        }
        let database = client.database_with_options(&config.database, execution_database_options());
        Ok(Self {
            client,
            database,
            writer_lease: parking_lot::Mutex::new(None),
        })
    }

    fn collection(&self, namespace: &Namespace) -> Collection<Document> {
        self.database.collection(namespace.as_str())
    }

    pub(crate) fn namespace_names(&self) -> Result<Vec<String>, StorageError> {
        self.database
            .list_collection_names()
            .run()
            .map_err(map_operation_error)
    }

    pub(crate) fn reject_legacy_entity_layout(&self) -> Result<(), StorageError> {
        if self
            .namespace_names()?
            .iter()
            .any(|name| name != WRITER_LEASE_COLLECTION && !name.contains("__"))
        {
            return Err(StorageError::Corruption("unscoped MongoDB layout: use a fresh database for schema 3; automatic migration is disabled".into()));
        }
        Ok(())
    }

    /// Verifies that the server exposes sessions and a transaction-capable topology.
    pub fn verify_transaction_support(&self) -> Result<(), StorageError> {
        self.client
            .start_session()
            .run()
            .map_err(map_operation_error)?;
        let hello = self
            .client
            .database("admin")
            .run_command(doc! { "hello": 1 })
            .run()
            .map_err(map_operation_error)?;
        if !transaction_topology_supported(&hello) {
            return Err(StorageError::invalid_argument(
                "MongoDB projector requires a replica set or sharded transaction-capable topology",
            ));
        }
        Ok(())
    }

    /// Proves recovery with a server-acknowledged operation inside a transaction.
    ///
    /// The projection state collection is guaranteed to exist after projector
    /// startup. The impossible filter keeps this probe side-effect free while
    /// still forcing the driver to start and commit a real transaction.
    pub fn verify_acknowledged_transaction(&self) -> Result<(), StorageError> {
        let mut session = self
            .client
            .start_session()
            .run()
            .map_err(map_operation_error)?;
        session
            .start_transaction()
            .selection_criteria(primary_selection())
            .read_concern(ReadConcern::majority())
            .write_concern(majority_write_concern())
            .max_commit_time(EXECUTION_READ_TIMEOUT)
            .and_run(|session| {
                self.database
                    .collection::<Document>(WRITER_LEASE_COLLECTION)
                    .update_one(
                        doc! { "_id": { "$exists": false } },
                        doc! { "$set": { "_outbe_transaction_probe": true } },
                    )
                    .upsert(false)
                    .session(&mut *session)
                    .run()?;
                Ok(())
            })
            .map_err(map_operation_error)
    }

    /// Atomically acquires the database's single active projection-writer lease.
    pub fn acquire_writer_lease(self: &Arc<Self>) -> Result<MongoWriterLease, StorageError> {
        let mut binding = self.writer_lease.lock();
        if binding.is_some() {
            return Err(StorageError::invalid_argument(
                "this MongoDB adapter already owns a projection writer lease",
            ));
        }

        let owner = ObjectId::new().to_hex();
        acquire_writer_lease(&self.database, &owner)?;
        let lost = Arc::new(AtomicBool::new(false));
        *binding = Some(WriterLeaseBinding {
            owner: owner.clone(),
            lost: lost.clone(),
        });
        drop(binding);

        let (stop_tx, stop_rx) = mpsc::channel();
        let database = self.database.clone();
        let renew_owner = owner.clone();
        let renew_lost = lost.clone();
        let renewer = match std::thread::Builder::new()
            .name("offchain-writer-lease".to_owned())
            .spawn(move || loop {
                match stop_rx.recv_timeout(WRITER_LEASE_RENEW_INTERVAL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                match renew_writer_lease(&database, &renew_owner) {
                    Ok(true) => {}
                    Ok(false) => {
                        renew_lost.store(true, Ordering::Release);
                        break;
                    }
                    Err(error) if error.kind() == StorageErrorKind::Unavailable => {}
                    Err(_) => {
                        renew_lost.store(true, Ordering::Release);
                        break;
                    }
                }
            }) {
            Ok(renewer) => renewer,
            Err(error) => {
                self.writer_lease.lock().take();
                release_writer_lease_detached(self.database.clone(), owner);
                return Err(StorageError::unavailable(error));
            }
        };

        Ok(MongoWriterLease {
            storage: self.clone(),
            owner,
            lost,
            stop: Some(stop_tx),
            renewer: Some(renewer),
            released: false,
        })
    }
}

impl Drop for MongoWriterLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Detach both Mongo operations: a stalled network syscall in the
        // driver must not delay whole-node shutdown. Owner-qualified cleanup
        // is safe to race with an already in-flight, non-upserting renewal.
        drop(self.renewer.take());
        let mut binding = self.storage.writer_lease.lock();
        if binding
            .as_ref()
            .is_some_and(|lease| lease.owner == self.owner)
        {
            binding.take();
        }
        drop(binding);
        release_writer_lease_detached(self.storage.database.clone(), self.owner.clone());
    }
}

impl MongoWriterLease {
    /// Returns false after another writer has replaced this expired lease.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.lost.load(Ordering::Acquire)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("MongoDB projection writer lease was lost")]
struct WriterLeaseLost;

#[derive(Debug, thiserror::Error)]
#[error("MongoDB projection database already has an active writer")]
struct WriterLeaseBusy;

fn writer_lease_duration_millis() -> i64 {
    i64::try_from(WRITER_LEASE_DURATION.as_millis()).unwrap_or(i64::MAX)
}

fn writer_lease_update(owner: &str) -> Vec<Document> {
    vec![doc! {
        "$set": {
            "owner": owner,
            "lease_until": {
                "$dateAdd": {
                    "startDate": "$$NOW",
                    "unit": "millisecond",
                    "amount": writer_lease_duration_millis(),
                }
            }
        }
    }]
}

fn writer_lease_collection(database: &Database) -> Collection<Document> {
    database.collection(WRITER_LEASE_COLLECTION)
}

fn acquire_writer_lease(database: &Database, owner: &str) -> Result<(), StorageError> {
    let collection = writer_lease_collection(database);
    match collection
        .insert_one(doc! {
            "_id": WRITER_LEASE_ID,
            "owner": "",
            "lease_until": DateTime::from_millis(0),
        })
        .run()
    {
        Ok(_) => {}
        Err(error) if is_duplicate_key_error(&error) => {}
        Err(error) => return Err(map_operation_error(error)),
    }
    let result = collection
        .update_one(
            doc! {
                "_id": WRITER_LEASE_ID,
                "$or": [
                    { "owner": owner },
                    { "$expr": { "$lte": ["$lease_until", "$$NOW"] } },
                ],
            },
            writer_lease_update(owner),
        )
        .run();
    match result {
        Ok(result) if result.matched_count == 1 || result.upserted_id.is_some() => Ok(()),
        Ok(_) => Err(StorageError::unavailable(WriterLeaseBusy)),
        Err(error) if is_duplicate_key_error(&error) => {
            Err(StorageError::unavailable(WriterLeaseBusy))
        }
        Err(error) => Err(map_operation_error(error)),
    }
}

fn renew_writer_lease(database: &Database, owner: &str) -> Result<bool, StorageError> {
    writer_lease_collection(database)
        .update_one(
            doc! { "_id": WRITER_LEASE_ID, "owner": owner },
            writer_lease_update(owner),
        )
        .run()
        .map(|result| result.matched_count == 1)
        .map_err(map_operation_error)
}

fn release_writer_lease(database: &Database, owner: &str) -> Result<(), StorageError> {
    writer_lease_collection(database)
        .delete_one(doc! { "_id": WRITER_LEASE_ID, "owner": owner })
        .run()
        .map(|_| ())
        .map_err(map_operation_error)
}

fn release_writer_lease_detached(database: Database, owner: String) {
    let _ = std::thread::Builder::new()
        .name("offchain-writer-release".to_owned())
        .spawn(move || release_writer_lease(&database, &owner));
}

fn is_duplicate_key_error(error: &MongoError) -> bool {
    match error.kind.as_ref() {
        MongoErrorKind::Command(command) => command.code == 11_000,
        MongoErrorKind::Write(WriteFailure::WriteError(write)) => write.code == 11_000,
        _ => false,
    }
}

fn cap_execution_timeouts(options: &mut ClientOptions) {
    options.server_selection_timeout = Some(
        options
            .server_selection_timeout
            .unwrap_or(EXECUTION_READ_TIMEOUT)
            .min(EXECUTION_READ_TIMEOUT),
    );
    options.connect_timeout = Some(
        options
            .connect_timeout
            .unwrap_or(EXECUTION_READ_TIMEOUT)
            .min(EXECUTION_READ_TIMEOUT),
    );
}

fn transaction_topology_supported(hello: &Document) -> bool {
    let has_sessions = hello.contains_key("logicalSessionTimeoutMinutes");
    let replica_set = hello.get_str("setName").is_ok();
    let sharded = hello
        .get_str("msg")
        .is_ok_and(|message| message == "isdbgrid");
    has_sessions && (replica_set || sharded)
}

mod codec;
mod read;
mod write;
use codec::*;

#[cfg(test)]
mod tests;
