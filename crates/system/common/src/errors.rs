use alloy_primitives::U256;
use outbe_primitives::error::PrecompileError;

use crate::stablecoin::CurrencyCodeError;

/// Errors returned by [`crate::math`] helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PrimitivesMathError {
    #[error("unsupported asset decimals")]
    UnsupportedAssetDecimals,
    #[error("principal and valuation must be positive")]
    PrincipalAndValuationMustBePositive,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    #[error("value rounds to zero")]
    ValueRoundsToZero,
    #[error("scaled_math: multiplication overflow")]
    ScaledMathMultiplicationOverflow,
    #[error("scaled_math: division by zero")]
    ScaledMathDivisionByZero,
    #[error("scaled_math: ceiling overflow")]
    ScaledMathCeilingOverflow,
    #[error("lb_math: log2 i256 conversion")]
    LbMathLog2I256Conversion,
    #[error("lb_math: log2 underflow (x == 0)")]
    LbMathLog2Underflow,
    #[error("lb_math: log2 n widen")]
    LbMathLog2NWiden,
    #[error("lb_math: pow exponent out of LB range (|y| = {abs_y})")]
    LbMathPowExponentOutOfRange { abs_y: u32 },
    #[error("lb_math: pow underflow (x = {x}, y = {y})")]
    LbMathPowUnderflow { x: U256, y: i32 },
    #[error("lb_math: mul_div by zero")]
    LbMathMulDivByZero,
    #[error("lb_math: mul_div overflow")]
    LbMathMulDivOverflow,
    #[error("lb_math: mul_shift overflow")]
    LbMathMulShiftOverflow,
    #[error("lb_math: log2(base) == 0")]
    LbMathLog2BaseZero,
    #[error("lb_math: REAL_ID_SHIFT widen")]
    LbMathRealIdShiftWiden,
    #[error("lb_math: id widen")]
    LbMathIdWiden,
    #[error("lb_math: bin id out of range (id = {id})")]
    LbMathBinIdOutOfRange { id: u32 },
    #[error("lb_math: tree invariant broken (non-zero level word resolved to empty child word)")]
    LbMathTreeInvariantBroken,
}

impl From<PrimitivesMathError> for PrecompileError {
    fn from(err: PrimitivesMathError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::call_bins`] index mutations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CallBinError {
    #[error("call bin index: entry is already indexed")]
    EntryAlreadyIndexed,
    #[error("call bin index: bin entry count overflow")]
    BinEntryCountOverflow,
    #[error("call bin index: slot does not hold its entry")]
    SlotDoesNotHoldEntry,
    #[error("call bin index: slot past the end of its bin")]
    SlotPastEndOfBin,
}

impl From<CallBinError> for PrecompileError {
    fn from(err: CallBinError) -> Self {
        Self::Revert(err.to_string())
    }
}

impl From<CurrencyCodeError> for PrecompileError {
    fn from(value: CurrencyCodeError) -> Self {
        Self::Revert(value.to_string())
    }
}
