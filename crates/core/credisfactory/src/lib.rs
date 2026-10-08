//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `issueCredis` uses the source's pledge for a fixed reservation and records
//!   the source on the position.
//! - `settleCredis` applies interest-first payments and returns released collateral
//!   to the source's liquid balance.
//! - [`called`] is the daily Cycle-triggered price-path scan. It calls positions
//!   whose breach window filled. It also voids the remainder of called positions
//!   whose settlement window has lapsed, and burns the unpaid share of the
//!   collateral from the source into the Promis Reserve.

pub mod called;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
