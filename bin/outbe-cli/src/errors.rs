//! Typed error definitions for the Outbe CLI.

mod commands;
mod rpc;
mod tee_common;

pub(crate) use commands::{
    CommandInputError, EpochError, MonitorError, OracleError, RadError, RadicleError, SlashError,
    StablecoinError, StakingError, ValidatorError, VoteError, ZeroFeeError,
};
pub(crate) use rpc::{RpcError, TxError};
pub(crate) use tee_common::TeeStatusError;
