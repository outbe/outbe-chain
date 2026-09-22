//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `issueCredis` consumes a confidential Gratis pledge-lock ticket (pledgeNote
//!   and spend authorization) via [`outbe_gratis`], opens an [`outbe_credis`]
//!   position bound to the smart account (storing the pledger EOA), crediting the
//!   collateral into the pledger's own pledged ledger, and pays reserved stablecoins
//!   to the CCA to cover native COEN delivered to the user's smart account.
//! - `settle` applies a payment interest-first and releases the principal-proportional
//!   share of collateral from the pledger's pledged ledger back to its balance.
//! - [`called`] is the daily Cycle-triggered price-path scan: it calls positions
//!   whose breach window filled and voids the remainder of called positions whose
//!   settlement window has lapsed, burning the unpaid share of the collateral into
//!   the Promis Reserve.

pub mod called;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
