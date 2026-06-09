use outbe_common::WorldwideDay;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TributeFactoryError {
    #[error("TEE not configured")]
    TeeNotConfigured,

    #[error("decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("worldwide_day {worldwide_day} is not in OFFERING status (status={status})")]
    WorldwideDayNotOffering {
        worldwide_day: WorldwideDay,
        status: u8,
    },

    #[error("tribute already exists for this combination of parameters")]
    TributeAlreadyExists,

    #[error("SU hash already used")]
    SuHashAlreadyUsed,

    #[error("invalid SU hash hex: {hash}")]
    InvalidSuHashHex { hash: String },

    #[error("SU hash must be 32 bytes, got {length}")]
    InvalidSuHashLength { length: usize },

    #[error("amount must be positive")]
    AmountMustBePositive,

    #[error("nominal price is zero: no VWAP or S-curve data available for worldwide_day {worldwide_day}")]
    NominalPriceUnavailable { worldwide_day: WorldwideDay },

    #[error("nominal amount overflow")]
    NominalAmountOverflow,

    #[error("invalid wallet address: {address}")]
    InvalidWalletAddress { address: String },

    #[error("invalid SRA address: {address}")]
    InvalidSraAddress { address: String },

    #[error("issuance currency {issuance_currency} not registered in oracle")]
    IssuanceCurrencyNotRegistered { issuance_currency: u16 },

    #[error("settlement currency pair not registered")]
    SettlementCurrencyPairNotRegistered,

    #[error("wallet_addresses must be specified when sra_addresses is provided")]
    WalletAddressesRequiredWhenSraProvided,

    #[error("sra_addresses must be specified when wallet_addresses is provided")]
    SraAddressesRequiredWhenWalletProvided,

    #[error("invalid wallet_address at index {index}: {address}")]
    InvalidWalletAddressAtIndex { index: usize, address: String },

    #[error("invalid sra_address at index {index}: {address}")]
    InvalidSraAddressAtIndex { index: usize, address: String },

    #[error("base amount has too many decimal places (max 18)")]
    BaseAmountTooManyDecimals,

    #[error("invalid base amount format")]
    InvalidBaseAmountFormat,

    #[error("invalid atto amount format")]
    InvalidAttoAmountFormat,
}

impl From<TributeFactoryError> for PrecompileError {
    fn from(value: TributeFactoryError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

pub type TributeFactoryResult<T> = std::result::Result<T, TributeFactoryError>;
