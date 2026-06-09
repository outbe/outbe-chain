use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TributeError {
    #[error("tribute not found")]
    TributeNotFound,

    #[error("invalid owner")]
    InvalidOwner,

    #[error("settlement amount must be positive")]
    SettlementAmountMustBePositive,

    #[error("worldwide day is sealed")]
    WorldwideDaySealed,

    #[error("owner balance overflow")]
    OwnerBalanceOverflow,
}

impl From<TributeError> for PrecompileError {
    fn from(value: TributeError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

pub type TributeResult<T> = std::result::Result<T, TributeError>;
