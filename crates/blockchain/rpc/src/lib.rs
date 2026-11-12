//! Outbe custom RPC namespace (`outbe_*`).
//!
//! Provides JSON-RPC methods for querying validator infrastructure state:
//! validators, epoch info, staking, rewards, and slashing data.

mod api;
pub mod errors;
mod server;
pub mod state_reader;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use api::{
    EpochInfo, OutbeApiServer, ParticipationInfo, SlashConfig, SlashInfo, ValidatorDetailInfo,
    ValidatorInfo,
};
pub use errors::{RpcError, RpcValidatorStateError};
pub use server::OutbeApiHandler;
