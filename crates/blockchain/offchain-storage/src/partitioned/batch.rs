//! Ordered scope-addressed mutations. Physical transactions belong to adapters.

use super::{PartitionedStorage, StorageScope};
use crate::{AtomicWriteBatch, AtomicWriteOperation, StorageError, StorageWriter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionedOperation {
    pub scope: StorageScope,
    pub operation: AtomicWriteOperation,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PartitionedBatch {
    pub operations: Vec<PartitionedOperation>,
    pub retired_scopes: Vec<StorageScope>,
}

impl PartitionedBatch {
    pub fn validate(&self) -> Result<(), StorageError> {
        for scope in &self.retired_scopes {
            scope.validate()?;
            if matches!(scope.partition, super::PartitionId::Shared) {
                return Err(StorageError::InvalidArgument(
                    "shared scopes cannot be retired".into(),
                ));
            }
        }
        for operation in &self.operations {
            StorageScope::new(&operation.scope.domain, operation.scope.partition.clone())?;
        }
        let mut logical = AtomicWriteBatch::from_operations(
            self.operations
                .iter()
                .map(|op| op.operation.clone())
                .collect(),
        );
        for scope in &self.retired_scopes {
            logical.retire_scope(scope.clone());
        }
        logical.validate()
    }
}

impl StorageWriter for PartitionedStorage {
    fn verify_transaction_capability(&self) -> Result<(), StorageError> {
        self.writer
            .as_ref()
            .ok_or_else(|| StorageError::InvalidArgument("read-only partition session".into()))?
            .verify_write_capability()
    }

    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        batch.validate()?;
        let _guard = self.gate.write();
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| StorageError::InvalidArgument("read-only partition session".into()))?;
        let mut routed = PartitionedBatch {
            retired_scopes: batch.retired_scopes().to_vec(),
            ..Default::default()
        };
        for operation in batch.operations() {
            let (namespace, key) = match operation {
                AtomicWriteOperation::Put { namespace, key, .. }
                | AtomicWriteOperation::Delete { namespace, key } => (namespace, key),
            };
            let scope = match namespace.scope() {
                Some(scope) => Some(scope.clone()),
                None => self
                    .routing
                    .point(namespace, key, self.source.as_ref())?
                    .map(|location| location.scope),
            };
            match scope {
                Some(scope) => routed.operations.push(PartitionedOperation {
                    scope,
                    operation: operation.clone(),
                }),
                None if matches!(operation, AtomicWriteOperation::Delete { .. }) => {}
                None => {
                    return Err(StorageError::InvalidArgument(
                        "write requires an explicit entity partition".into(),
                    ))
                }
            }
        }
        routed.validate()?;
        writer.commit(&routed)
    }
}
