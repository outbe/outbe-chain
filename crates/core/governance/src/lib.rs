//! Governance precompile: on-chain registry of the normative objects
//! (meta-canon, canon) and improvement proposals (OIP, GIP).
//!
//! - **Meta-canon / canon** - a single structured text each, versioned, with a
//!   keccak hash and a `version -> hash` revision map. Two operations: read and
//!   full-overwrite write. No status model.
//! - **OIP / GIP** - separate record types with independent id sequences. Each
//!   carries a small header (author, status, blocks, text hash) plus the
//!   proposal text in-record. Status lifecycle:
//!   `Draft -> Approved | Rejected | Rework`, `Rework -> Draft`,
//!   `Approved -> Implemented`.
//! - **diff** - unified diff of a proposal's text against the current canon or
//!   meta-canon (view-only, via `similar`).
//!
//! Writes to the normative texts and to proposal status are gated on the
//! caller being an ACTIVE validator in the live `ValidatorSet`, checked at call
//! time. This gate is PoC scaffolding in place of the decision pipeline, which
//! is not built yet.
//! The vote path ([`GovernanceVoteTarget`]) may also materialize Approved
//! OIP/GIP records after validator quorum.
//!
//! Submission rejects a full per-author index before writing a proposal.
//! Author listings paginate index slots and omit missing records rather than
//! returning fabricated proposal metadata.
//!
//! OIP/GIP operations share a private typed proposal store. Each record adapter
//! binds its own maps, indexes and event types; schema layouts and the public
//! API stay separate. Metadata reads and status changes access fixed-width
//! fields without loading proposal text.

pub mod diff;
pub mod errors;
pub mod precompile;
mod proposals;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod status;
pub mod vote_target;

pub use schema::{Gip, GovernanceContract, Oip};
pub use vote_target::{GovernanceVotePayload, GovernanceVoteTarget, ProposalKind};

#[cfg(test)]
mod tests;
