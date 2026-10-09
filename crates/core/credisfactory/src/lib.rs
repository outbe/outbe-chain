//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `issueCredis` uses the source's pledge for a fixed reservation, reads the call
//!   anchor and the policy rate at issuance, and records the source on the Credis.
//! - `settleCredis` applies interest-first payments and returns released collateral
//!   to the source's liquid balance.
//! - [`called`] is the daily Cycle-triggered price-path scan. It calls Credis
//!   whose breach window filled.
//! - [`expired`] forfeits called Credis after their settlement window.
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
