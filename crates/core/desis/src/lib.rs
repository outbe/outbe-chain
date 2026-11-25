//! Desis: Intex auction/clearing engine on Outbe (demand side).
//!
//! Drives the auction schedule from a Metadosis brief, collects the bid batches
//! relayed by the target chains through the ERC-7786 hub, clears the auction, and hands
//! issuance to IntexFactory.

pub mod api;
mod cleanup;
pub mod constants;
pub mod errors;
pub mod outbox;
pub mod precompile;
pub(crate) mod runtime;
pub mod schema;
pub(crate) mod state;

#[cfg(test)]
mod tests;

pub use cleanup::run_cleanup;
pub use errors::DesisError;
pub use outbox::DesisOutboxModule;
pub use runtime::{advance_auctions, complete_auctions};
pub use schema::{AuctionConfig, AuctionStage, ClearingResult, DesisContract, IntexAuctionBidData};
