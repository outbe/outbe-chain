//! Error types for the GratisFactory precompile.

use crate::precompile::IGratisFactory;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the GratisFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisFactoryError {
    #[error("fidelity index not eligible")]
    FidelityNotEligible,
    #[error("reservation not found")]
    ReservationNotFound,
    #[error("reservation expired")]
    ReservationExpired,
    #[error("caller is not the reservation source")]
    NotReservationSource,
    #[error("reservation already pledged")]
    PledgeExists,
    #[error("pledge not found")]
    PledgeNotFound,
    #[error("pledge does not match the reservation")]
    PledgeMismatch,
    #[error("native COEN amount overflow")]
    NativeCoenOverflow,
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<GratisFactoryError> for PrecompileError {
    fn from(err: GratisFactoryError) -> Self {
        use IGratisFactory as I;
        match err {
            GratisFactoryError::FidelityNotEligible => revert_with(I::FidelityNotEligible {}),
            GratisFactoryError::ReservationNotFound => revert_with(I::ReservationNotFound {}),
            GratisFactoryError::ReservationExpired => revert_with(I::ReservationExpired {}),
            GratisFactoryError::NotReservationSource => revert_with(I::NotReservationSource {}),
            GratisFactoryError::PledgeExists => revert_with(I::PledgeExists {}),
            GratisFactoryError::PledgeNotFound => revert_with(I::PledgeNotFound {}),
            GratisFactoryError::PledgeMismatch => revert_with(I::PledgeMismatch {}),
            GratisFactoryError::NativeCoenOverflow => revert_with(I::NativeCoenOverflow {}),
        }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CollateralError {
    #[error("Credis already has collateral")]
    Exists,
    #[error("collateral not found")]
    NotFound,
    #[error("amount exceeds the Credis's collateral")]
    Exceeded,
}

impl From<CollateralError> for PrecompileError {
    fn from(err: CollateralError) -> Self {
        use IGratisFactory as I;
        match err {
            CollateralError::Exists => revert_with(I::Exists {}),
            CollateralError::NotFound => revert_with(I::NotFound {}),
            CollateralError::Exceeded => revert_with(I::Exceeded {}),
        }
    }
}
