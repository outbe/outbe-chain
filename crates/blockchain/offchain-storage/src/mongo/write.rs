//! Ordered MongoDB transaction and writer lease fencing.
use super::*;

impl StorageWriter for MongoStorage {
    fn verify_transaction_capability(&self) -> Result<(), StorageError> {
        MongoStorage::verify_acknowledged_transaction(self)
    }

    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.apply_atomic_clearing(batch, &[])
    }
    fn apply_atomic_clearing(
        &self,
        batch: &AtomicWriteBatch,
        namespaces: &[Namespace],
    ) -> Result<(), StorageError> {
        batch.validate()?;
        if !batch.retired_scopes().is_empty() {
            return Err(StorageError::InvalidArgument(
                "raw Mongo requires a partition adapter for scope retirement".into(),
            ));
        }
        if batch.is_empty() && namespaces.is_empty() {
            return Ok(());
        }
        let writer_lease = self.writer_lease.lock().clone();
        if writer_lease
            .as_ref()
            .is_some_and(|lease| lease.lost.load(Ordering::Acquire))
        {
            return Err(StorageError::WriterLeaseLost);
        }
        let operations = batch
            .operations()
            .iter()
            .map(PreparedMongoOperation::try_from)
            .collect::<Result<Vec<_>, _>>()?;
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
                if let Some(lease) = &writer_lease {
                    let result = writer_lease_collection(&self.database)
                        .update_one(
                            doc! { "_id": WRITER_LEASE_ID, "owner": &lease.owner },
                            writer_lease_update(&lease.owner),
                        )
                        .session(&mut *session)
                        .run()?;
                    if result.matched_count != 1 {
                        lease.lost.store(true, Ordering::Release);
                        return Err(MongoError::custom(WriterLeaseLost));
                    }
                }
                for operation in &operations {
                    match operation {
                        PreparedMongoOperation::Put {
                            namespace,
                            encoded_key,
                            document,
                        } => {
                            self.collection(namespace)
                                .replace_one(doc! { "_id": encoded_key }, document.clone())
                                .upsert(true)
                                .collation(simple_binary_collation())
                                .session(&mut *session)
                                .run()?;
                        }
                        PreparedMongoOperation::Delete {
                            namespace,
                            encoded_key,
                        } => {
                            self.collection(namespace)
                                .delete_one(doc! { "_id": encoded_key })
                                .collation(simple_binary_collation())
                                .session(&mut *session)
                                .run()?;
                        }
                    }
                }
                for namespace in namespaces {
                    self.collection(namespace)
                        .delete_many(doc! {})
                        .session(&mut *session)
                        .run()?;
                }
                Ok(())
            })
            .map_err(map_operation_error)
    }
}
