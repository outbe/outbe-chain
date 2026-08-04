//! Module-structure standard layout:
//! - `schema.rs` — storage schema for the `ValidatorSet` facade.
//! - `state.rs` — `CommitteeSnapshotStore` helpers.
//! - `runtime.rs` — validator-set use-cases and the raw-storage adapter.
//! - `state_machine.rs` — typed validator lifecycle and coupled-field rules.
//! - `hooks.rs` — per-finalized-block guard wrappers.
//! - `precompile.rs` — ABI dispatch.
//! - `errors.rs` — module-local activation error type.
//!
//! `pub use` re-exports below preserve the old `contract` / `logic`
//! paths for external callers; migrate them opportunistically.
pub mod delegation;
pub mod errors;
pub mod hooks;
pub mod metrics;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod state_machine;

#[cfg(any(test, feature = "test-utils"))]
#[doc(hidden)]
pub mod test_support;

#[cfg(test)]
mod tests;

pub mod contract {
    pub use crate::schema::*;
}
pub mod logic {
    pub use crate::runtime::*;
}

pub use errors::ActivationError;
pub use runtime::{EpochSnapshot, ValidatorParticipation, ValidatorRecord};
pub use state::{
    clear_committee_snapshot, committee_set_hash_v2, committee_snapshot_key,
    next_vrf_material_version, read_committee_snapshot, read_committee_snapshot_for_epoch,
    snapshot_identity, write_committee_snapshot, CommitteeEntry, CommitteeSnapshot,
    COMMITTEE_SNAPSHOT_RETAIN_EPOCHS, OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN,
    OUTBE_COMMITTEE_SNAPSHOT_KEY_V2_DOMAIN, VRF_MATERIAL_VERSION_GENESIS,
};
pub use state_machine::{
    ActiveState, ExitingState, JailedState, P2pInfo, PendingState, StakeProjection,
    ValidatorHistory, ValidatorLifecycle, ValidatorState,
};
