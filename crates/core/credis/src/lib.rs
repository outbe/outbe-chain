//! Credis: the terms, profile and records of a Credis. The call bins and the expiry queue
//! live here, with the read-time state, accrued interest and outcome.
//!
//! CredisFactory applies every rule and runs the call and forfeit sweeps. The precompile
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
pub use schema::{Credis, CredisContract, CredisState};
pub use state::{CallBins, ExpiryHours};

#[cfg(test)]
mod tests;
