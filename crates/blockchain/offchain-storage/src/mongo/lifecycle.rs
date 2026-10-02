use crate::{MongoWriterLease, StorageError, StorageLifecycle};

pub(crate) struct MongoLifecycle(pub(crate) Option<MongoWriterLease>);
impl StorageLifecycle for MongoLifecycle {
    fn activate(&self) -> Result<(), StorageError> {
        if self.0.as_ref().expect("live ownership").is_valid() {
            Ok(())
        } else {
            Err(StorageError::WriterLeaseLost)
        }
    }
    fn close(&mut self) -> Result<(), StorageError> {
        if let Some(lease) = self.0.as_mut() {
            if let Some(stop) = lease.stop.take() {
                let _ = stop.send(());
            }
            // This runs on the session cleanup worker, never on node's shutdown thread.
            if let Some(renewer) = lease.renewer.take() {
                renewer.join().map_err(|_| {
                    StorageError::backend(std::io::Error::other("Mongo lease renewer panicked"))
                })?;
            }
            super::release_writer_lease(&lease.storage.database, &lease.owner)?;
            let mut binding = lease.storage.writer_lease.lock();
            if binding
                .as_ref()
                .is_some_and(|binding| binding.owner == lease.owner)
            {
                binding.take();
            }
            lease.released = true;
        }
        self.0.take();
        Ok(())
    }
}
