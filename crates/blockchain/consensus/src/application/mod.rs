//! Application actor — bridges Simplex consensus with Reth's execution layer.
//!
//! Handles propose/verify/finalize requests from the consensus engine
//! by communicating with Reth via `beacon_engine_handle`.

pub mod actor;
pub mod handler;
pub mod ingress;
pub(crate) mod validation;

pub use handler::{ApplicationDeps, ApplicationHandler};
pub use ingress::Mailbox;
