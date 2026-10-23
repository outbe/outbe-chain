//! Module-local error types for the Credis contract.
//!
//! Errors that are not credis-specific (out-of-gas, generic revert) come from
//! `outbe_primitives::error::PrecompileError`.

use crate::precompile::ICredis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisError {
    #[error("Credis not found")]
    CredisNotFound,
    #[error("invalid Credis state value: {0}")]
    InvalidStateValue(u8),
    #[error("credis arithmetic overflow")]
    ArithmeticOverflow,
    #[error("index out of bounds")]
    IndexOutOfBounds,
    #[error("Credis is non-transferable")]
    NonTransferable,
    #[error("unknown credis profile selector: {0}")]
    UnknownProfileSelector(u8),
    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => CredisError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<CredisError> for PrecompileError {
    fn from(err: CredisError) -> Self {
        use ICredis as I;
        match err {
            CredisError::CredisNotFound => revert_with(I::CredisNotFound {}),
            CredisError::InvalidStateValue(value) => revert_with(I::InvalidStateValue { value }),
            CredisError::ArithmeticOverflow => revert_with(I::ArithmeticOverflow {}),
            CredisError::IndexOutOfBounds => revert_with(I::IndexOutOfBounds {}),
            CredisError::NonTransferable => revert_with(I::NonTransferable {}),
            CredisError::UnknownProfileSelector(value) => {
                revert_with(I::UnknownProfileSelector { value })
            }
            CredisError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}
