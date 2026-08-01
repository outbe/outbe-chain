//! Node-local OCOMP retention and finalized-input authority.
//!
//! Nothing in this module changes block validity. It decides only whether this
//! validator has enough durable, authenticated input to advertise, vote for,
//! export, execute, or sign one PoC job.

pub(crate) mod attestation;
pub mod control;
pub mod finality;
pub mod fork;
pub mod measurement;
mod openings;
pub mod retention;
pub mod service;
pub(crate) mod sign_once;
pub(crate) mod signer;
pub mod snapshot_control;

pub use attestation::{CurrentHeightSource, ProviderHeightSource};
pub use openings::{build_lysis_openings, verify_lysis_openings};

#[cfg(test)]
mod tests;
