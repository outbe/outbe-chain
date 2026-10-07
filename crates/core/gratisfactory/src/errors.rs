use outbe_primitives::error::PrecompileError;
use thiserror::Error;

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
}

impl From<GratisFactoryError> for PrecompileError {
    fn from(err: GratisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
