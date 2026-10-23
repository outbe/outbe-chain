//! Module-local error types for the Intex runtime module.
//!
//! Errors that are not registry-specific come from
//! `outbe_primitives::error::PrecompileError`. The storage DSL's record-level
//! `create` rejects a duplicate series, so no local variant exists for it.

use crate::precompile::IIntex;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IntexError {
    #[error("series not found")]
    SeriesNotFound,
    #[error("issuedAt must be non-zero")]
    ZeroIssuedAt,
    #[error("invalid stored lifecycle state value: {0}")]
    InvalidStateValue(u8),
    #[error("invalid series id components")]
    InvalidSeriesId,
    #[error("realized units exceed the issued count for series")]
    RealizedUnitsOverflow,
    #[error("call group already called")]
    GroupAlreadyCalled,
    #[error("series index out of range")]
    SeriesIndexOutOfRange,
    #[error("unknown intex profile selector: {0}")]
    UnknownProfileSelector(u8),
    #[error("called group member out of range")]
    CalledGroupMemberOutOfRange,
    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => IntexError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<IntexError> for PrecompileError {
    fn from(err: IntexError) -> Self {
        use IIntex as I;
        match err {
            IntexError::SeriesNotFound => revert_with(I::SeriesNotFound {}),
            IntexError::ZeroIssuedAt => revert_with(I::ZeroIssuedAt {}),
            IntexError::InvalidStateValue(value) => revert_with(I::InvalidStateValue { value }),
            IntexError::InvalidSeriesId => revert_with(I::InvalidSeriesId {}),
            IntexError::RealizedUnitsOverflow => revert_with(I::RealizedUnitsOverflow {}),
            IntexError::GroupAlreadyCalled => revert_with(I::GroupAlreadyCalled {}),
            IntexError::SeriesIndexOutOfRange => revert_with(I::SeriesIndexOutOfRange {}),
            IntexError::UnknownProfileSelector(value) => {
                revert_with(I::UnknownProfileSelector { value })
            }
            IntexError::CalledGroupMemberOutOfRange => {
                revert_with(I::CalledGroupMemberOutOfRange {})
            }
            IntexError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}
