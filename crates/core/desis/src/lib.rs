//! Desis: Intex auction/clearing engine on Outbe (demand side).
//!
//! Takes the Intex auction of each emission day by its date and the Desis limit from Metadosis,
//! collects the bid batches relayed by the target chains through the ERC-7786 hub, clears the
//! auction, and hands issuance to IntexFactory.

pub mod api;
mod cleanup;
pub mod constants;
pub mod errors;
pub mod outbox;
pub mod precompile;
pub(crate) mod runtime;
pub mod schema;
pub(crate) mod state;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

#[cfg(test)]
mod tests;

pub use cleanup::run_cleanup;
pub use errors::DesisError;
pub use outbox::DesisOutboxModule;
pub use runtime::{advance_intex_auctions, complete_intex_auctions};
pub use schema::{AuctionConfig, AuctionStage, ClearingResult, DesisContract, IntexAuctionBidData};
