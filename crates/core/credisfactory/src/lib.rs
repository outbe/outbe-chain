//! Credis factory precompile (`0x1009`). Orchestrates the credis lifecycle on
//! top of the confidential Gratis token:
//!
//! - `issueCredis` uses the source's pledge for a fixed reservation, copies its pledge
//!   and call terms, and records the source on the Credis.
//! - `settleCredis` applies interest-first payments and returns released collateral
//!   to the source's liquid balance.
//! - `forfeitCredis` lets the owner give up an Issued or Called Credis. Its
//!   collateral burns as in an expired one.
//! - [`called`] is the daily Cycle-triggered price-path scan. It calls Credis
//!   whose breach window filled.
//! - [`expired`] forfeits called Credis after their settlement window.
//!   It burns unpaid collateral from the source's pledged balance into the Promis Reserve.

pub mod called;
pub mod errors;
pub mod expired;
pub mod hooks;
pub mod precompile;
pub mod rules;
pub mod runtime;
mod sol_ext;
#[cfg(feature = "e2e-test")]
mod test_arming_e2e;

#[cfg(test)]
mod tests;
