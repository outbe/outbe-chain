//! Typed error definitions for the Outbe CLI.

mod commands;
mod rpc;
mod tee_common;

pub(crate) use commands::{
    CommandInputError, MonitorError, RadError, SlashError, StakingError, ValidatorError, VoteError,
};
pub(crate) use rpc::{RpcError, TxError};
pub(crate) use tee_common::TeeStatusError;
