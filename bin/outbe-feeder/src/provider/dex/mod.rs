//! Pool spot prices read a few blocks behind the head and independent rolling base-token swap volumes.
mod abi;
mod config;
mod math;
mod pool;
mod rpc;
mod worker;

pub(crate) use config::{validate_config, DexProviderConfig};
pub(crate) use worker::DexProvider;

#[cfg(test)]
mod tests;
