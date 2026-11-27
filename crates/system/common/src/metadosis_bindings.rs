//! Metadosis command bindings and storage lease assertions built on `outbe_primitives::storage`.

use alloy_primitives::B256;
use outbe_primitives::{
    error::Result,
    storage::{scoped_block_binding, MutationPurposeTag, StorageHandle},
};

#[must_use]
pub fn metadosis_init_genesis_binding(chain_id: u64, block_number: u64, timestamp: u64) -> B256 {
    scoped_block_binding(
        b"METADOSIS_CREATE_DAY_V1",
        chain_id,
        block_number,
        timestamp,
    )
}

#[must_use]
pub fn metadosis_process_ready_binding(chain_id: u64, block_number: u64, timestamp: u64) -> B256 {
    scoped_block_binding(
        b"METADOSIS_PROCESS_READY_V1",
        chain_id,
        block_number,
        timestamp,
    )
}

#[must_use]
pub fn metadosis_advance_due_binding(chain_id: u64, block_number: u64, timestamp: u64) -> B256 {
    scoped_block_binding(
        b"METADOSIS_ADVANCE_DUE_V1",
        chain_id,
        block_number,
        timestamp,
    )
}

/// Domain extension methods on [`StorageHandle`] for verifying active Metadosis lifecycle leases.
pub trait MetadosisStorageExt {
    fn require_metadosis_process_ready(&self) -> Result<()>;
    fn require_metadosis_init_genesis(&self) -> Result<()>;
    fn require_metadosis_advance_due(&self) -> Result<()>;
}

impl MetadosisStorageExt for StorageHandle<'_> {
    fn require_metadosis_process_ready(&self) -> Result<()> {
        require_cycle_lifecycle(self, metadosis_process_ready_binding)
    }

    fn require_metadosis_init_genesis(&self) -> Result<()> {
        require_cycle_lifecycle(self, metadosis_init_genesis_binding)
    }

    fn require_metadosis_advance_due(&self) -> Result<()> {
        require_cycle_lifecycle(self, metadosis_advance_due_binding)
    }
}

fn require_cycle_lifecycle(
    storage: &StorageHandle<'_>,
    binding_for: fn(u64, u64, u64) -> B256,
) -> Result<()> {
    let chain_id = storage.chain_id()?;
    let block_number = storage.block_number()?;
    let binding = binding_for(chain_id, block_number, storage.now_secs()?);
    storage.require_scoped_mutation_frame(MutationPurposeTag::CycleLifecycle, binding)
}
