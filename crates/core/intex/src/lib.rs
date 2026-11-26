//! Intex: canonical, cross-chain Intex series ledger, the terms and profile a series is
//! issued under, and its call lifecycle data: the price bins, the called-group queue and
//! the forfeit queue of expired groups.
//!
//! IntexFactory runs the lifecycle and writes the registry through the Rust-to-Rust API
//! in [`api`]. The [`precompile`] surface is read-only: the series views and the Settled
//! Intex ERC-1155 collection.

pub mod api;
pub mod config;
pub mod constants;
pub mod errors;
mod metadata;
pub mod precompile;
pub mod schema;
pub(crate) mod state;

pub use config::IntexParams;
pub use errors::IntexError;
pub use schema::{
    intex_id, CallStamp, CreateSeriesParams, IntexCallTrigger, IntexContract, IntexState, SeriesId,
    SeriesRecord, SERIES_ID_LEN,
};
pub use state::{CallBins, CloseQueue, ForfeitQueue};

#[cfg(test)]
mod tests;
