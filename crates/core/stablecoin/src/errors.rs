use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::abi::IStablecoin;

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum StablecoinStateError {
    #[error("stablecoin token is not initialized")]
    Uninitialized,

    #[error("stablecoin token is already initialized")]
    AlreadyInitialized,

    #[error("invalid stablecoin initialization identity")]
    InvalidInitializationIdentity,

    #[error("unknown stablecoin initialization policy {policy_id}")]
    UnknownInitializationPolicy { policy_id: U256 },

    #[error(
        "stablecoin migration required from schema {stored_schema_version} to {active_schema_version}"
    )]
    MigrationRequired {
        stored_schema_version: u64,
        active_schema_version: u64,
    },

    #[error("stablecoin initialization found non-pristine root slot {slot}")]
    NonPristineRoot { slot: u64 },
}

impl From<StablecoinStateError> for PrecompileError {
    fn from(error: StablecoinStateError) -> Self {
        match error {
            StablecoinStateError::MigrationRequired {
                stored_schema_version,
                active_schema_version,
            } => PrecompileError::RevertBytes(Bytes::from(
                IStablecoin::MigrationRequired {
                    storedSchemaVersion: stored_schema_version,
                    activeSchemaVersion: active_schema_version,
                }
                .abi_encode(),
            )),
            fatal @ (StablecoinStateError::AlreadyInitialized
            | StablecoinStateError::NonPristineRoot { .. }) => {
                PrecompileError::Fatal(fatal.to_string())
            }
            domain_error => PrecompileError::Revert(domain_error.to_string()),
        }
    }
}
