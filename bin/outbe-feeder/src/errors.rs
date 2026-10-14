//! Strictly typed errors for the price feeder daemon.
//!
//! Each submodule defines the error types for one feeder component.

mod aggregator;
mod cex;
mod config;
mod dex;
mod journal;
mod oracle;
mod provider;

pub(crate) use aggregator::AggregatorError;
pub(crate) use cex::CexError;
pub(crate) use config::ConfigError;
pub(crate) use dex::DexError;
pub(crate) use journal::JournalError;
pub(crate) use oracle::{FeederError, OracleError, VoteDecodeError};
pub(crate) use provider::{MockHttpError, OnchainFeedError, ProviderError, RedstoneError};

/// Returns `Ok(())` when `condition` is true, or `Err(error.into())` when false.
pub(crate) fn check<E: Into<eyre::Report>>(condition: bool, error: E) -> eyre::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(error.into())
    }
}
