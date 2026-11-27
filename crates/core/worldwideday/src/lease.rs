//! The scoped-mutation leases under which a Worldwide Day opens or seals.

use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::metadosis_bindings::MetadosisStorageExt;

#[derive(Clone, Copy)]
enum DayLease {
    InitGenesis,
    MetadosisReady,
    AdvanceDue,
}

fn require_day_lease(storage: &StorageHandle<'_>, lease: DayLease) -> Result<()> {
    match lease {
        DayLease::InitGenesis => storage.require_metadosis_init_genesis(),
        DayLease::MetadosisReady => storage.require_metadosis_process_ready(),
        DayLease::AdvanceDue => storage.require_metadosis_advance_due(),
    }
}

/// Tries each caller binding. Only a lease identity mismatch selects the next
/// binding. Storage, borrow, and time errors return immediately.
fn accept_day_lease(storage: &StorageHandle<'_>, leases: &[DayLease]) -> Result<()> {
    let mut mismatch = None;
    for lease in leases {
        match require_day_lease(storage, *lease) {
            Ok(()) => return Ok(()),
            Err(error) if lease_identity_mismatch(&error) => mismatch = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(mismatch.unwrap_or_else(|| PrecompileError::Fatal("day lease list is empty".into())))
}

fn lease_identity_mismatch(error: &PrecompileError) -> bool {
    let PrecompileError::Revert(message) = error else {
        return false;
    };
    message == "test provider scoped mutation lease identity mismatch"
        || message == "scoped mutation frame identity mismatch"
}

/// Init-genesis, Metadosis-ready and advance-due open and seal worldwide days.
pub fn require_worldwide_day_lease(storage: &StorageHandle<'_>) -> Result<()> {
    accept_day_lease(
        storage,
        &[
            DayLease::InitGenesis,
            DayLease::MetadosisReady,
            DayLease::AdvanceDue,
        ],
    )
}
