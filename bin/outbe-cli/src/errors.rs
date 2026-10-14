//! Typed error definitions for the Outbe CLI.

mod commands;
mod rpc;
mod tee_common;
mod tee_join;
mod tee_upgrade;

pub(crate) use commands::{
    CommandInputError, EpochError, MonitorError, OracleError, RadError, RadicleError, SlashError,
    StablecoinError, TributeError, ValidatorError, VoteError, ZeroFeeError,
};
pub(crate) use rpc::{RpcError, TxError};
pub(crate) use tee_common::{
    TeeAdmissionHistoryError, TeeIdentityError, TeeRenewalError, TeeRpcError,
};
pub(crate) use tee_join::{
    TeeJoinAdmissionError, TeeJoinCompletionError, TeeJoinEnclaveError, TeeJoinError,
};
pub(crate) use tee_upgrade::{TeeUpgradeError, TeeUpgradeNetworkError};
