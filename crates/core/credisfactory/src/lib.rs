//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `issueCredis` consumes an owner-bound pledge note for a fixed reservation,
//!   credits aggregate Credis collateral and stores the authenticated return serial.
//! - `settleCredis` applies interest-first payments and appends notes for released collateral.
//! - [`called`] is the daily Cycle-triggered price-path scan. It calls positions
//!   whose breach window filled. It also voids the remainder of called positions
//!   whose settlement window has lapsed, and burns the unpaid share of the
//!   collateral into the Promis Reserve.

pub mod called;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::CredisFactoryContract;

#[cfg(test)]
mod tests;
