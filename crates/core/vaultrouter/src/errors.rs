//! Error type for the vaultrouter precompile.
//!
//! Mirrors the custom errors of the Solidity vault-router interfaces plus
//! the `ErrorsLib` reverts the original contract used (`ZeroAddress`,
//! `Unauthorized`). Following the repo convention, precompile reverts carry a
//! string reason rather than an ABI-encoded custom-error selector.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VaultRouterError {
    #[error("ERC20 operation failed or returned malformed data")]
    TokenOperationFailed,
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
    #[error("reserve vault owner not renounced: {0}")]
    ReserveVaultOwnerNotRenounced(Address),
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
    #[error("same vault rebalance")]
    SameVaultRebalance,
    #[error("invalid rebalance amount")]
    InvalidRebalanceAmount,
    #[error("rebalance vault not registered: {0}")]
    RebalanceVaultNotRegistered(Address),
    #[error("rebalance input exceeds max: required={required}, max_amount_to={max_amount_to}")]
    RebalanceInputExceedsMax { required: U256, max_amount_to: U256 },
    #[error("unsupported asset decimals: {0}")]
    UnsupportedAssetDecimals(u8),
    #[error("invalid reservation amount")]
    InvalidReservationAmount,
    #[error("reservation exists: {0}")]
    ReservationExists(U256),
    #[error("reservation not found: {0}")]
    ReservationNotFound(U256),
    #[error("reservation expired: {0}")]
    ReservationExpired(U256),
    #[error("reservation account mismatch")]
    ReservationAccountMismatch,
    #[error("reservation amount exceeds hold: available={available}, required={required}")]
    ReservationInsufficient { available: U256, required: U256 },
    #[error("block timestamp does not fit unix seconds")]
    TimestampOverflow,
}

impl From<VaultRouterError> for PrecompileError {
    fn from(err: VaultRouterError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
