//! Typed errors for `outbe-hyperlane-agent`.
//!
//! Each submodule defines the error type of one part of the agent.

mod config;
mod liveness;
mod relay;
mod signer;
mod store;
mod sync;

pub(crate) use config::ConfigError;
pub(crate) use liveness::LivenessError;
pub(crate) use relay::RelayError;
pub(crate) use signer::SignerError;
pub(crate) use store::StoreError;
pub(crate) use sync::SyncError;
