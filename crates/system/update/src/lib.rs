//! `Update` - on-chain protocol version scheduling and activation.
//!
//! Vote owns proposal lifecycle; update stores scheduled upgrades and
//! activates them at `activationHeight` via begin-block processing.
//!
//! An approved `upgradeContracts` proposal upgrades our UUPS contracts on the
//! listed chains through `applyApproved` (see `vote_target::contract_upgrade`).

mod active_version;
pub mod api;
pub mod constants;
pub mod errors;
pub mod handlers;
pub mod lifecycle;
pub mod payload;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod sol_ext;
pub mod startup;
pub mod state;
pub mod version;
pub mod vote_target;

pub use handlers::{UpgradeHandler, UpgradeHandlerRegistry, UpgradeHandlers};
pub use payload::{
    decode_schedule_update_json, encode_schedule_update_json, validate_schedule_update_json,
    ContractUpgrade, RevokeEnclaveBindingPayloadV1, ScheduleUpdatePayload, UpdateVotePayload,
    UpgradeContractsPayload, REVOKE_ENCLAVE_BINDING_ACTION_V1, UPGRADE_CONTRACTS_OPERATION,
};
pub use schema::Update;
pub use state::ScheduledUpdateInfo;
pub use version::{encode_protocol_version, ProtocolVersion};
pub use vote_target::UpdateVoteTarget;

#[cfg(test)]
mod tests;
