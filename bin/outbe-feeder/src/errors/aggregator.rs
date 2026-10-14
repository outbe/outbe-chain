//! Error types for price aggregation and deviation filtering math.

use thiserror::Error;

/// Errors that occur during VWAP, TVWAP, or sigma deviation calculations.
#[derive(Debug, Error)]
pub(crate) enum AggregatorError {
    /// The sum of prices overflowed `U512` during mean calculation.
    #[error("deviation mean sum overflow")]
    DeviationMeanSumOverflow,
    /// The sum of squared deviations overflowed `U1024`.
    #[error("deviation square sum overflow")]
    DeviationSquareSumOverflow,
    /// A wide integer value exceeds the `U256` range.
    #[error("{label} exceeds U256")]
    ValueExceedsU256 {
        /// The name of the value that overflowed `U256`.
        label: &'static str,
    },
    /// The product of price and volume weight overflowed `U512`.
    #[error("price-volume product overflow")]
    PriceVolumeProductOverflow,
    /// The sum of weighted prices overflowed `U1024`.
    #[error("price-volume sum overflow")]
    PriceVolumeSumOverflow,
    /// The sum of weights overflowed `U512`.
    #[error("weight sum overflow")]
    WeightSumOverflow,
    /// The sum of volumes overflowed `U512`.
    #[error("volume sum overflow")]
    VolumeSumOverflow,
}
