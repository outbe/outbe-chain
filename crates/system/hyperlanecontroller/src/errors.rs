use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HyperlaneControllerError {
    #[error("hyperlane controller is already initialized")]
    AlreadyInitialized,

    #[error("hyperlane controller is not initialized")]
    NotInitialized,

    #[error("local domain {domain} must be included in the ISM table")]
    LocalDomainMissing { domain: u32 },

    #[error("caller {caller} is not the current owner {owner} of the local ISM")]
    NotIsmOwner { caller: Address, owner: Address },

    #[error("local ISM pending owner is {pending}, expected the controller")]
    IsmNotPendingToController { pending: Address },

    #[error("contract {contract} is owned by {owner}, expected the controller")]
    NotOwnedByController { contract: Address, owner: Address },

    #[error("address must be non-zero")]
    InvalidAddress,

    #[error("domain must be non-zero")]
    InvalidDomain,

    #[error("domains and ISMs length mismatch")]
    IsmLengthMismatch,

    #[error("domains and hooks length mismatch")]
    HookLengthMismatch,

    #[error("no MerkleTreeHook configured for domain {domain}")]
    UnknownHook { domain: u32 },

    #[error("caller {caller} is not an active validator or its oracle delegate")]
    NotActiveValidator { caller: Address },

    #[error(
        "checkpoint signer {recovered} does not match validator {validator} signer {expected}"
    )]
    SignerMismatch {
        validator: Address,
        expected: Address,
        recovered: Address,
    },

    #[error("checkpoint index {index} is not newer than the submitted {submitted}")]
    StaleIndex { index: u32, submitted: u32 },

    #[error("checkpoint signature must be 65 bytes, got {length}")]
    InvalidSignatureLength { length: usize },

    #[error("domain {domain} is not configured")]
    UnknownDomain { domain: u32 },

    #[error("local domain {domain} cannot be removed or targeted remotely")]
    LocalDomainNotAllowed { domain: u32 },

    #[error("validator list must be non-empty and at most 255 entries, got {count}")]
    InvalidValidatorCount { count: usize },

    #[error("threshold {threshold} must be within 1..={validators}")]
    InvalidThreshold { threshold: u8, validators: usize },

    #[error("validator {validator} is zero or duplicated")]
    InvalidValidator { validator: Address },

    #[error("remote call list must be non-empty")]
    EmptyCalls,

    #[error("fund requires a non-zero value")]
    ZeroFund,

    #[error("controller balance {available} is below the required {required}")]
    InsufficientBalance { required: U256, available: U256 },

    #[error("undecodable return data from {0}")]
    UndecodableReturn(&'static str),

    #[error("invalid hyperlane controller proposal payload: {0}")]
    InvalidProposalPayload(String),
}

impl From<HyperlaneControllerError> for PrecompileError {
    fn from(err: HyperlaneControllerError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
