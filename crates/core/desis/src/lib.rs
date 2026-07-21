//! Desis: Intex auction/clearing engine on Outbe (demand side).
//!
//! Receives bid batches from the target chains via OriginRouter, runs the three-stage
//! auction lifecycle (Start → Reveal → Clearing), clears the auction, and
//! hands issuance to IntexFactory.

pub mod api;
pub mod constants;
pub mod errors;
pub mod hooks;
pub mod precompile;
pub(crate) mod runtime;
pub mod schema;
pub(crate) mod sol_ext;
pub(crate) mod state;

#[cfg(test)]
mod tests;

pub use errors::DesisError;
pub use hooks::DesisLifecycle;
pub use schema::{AuctionConfig, AuctionStage, BidData, ClearingResult, DesisContract};
