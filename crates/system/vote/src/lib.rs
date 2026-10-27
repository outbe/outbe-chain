//! `Vote` - reusable on-chain proposal/voting module (`0x...EE0C`).
//!
//! Module-structure layout:
//! - `schema.rs` - storage schema and records.
//! - `state.rs` - proposal/vote CRUD and indexes.
//! - `runtime.rs` - validator-gated proposal/voting logic.
//! - `precompile.rs` - live ABI dispatch for proposals and votes.
//! - `lifecycle.rs` - begin-block tally entrypoint.
//! - `events.rs` - domain event payloads used by runtime/precompile wiring.
//!
//! # Live electorate
//!
//! Admission stores the target's electorate policy, not a membership snapshot.
//! Cast eligibility is checked against the policy's current eligible group.
//! At finalization, after the inclusive voting deadline, the tally and quorum
//! denominator use the same live eligible group. Stored ballots from validators
//! no longer in that group are not counted; eligible late joiners may vote while
//! the voting window remains open. There is no creation-time membership or count
//! snapshot.
//!
//! Proposal views recalculate the tally against the current group, even after
//! finalization. The terminal status and finalization event record the decision
//! made at finalization; a later view does not change that decision.
//!
//! # Opt-in three-window plans
//!
//! `VoteTarget::lifecycle` preserves Standard timing and bond dispatch by default.
//! Three-window validator-only targets admit primary ballots through C+V, register
//! accepted plans at Q=C+V+1, and collect independent linked ballots through T-1.
//! At T=Q+R, the lowest-id quorum challenger shadows the parent; higher-id
//! siblings are Canceled, lower-id non-quorum siblings Expired. Surviving plans
//! prepare at T and the target reports actual completion at H=T+A.
//! Appended sidecars and a bounded active-plan index preserve legacy record slots.
//! Legacy software plans are adopted only through the owning target's callback,
//! without changing their original voting deadline or activation height.

mod abi;

pub mod api;
pub mod constants;
pub mod errors;
pub mod events;
pub mod handlers;
pub mod lifecycle;
pub mod notify;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod windows;

pub use handlers::{ProposalAction, ProposalLifecycle, VotePlanContext};
pub use handlers::{VoteTarget, VoteTargetHandlers, VoteTargetRegistry};
pub use schema::Vote;
pub use state::{
    active_validator_addresses, calculate_vote_tally, ProposalInfo, ProposalStatus,
    ProposalSubmission, VoteInfo, VoteKind, VoteTally,
};
pub use windows::{ProposalKind, ProposalLifecycleInfo, ProposalPhase};

#[cfg(test)]
mod tests;
