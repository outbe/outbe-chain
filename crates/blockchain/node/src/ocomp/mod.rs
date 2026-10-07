//! OCOMP retention, finalized-input authority, and fork-install loading.
//!
//! Most of this module is node-local. It decides whether this validator has
//! enough durable, authenticated input to advertise, vote for, export, execute,
//! or sign one PoC job.
//!
//! Two parts are consensus-visible and must stay deterministic across validators:
//! - [`finality::ProductionOcompFinalizedIntentAuthority`] is the verifier that
//!   EVM precompile dispatch uses during block execution.
//! - [`fork`] loads the genesis OCOMP install that sets lifecycle activation.

pub mod finality;
pub mod fork;
pub mod local_result;
mod openings;
pub mod retention;
pub use openings::{build_lysis_openings, build_public_lysis_openings, verify_lysis_openings};

#[cfg(test)]
mod tests;
