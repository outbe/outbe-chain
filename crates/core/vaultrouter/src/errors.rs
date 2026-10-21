//! Error types for the vaultrouter precompile.
//!
//! A variant with a matching custom error in `IVaultRouter.sol` reverts with that
//! error's ABI encoding: its selector, then its fields. A variant with no declared
//! Solidity error reverts with a string message.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::api::IVaultRouter;

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
    #[error("previous day VWAP unavailable")]
    PreviousDayVwapUnavailable,
    #[error("reference pair unavailable")]
    ReferencePairUnavailable,
}

/// Reverts with the ABI encoding of a declared `IVaultRouter` error.
fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<VaultRouterError> for PrecompileError {
    fn from(err: VaultRouterError) -> Self {
        use IVaultRouter as I;
        match err {
            VaultRouterError::TokenOperationFailed => revert_with(I::TokenOperationFailed {}),
            VaultRouterError::InvalidLiquiditySource => revert_with(I::InvalidLiquiditySource {}),
            VaultRouterError::InvalidLiquidityTarget => revert_with(I::InvalidLiquidityTarget {}),
            VaultRouterError::ReserveVaultNotConfigured => {
                revert_with(I::ReserveVaultNotConfigured {})
            }
            VaultRouterError::ReserveVaultAlreadyAdded => {
                revert_with(I::ReserveVaultAlreadyAdded {})
            }
            VaultRouterError::ReserveVaultNotFound => revert_with(I::ReserveVaultNotFound {}),
            VaultRouterError::ReserveVaultOwnerNotRenounced(current_owner) => {
                revert_with(I::ReserveVaultOwnerNotRenounced {
                    currentOwner: current_owner,
                })
            }
            VaultRouterError::LiquiditySourceNotFound => revert_with(I::LiquiditySourceNotFound {}),
            VaultRouterError::LiquidityTargetNotFound => revert_with(I::LiquidityTargetNotFound {}),
            VaultRouterError::InsufficientSharesForWithdraw {
                available,
                required,
            } => revert_with(I::InsufficientSharesForWithdraw {
                availableShares: available,
                requiredShares: required,
            }),
            VaultRouterError::SameVaultRebalance => revert_with(I::SameVaultRebalance {}),
            VaultRouterError::InvalidRebalanceAmount => revert_with(I::InvalidRebalanceAmount {}),
            VaultRouterError::RebalanceVaultNotRegistered(vault) => {
                revert_with(I::RebalanceVaultNotRegistered { vault })
            }
            VaultRouterError::RebalanceInputExceedsMax {
                required,
                max_amount_to,
            } => revert_with(I::RebalanceInputExceedsMax {
                requiredMinor: required,
                maxAssetsToMinor: max_amount_to,
            }),
            VaultRouterError::UnsupportedAssetDecimals(decimals) => {
                revert_with(I::UnsupportedAssetDecimals { decimals })
            }
            VaultRouterError::ReservationExists(id) => revert_with(I::ReservationExists { id }),
            VaultRouterError::ReservationNotFound(id) => revert_with(I::ReservationNotFound { id }),
            VaultRouterError::ReservationExpired(id) => revert_with(I::ReservationExpired { id }),
            VaultRouterError::CredisOwnerMismatch => revert_with(I::CredisOwnerMismatch {}),
            VaultRouterError::ReservationInsufficient {
                available,
                required,
            } => revert_with(I::ReservationInsufficient {
                availableMinor: available,
                requiredMinor: required,
            }),
            VaultRouterError::InvalidReservationAmount => {
                revert_with(I::InvalidReservationAmount {})
            }
            VaultRouterError::ZeroAddress => revert_with(I::ZeroAddress {}),
            VaultRouterError::Unauthorized => revert_with(I::Unauthorized {}),
            VaultRouterError::ReceiverNotDeployed => revert_with(I::ReceiverNotDeployed {}),
            VaultRouterError::UndecodableReturn(value) => revert_with(I::UndecodableReturn {
                value: value.to_string(),
            }),
            VaultRouterError::TimestampOverflow => revert_with(I::TimestampOverflow {}),
            VaultRouterError::RouterCallUndecodable(value) => {
                revert_with(I::RouterCallUndecodable {
                    value: value.to_string(),
                })
            }
            VaultRouterError::IndexOutOfBounds => revert_with(I::IndexOutOfBounds {}),
            VaultRouterError::InvalidAsset => revert_with(I::InvalidAsset {}),
            VaultRouterError::InvalidAssetDecimals => revert_with(I::InvalidAssetDecimals {}),
            VaultRouterError::UnsupportedAssetDecimalsSimple => {
                revert_with(I::UnsupportedAssetDecimalsSimple {})
            }
            VaultRouterError::InvalidAssetCurrency => revert_with(I::InvalidAssetCurrency {}),
            VaultRouterError::PledgePriceUnavailable => revert_with(I::PledgePriceUnavailable {}),
            VaultRouterError::PreviousDayVwapUnavailable => {
                revert_with(I::PreviousDayVwapUnavailable {})
            }
            VaultRouterError::ReferencePairUnavailable => {
                revert_with(I::ReferencePairUnavailable {})
            }
        }
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
        use IVaultRouter as I;
        match error {
            SettlementError::AssetVaultsCount => revert_with(I::AssetVaultsCount {}),
            SettlementError::AssetNotRegistered { asset } => {
                revert_with(I::AssetNotRegistered { asset })
            }
            SettlementError::CurrencyMismatch { iso_code } => {
                revert_with(I::CurrencyMismatch { isoCode: iso_code })
            }
            SettlementError::AssetDecimals => revert_with(I::AssetDecimals {}),
            SettlementError::AssetIsoCode => revert_with(I::AssetIsoCode {}),
            SettlementError::Rounding(reason) => revert_with(I::Rounding {
                reason: reason.to_string(),
            }),
            SettlementError::OracleUnavailable => revert_with(I::OracleUnavailable {}),
            SettlementError::SnapshotMismatch {
                authorized,
                required,
            } => revert_with(I::SnapshotMismatch {
                authorized,
                required,
            }),
            SettlementError::TokenOperationFailed => revert_with(I::TokenOperationFailed {}),
            SettlementError::AmountMismatch => revert_with(I::AmountMismatch {}),
        }
    }
}
