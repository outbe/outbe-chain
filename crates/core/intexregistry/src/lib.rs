//! IntexRegistry: canonical, cross-chain Intex series ledger.
//!
//! Owns the per-series identity parameters and lifecycle status that previously
//! lived on the Origin `IntexNFT1155.SeriesData` struct. IntexFactory writes and
//! reads the registry through the Rust-to-Rust API in [`api`]. The [`precompile`]
//! surface is read-only — it exposes series data for off-chain observability;
//! all writes stay Rust-to-Rust.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod schema;
pub(crate) mod state;

pub use errors::IntexRegistryError;
pub use schema::{
    CreateSeriesParams, IntexCallTrigger, IntexRegistryContract, IntexState, SeriesRecord,
};

#[cfg(test)]
mod tests;
