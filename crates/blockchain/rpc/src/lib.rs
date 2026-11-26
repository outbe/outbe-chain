//! Outbe custom RPC namespace (`outbe_*`).
//!
//! Provides JSON-RPC methods for consensus status, certified finalization
//! recovery, TEE upgrade/renewal scheduling, and Radicle sidecar health.

mod api;
pub mod errors;
mod server;
pub mod state_reader;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use api::OutbeApiServer;
pub use errors::RpcError;
pub use server::OutbeApiHandler;
