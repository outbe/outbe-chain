//! Error types for the IntexFactory precompile.

use outbe_common::settlement::RoundingError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the IntexFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IntexFactoryError {
    #[error("zero address")]
    ZeroAddress,
    #[error("amount must be positive")]
    ZeroAmount,
    #[error("units must be positive")]
    ZeroUnits,
    #[error("series not found")]
    SeriesNotFound,
    #[error("series not settleable in state {0}")]
    NotSettleable(u8),
    #[error("settlement deadline expired")]
    DeadlineExpired,
    #[error("zero balance")]
    ZeroBalance,
    #[error("units exceed balance")]
    UnitsExceedBalance,
    #[error("insufficient settled balance")]
    InsufficientSettled,
    #[error("insufficient proof of work")]
    InsufficientProofOfWork,
    #[error("{0}")]
    Rounding(RoundingError),
    #[error("oracle nominal unavailable")]
    OracleUnavailable,
    #[error("VWAP snapshot {authorized:#x} does not match the required {required:#x}")]
    VwapSnapshotMismatch {
        authorized: alloy_primitives::U256,
        required: alloy_primitives::U256,
    },
    #[error("settlement asset {0} has no registered vault")]
    SettlementAssetNotRegistered(alloy_primitives::Address),
    #[error("settlement asset currency {0} does not match the series")]
    SettlementCurrencyMismatch(u16),
    #[error("no COEN rate published for currency {0}")]
    FxRateUnavailable(u16),
    #[error("COEN rate for currency {0} is too old to convert with")]
    FxRateStale(u16),
    #[error("caller is not the origin router")]
    NotOriginRouter,
    #[error(
        "currency {iso} day {worldwide_day} is indexed in bin {expected}, series priced into {got}"
    )]
    GroupBinMismatch {
        iso: u16,
        worldwide_day: outbe_primitives::time::WorldwideDay,
        expected: u32,
        got: u32,
    },
    #[error("currency {iso} day {worldwide_day} is already indexed")]
    GroupAlreadyIndexed {
        iso: u16,
        worldwide_day: outbe_primitives::time::WorldwideDay,
    },

    #[error("settlement token call failed")]
    TokenOperationFailed,

    #[error("settlement token moved an unexpected amount")]
    SettlementAmountMismatch,

    #[error("unknown intex profile selector: {0}")]
    UnknownProfileSelector(u8),

    #[error("forfeited promis load overflow")]
    ForfeitedPromisLoadOverflow,

    #[error("closeCallNoticeForTest: no called group")]
    NoCalledGroupForTest,

    #[error("issueForTest: a currency per series")]
    IssueForTestCurrencyCountMismatch,

    #[error("block timestamp exceeds u32")]
    BlockTimestampExceedsU32,

    #[error("issuance chunk count exceeds u16")]
    IssuanceChunkCountExceedsU16,

    #[error("price exceeds the wire type")]
    PriceExceedsWireType,

    #[error("marked-up price overflow")]
    MarkedUpPriceOverflow,

    #[error("owner units exceed u32")]
    OwnerUnitsExceedU32,

    #[error("settled units exceed u32")]
    SettledUnitsExceedU32,

    #[error("NFT balance exceeds u32")]
    NftBalanceExceedsU32,

    #[error("NFT balanceOf undecodable")]
    NftBalanceOfUndecodable,

    #[error("called group member out of range")]
    CalledGroupMemberOutOfRange,

    #[error("the router answered no legs")]
    RouterAnsweredNoLegs,

    #[error("promis overflow")]
    PromisOverflow,

    #[error("mining sequence overflow")]
    MiningSequenceOverflow,

    #[error("exercised units exceed the series")]
    ExercisedUnitsExceedSeries,

    #[error("settlement cost overflow")]
    SettlementCostOverflow,

    #[error("cost amount overflow")]
    CostAmountOverflow,

    #[error("assetVaultsCount undecodable")]
    AssetVaultsCountUndecodable,

    #[error("isoCode undecodable")]
    IsoCodeUndecodable,

    #[error("ERC20 decimals undecodable")]
    Erc20DecimalsUndecodable,
}

impl From<outbe_common::pow::PowError> for IntexFactoryError {
    fn from(value: outbe_common::pow::PowError) -> Self {
        match value {
            outbe_common::pow::PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}

impl From<IntexFactoryError> for PrecompileError {
    fn from(err: IntexFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

impl From<RoundingError> for IntexFactoryError {
    fn from(err: RoundingError) -> Self {
        Self::Rounding(err)
    }
}
