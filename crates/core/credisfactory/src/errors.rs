//! Error types for the CredisFactory precompile.

use crate::precompile::ICredisFactory;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the CredisFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisFactoryError {
    #[error("settlement amount is zero")]
    InvalidAmount,
    #[error("attached COEN must equal the pledged collateral exactly")]
    CcaStakeMismatch,
    #[error("reservation not found")]
    ReservationNotFound,
    #[error("reservation expired")]
    ReservationExpired,
    #[error("reservation cca mismatch")]
    ReservationCcaMismatch,
    #[error("amount must be positive")]
    ZeroIssueTerm,
    #[error("pledge source is zero")]
    InvalidSource,
    #[error("Credis owner is zero")]
    InvalidOwner,
    #[error("Credis already exists")]
    CredisAlreadyExists,
    #[error("Credis is closed")]
    CredisClosed,
    #[error("payment is below the interest accrued since the last settlement")]
    PaymentBelowAccruedInterest,
    #[error("Credis is not called")]
    NotCalled,
    #[error("settlement deadline has not passed")]
    SettlementDeadlineNotPassed,
    #[error("settlement deadline has passed")]
    SettlementDeadlinePassed,
    #[error("Credis has no outstanding principal")]
    NothingOutstanding,
    #[error("only the Credis owner can forfeit it")]
    NotCredisOwner,
    #[error("payoff overflow")]
    PayoffOverflow,
    #[error("asset decimals undecodable")]
    AssetDecimalsUndecodable,
    #[error("asset currency undecodable")]
    AssetCurrencyUndecodable,
    #[error("asset metadata changed")]
    AssetMetadataChanged,
    #[error("COEN stake overflow")]
    CoenStakeOverflow,
    #[error("Credis changed during issuance")]
    CredisChangedDuringIssuance,
    #[error("collateral increased")]
    CollateralIncreased,
    #[error("collateral release mismatch")]
    CollateralReleaseMismatch,
    #[error("Credis changed during payment")]
    CredisChangedDuringPayment,
    #[error("forfeiture collateral mismatch")]
    ForfeitureCollateralMismatch,
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        use ICredisFactory as I;
        match err {
            CredisFactoryError::InvalidAmount => revert_with(I::InvalidAmount {}),
            CredisFactoryError::CcaStakeMismatch => revert_with(I::CcaStakeMismatch {}),
            CredisFactoryError::ReservationNotFound => revert_with(I::ReservationNotFound {}),
            CredisFactoryError::ReservationExpired => revert_with(I::ReservationExpired {}),
            CredisFactoryError::ReservationCcaMismatch => revert_with(I::ReservationCcaMismatch {}),
            CredisFactoryError::ZeroIssueTerm => revert_with(I::ZeroIssueTerm {}),
            CredisFactoryError::InvalidSource => revert_with(I::InvalidSource {}),
            CredisFactoryError::InvalidOwner => revert_with(I::InvalidOwner {}),
            CredisFactoryError::CredisAlreadyExists => revert_with(I::CredisAlreadyExists {}),
            CredisFactoryError::CredisClosed => revert_with(I::CredisClosed {}),
            CredisFactoryError::PaymentBelowAccruedInterest => {
                revert_with(I::PaymentBelowAccruedInterest {})
            }
            CredisFactoryError::NotCalled => revert_with(I::NotCalled {}),
            CredisFactoryError::SettlementDeadlineNotPassed => {
                revert_with(I::SettlementDeadlineNotPassed {})
            }
            CredisFactoryError::SettlementDeadlinePassed => {
                revert_with(I::SettlementDeadlinePassed {})
            }
            CredisFactoryError::NothingOutstanding => revert_with(I::NothingOutstanding {}),
            CredisFactoryError::NotCredisOwner => revert_with(I::NotCredisOwner {}),
            CredisFactoryError::PayoffOverflow => revert_with(I::PayoffOverflow {}),
            CredisFactoryError::AssetDecimalsUndecodable => {
                revert_with(I::AssetDecimalsUndecodable {})
            }
            CredisFactoryError::AssetCurrencyUndecodable => {
                revert_with(I::AssetCurrencyUndecodable {})
            }
            CredisFactoryError::AssetMetadataChanged => revert_with(I::AssetMetadataChanged {}),
            CredisFactoryError::CoenStakeOverflow => revert_with(I::CoenStakeOverflow {}),
            CredisFactoryError::CredisChangedDuringIssuance => {
                revert_with(I::CredisChangedDuringIssuance {})
            }
            CredisFactoryError::CollateralIncreased => revert_with(I::CollateralIncreased {}),
            CredisFactoryError::CollateralReleaseMismatch => {
                revert_with(I::CollateralReleaseMismatch {})
            }
            CredisFactoryError::CredisChangedDuringPayment => {
                revert_with(I::CredisChangedDuringPayment {})
            }
            CredisFactoryError::ForfeitureCollateralMismatch => {
                revert_with(I::ForfeitureCollateralMismatch {})
            }
        }
    }
}
