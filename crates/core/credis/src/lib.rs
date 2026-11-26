//! Credis: the terms, profile and records of a Credis, its sub-series and call groups. The call bins,
//! the close queue and the cleanup queue live here, with the read-time state, accrued
//! fee and outcome.
//!
//! CredisFactory applies every rule and runs the call and close sweeps. The precompile
//! surface is the Credis NFT and its views.

pub mod config;
pub mod constants;
pub mod errors;
mod metadata;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod state;

pub use runtime::{outcome, settlement_deadline, Outcome};
pub use schema::{CallTerms, Credis, CredisContract, CredisState, CredisView, SubSeriesSummary};
pub use state::{CallBins, CleanupQueue, CloseQueue};

#[cfg(test)]
mod tests;
