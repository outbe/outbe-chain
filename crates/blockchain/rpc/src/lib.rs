//! Outbe custom RPC namespace (`outbe_*`).
//!
//! Provides JSON-RPC methods for querying validator infrastructure state:
//! validators, epoch info, staking, rewards, and slashing data.

mod api;
mod server;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use api::OutbeApiServer;
pub use server::OutbeApiHandler;
