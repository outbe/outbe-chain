//! Error type for the vaultprovider precompile.
//!
//! Mirrors the custom errors of the Solidity `IVaultProvider` interface plus
//! the `ErrorsLib` reverts the original contract used (`ZeroAddress`,
//! `Unauthorized`). Following the repo convention, precompile reverts carry a
//! string reason rather than an ABI-encoded custom-error selector.

use alloy_primitives::U256;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VaultProviderError {
    #[error("zero address")]
    ZeroAddress,
    #[error("unauthorized")]
    Unauthorized,
    #[error("invalid liquidity source")]
    InvalidLiquiditySource,
    #[error("invalid liquidity target")]
    InvalidLiquidityTarget,
    #[error("reserve vault not configured")]
    ReserveVaultNotConfigured,
    #[error("reserve vault already added")]
    ReserveVaultAlreadyAdded,
    #[error("reserve vault not found")]
    ReserveVaultNotFound,
    #[error("liquidity source not found")]
    LiquiditySourceNotFound,
    #[error("liquidity target not found")]
    LiquidityTargetNotFound,
    #[error("insufficient shares for withdraw: available={available}, required={required}")]
    InsufficientSharesForWithdraw { available: U256, required: U256 },
    #[error("token bundle receiver is not a deployed contract")]
    ReceiverNotDeployed,
    #[error("undecodable sub-call return: {0}")]
    UndecodableReturn(&'static str),
}

impl From<VaultProviderError> for PrecompileError {
    fn from(err: VaultProviderError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
