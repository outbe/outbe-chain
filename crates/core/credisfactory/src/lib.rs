//! Credis factory precompile (`0x1009`).
//!
//! `issueCredis` consumes a private PledgeNote, opens a position with an opaque
//! collateral handle, and delivers reserved stablecoins plus the CCA's COEN stake.
//! `settle` repays interest first and releases proportional collateral inside the
//! enclave. [`called`] scans prices and forfeits unpaid collateral after notice.

pub mod called;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
