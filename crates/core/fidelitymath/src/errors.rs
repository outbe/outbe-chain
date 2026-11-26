//! Error types for fixed-point RCFI decay math.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FidelityMathError {
    #[error("fidelity math arithmetic overflow")]
    ArithmeticOverflow,
}
