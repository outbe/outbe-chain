//! Error types for the vaultrouter precompile.
//!
//! These variants match the custom errors from the Solidity interfaces and `ErrorsLib`.
//! Precompile reverts return a string message instead of an ABI selector.

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
    #[error("Credis owner mismatch")]
    CredisOwnerMismatch,
    #[error("reservation amount exceeds hold: available={available}, required={required}")]
    ReservationInsufficient { available: U256, required: U256 },
    #[error("block timestamp does not fit unix seconds")]
    TimestampOverflow,
    #[error("{0} undecodable")]
    RouterCallUndecodable(&'static str),
    #[error("index out of bounds")]
    IndexOutOfBounds,
    #[error("invalid asset")]
    InvalidAsset,
    #[error("invalid asset decimals")]
    InvalidAssetDecimals,
    #[error("unsupported asset decimals")]
    UnsupportedAssetDecimalsSimple,
    #[error("invalid asset currency")]
    InvalidAssetCurrency,
    #[error("pledge price unavailable")]
    PledgePriceUnavailable,
    #[error("timestamp exceeds u64")]
    TimestampExceedsU64,
    #[error("previous day VWAP unavailable")]
    PreviousDayVwapUnavailable,
    #[error("reference pair unavailable")]
    ReferencePairUnavailable,
}

impl From<VaultRouterError> for PrecompileError {
    fn from(err: VaultRouterError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

/// Why a right's settlement quote or payment failed: asset acceptance, conversion,
/// or the token transfer into the reserve vault.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SettlementError {
    #[error("assetVaultsCount undecodable")]
    AssetVaultsCount,
    #[error("settlement asset {asset} has no registered vault")]
    AssetNotRegistered { asset: Address },
    #[error("settlement asset currency {iso_code} does not match the right")]
    CurrencyMismatch { iso_code: u16 },
    #[error("settlement asset decimals undecodable")]
    AssetDecimals,
    #[error("settlement asset isoCode undecodable")]
    AssetIsoCode,
    #[error("{0}")]
    Rounding(#[source] outbe_primitives::settlement::RoundingError),
    #[error("oracle nominal unavailable")]
    OracleUnavailable,
    #[error("VWAP snapshot {authorized:#x} does not match the required {required:#x}")]
    SnapshotMismatch { authorized: U256, required: U256 },
    #[error("settlement token call failed")]
    TokenOperationFailed,
    #[error("settlement token moved an unexpected amount")]
    AmountMismatch,
}

impl From<SettlementError> for PrecompileError {
    fn from(error: SettlementError) -> Self {
        PrecompileError::Revert(error.to_string())
    }
}
