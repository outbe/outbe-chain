//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `issueCredis` uses the source's pledge for a fixed reservation and records
//!   the source on the position.
//! - `settleCredis` applies interest-first payments and returns released collateral
//!   to the source's liquid balance.
//! - [`called`] is the daily Cycle-triggered price-path scan. It calls positions
//!   whose breach window filled.
//! - [`expired`] voids called positions after their settlement window.
//!   It burns unpaid collateral from the source's pledged balance into the Promis Reserve.

pub mod called;
pub mod errors;
pub mod expired;
pub mod hooks;
pub mod precompile;
pub mod runtime;
mod sol_ext;

#[cfg(test)]
mod tests;
