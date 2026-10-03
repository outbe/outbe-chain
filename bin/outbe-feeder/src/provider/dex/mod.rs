//! Finalized pool spot prices and independent rolling base-token swap volumes.
mod abi;
mod config;
pub(crate) mod math;
mod pool;
mod worker;

pub(crate) use config::{validate_config, DexProviderConfig};
pub(crate) use worker::DexProvider;

#[cfg(test)]
mod tests;
