//! `Update` - on-chain protocol version scheduling and activation.
//!
//! Vote owns V/R/A boundaries. Update reserves accepted plans without TEE side
//! effects during V/R, prepares enclave successors at T, and activates at H.
//! Live payloads omit `activationHeight`: primary `{version, info?, mrenclave?}`,
//! cancel `{cancelProposalId}`, replacement `{replaceProposalId, update}`.
//! Historical software schedules retain H; pending legacy enclave rollouts must
//! be resolved before coordinated binary cutover, never silently migrated.

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
pub mod startup;
pub mod state;
pub mod version;
pub mod vote_target;

pub use handlers::{UpgradeHandler, UpgradeHandlerRegistry, UpgradeHandlers};
pub use payload::{
    decode_schedule_update_json, encode_schedule_update_json, encode_update_plan_json,
    validate_schedule_update_json, ScheduleUpdatePayload, UpdateCancelPayload, UpdatePlanPayload,
    UpdateReplacementPayload, UpdateVoteAction,
};
pub use schema::Update;
pub use state::ScheduledUpdateInfo;
pub use version::{encode_protocol_version, ProtocolVersion};
pub use vote_target::UpdateVoteTarget;

#[cfg(test)]
mod tests;
