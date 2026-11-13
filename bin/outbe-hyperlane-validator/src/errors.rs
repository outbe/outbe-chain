//! Typed errors for `outbe-hyperlane-validator`.
//!
//! Each submodule defines the error type of one validator part.

mod config;
mod liveness;
mod signer;
mod store;
mod sync;

pub(crate) use config::ConfigError;
pub(crate) use liveness::LivenessError;
pub(crate) use signer::SignerError;
pub(crate) use store::StoreError;
pub(crate) use sync::SyncError;
