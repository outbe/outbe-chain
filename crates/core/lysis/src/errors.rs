//! Error types for the Lysis module.

use alloy_primitives::U256;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::program_v1::FidelityPhaseV1;

/// Revert errors that the Lysis algorithm returns.
#[derive(Debug, Error)]
pub enum LysisError {
    #[error("lysis: fp_root intermediate overflow")]
    FpRootIntermediateOverflow,

    #[error("lysis: fp_root result exceeds U256")]
    FpRootResultExceedsU256,

    #[error("lysis: U256 -> I256 conversion overflow ({0})")]
    U256ToI256Overflow(U256),
}

impl From<LysisError> for PrecompileError {
    fn from(err: LysisError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

/// Typed arithmetic and derivation failure reasons in Lysis V1.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum LysisArithmeticErrorV1 {
    #[error("Fidelity league population overflow")]
    FidelityLeaguePopulationOverflow,

    #[error("Nod entry price out of bounds at {ordinal}")]
    NodEntryPriceOutOfBounds { ordinal: usize },

    #[error("Fidelity share sum overflow")]
    FidelityShareSumOverflow,

    #[error("Fidelity share remainder overflow")]
    FidelityShareRemainderOverflow,

    #[error("maximum Gratis fraction overflow")]
    MaximumGratisFractionOverflow,

    #[error("projected Gratis allocation overflow")]
    ProjectedGratisAllocationOverflow,

    #[error("{0}")]
    EntityIdDerivation(String),

    #[error("{0}")]
    Math(String),
}

/// Deterministic errors that the Lysis V1 semantic program returns.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ProgramErrorV1 {
    #[error("Lysis V1 input is empty")]
    EmptyInput,

    #[error("Lysis V1 input is not raw-ID ordered at {ordinal}")]
    NonCanonicalInput { ordinal: usize },

    #[error("Lysis V1 duplicate input at {ordinal}")]
    DuplicateInput { ordinal: usize },

    #[error("Lysis V1 WorldwideDay mismatch at {ordinal}")]
    WorldwideDayMismatch { ordinal: usize },

    #[error("Lysis V1 invalid Tribute identity at {ordinal}")]
    InvalidTributeIdentity { ordinal: usize },

    #[error("Lysis V1 total nominal overflow at {ordinal}")]
    TotalNominalOverflow { ordinal: usize },

    #[error("Lysis V1 total nominal is zero")]
    ZeroTotalNominal,

    #[error("Lysis V1 Fidelity {phase:?} unavailable at {ordinal}")]
    FidelityUnavailable {
        ordinal: usize,
        phase: FidelityPhaseV1,
    },

    #[error("Lysis V1 Fidelity mismatch at {ordinal}: {first} != {second}")]
    FidelityMismatch {
        ordinal: usize,
        first: u16,
        second: u16,
    },

    #[error("Lysis V1 Oracle {currency} unavailable at {ordinal}")]
    EntryPriceUnavailable { ordinal: usize, currency: u16 },

    #[error("Lysis V1 arithmetic: {reason}")]
    Arithmetic { reason: LysisArithmeticErrorV1 },

    #[error("Lysis V1 zero Gratis load at {ordinal}")]
    ZeroGratisLoad { ordinal: usize },

    #[error(
        "Lysis V1 zero monetary cost at {ordinal}: entry price {entry_price_minor}, Gratis load {gratis_load_minor}"
    )]
    ZeroCost {
        ordinal: usize,
        /// The entry price operand for the cost formula.
        entry_price_minor: U256,
        gratis_load_minor: U256,
    },

    #[error("Lysis V1 Gratis load exceeds remaining at {ordinal}")]
    GratisLoadExceedsRemaining { ordinal: usize },

    #[error("Lysis V1 invalid Nod target at {ordinal}")]
    InvalidNodTarget { ordinal: usize },

    #[error("Lysis V1 output count mismatch")]
    OutputCountMismatch,
}

impl From<LysisArithmeticErrorV1> for ProgramErrorV1 {
    fn from(reason: LysisArithmeticErrorV1) -> Self {
        Self::Arithmetic { reason }
    }
}

impl ProgramErrorV1 {
    /// Creates an entity-ID derivation arithmetic error.
    pub fn entity_id_derivation(
        err: outbe_primitives::wwd_entity_id::EntityIdDerivationError,
    ) -> Self {
        Self::Arithmetic {
            reason: LysisArithmeticErrorV1::EntityIdDerivation(err.to_string()),
        }
    }

    /// Creates a math error from a precompile math failure.
    pub fn math(err: PrecompileError) -> Self {
        Self::Arithmetic {
            reason: LysisArithmeticErrorV1::Math(err.to_string()),
        }
    }
}

impl From<ProgramErrorV1> for PrecompileError {
    fn from(error: ProgramErrorV1) -> Self {
        PrecompileError::BodyReadCorruption(error.to_string())
    }
}
