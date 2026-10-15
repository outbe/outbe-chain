//! Intex: canonical, cross-chain Intex series ledger, the terms and profile a series is
//! issued under, and its call lifecycle data: the price bins and the called-group queue.
//!
//! IntexFactory runs the lifecycle and writes the registry through the Rust-to-Rust API
//! in [`api`]. The [`precompile`] surface is read-only.

pub mod api;
pub mod config;
pub mod constants;
pub mod errors;
pub mod precompile;
pub mod schema;
pub(crate) mod state;

pub use config::IntexParams;
pub use errors::IntexError;
pub use schema::{
    CreateSeriesParams, IntexCallTrigger, IntexContract, IntexState, SeriesId, SeriesRecord,
    SERIES_ID_LEN,
};
pub use state::{CallBins, ExpiryHours, Group};

#[cfg(test)]
mod tests;
