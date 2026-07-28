use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::abi::IStablecoin;

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum StablecoinStateError {
    #[error("unexpected value {value}")]
    UnexpectedValue { value: U256 },

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

    #[error("invalid stablecoin address {account}")]
    InvalidAddress { account: Address },

    #[error("stablecoin amount must be non-zero")]
    InvalidAmount,

    #[error(
        "insufficient stablecoin balance for {account}: available {available}, required {required}"
    )]
    InsufficientBalance {
        account: Address,
        available: U256,
        required: U256,
    },

    #[error(
        "insufficient stablecoin allowance from {owner} to {spender}: available {available}, required {required}"
    )]
    InsufficientAllowance {
        owner: Address,
        spender: Address,
        available: U256,
        required: U256,
    },

    #[error(
        "stablecoin supply cap {supply_cap} would be exceeded by requested supply {requested_supply}"
    )]
    SupplyCapExceeded {
        supply_cap: U256,
        requested_supply: U256,
    },
}

impl From<StablecoinStateError> for PrecompileError {
    fn from(error: StablecoinStateError) -> Self {
        match error {
            StablecoinStateError::UnexpectedValue { value } => PrecompileError::RevertBytes(
                Bytes::from(IStablecoin::UnexpectedValue { value }.abi_encode()),
            ),
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
            StablecoinStateError::InvalidAddress { account } => PrecompileError::RevertBytes(
                Bytes::from(IStablecoin::InvalidAddress { account }.abi_encode()),
            ),
            StablecoinStateError::InvalidAmount => PrecompileError::RevertBytes(Bytes::from(
                IStablecoin::InvalidAmount {}.abi_encode(),
            )),
            StablecoinStateError::InsufficientBalance {
                account,
                available,
                required,
            } => PrecompileError::RevertBytes(Bytes::from(
                IStablecoin::InsufficientBalance {
                    account,
                    available,
                    required,
                }
                .abi_encode(),
            )),
            StablecoinStateError::InsufficientAllowance {
                owner,
                spender,
                available,
                required,
            } => PrecompileError::RevertBytes(Bytes::from(
                IStablecoin::InsufficientAllowance {
                    owner,
                    spender,
                    available,
                    required,
                }
                .abi_encode(),
            )),
            StablecoinStateError::SupplyCapExceeded {
                supply_cap,
                requested_supply,
            } => PrecompileError::RevertBytes(Bytes::from(
                IStablecoin::SupplyCapExceeded {
                    supplyCap: supply_cap,
                    requestedSupply: requested_supply,
                }
                .abi_encode(),
            )),
            domain_error => PrecompileError::Revert(domain_error.to_string()),
        }
    }
}
