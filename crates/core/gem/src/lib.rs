//! Gem: the terms, profile and data of a Gem. Bodies, groups, the call bins and the
//! expiry queue live here, with the read-time state.
//!
//! GemFactory applies every rule and runs the call and forfeit sweeps through [`api`].
//! The precompile surface is the Gem NFT and its views.

pub mod api;
pub mod config;
pub mod constants;
pub mod errors;
pub mod precompile;
pub mod schema;
pub mod state_api;
pub mod storage_records;

pub(crate) mod metadata;
pub(crate) mod state;

pub use api::{LoadedGem, LoadedGemGroup};
pub use config::GemParams;
pub use errors::{GemError, GemFatalError};
pub use schema::{
    CallTerms, GemAddParams, GemContract, GemGroupState, GemItemState, GemState, GemView,
};
pub use state::{CallBins, CleanupDays, ExpiryDays};

#[cfg(test)]
mod tests;
