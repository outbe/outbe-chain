//! Poseidon2 hash errors.

/// Typed failures from the native protocol hasher.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PoseidonError {
    #[error(transparent)]
    Runtime(#[from] outbe_barretenberg::RuntimeError),
    #[error("native Poseidon field {index} has {actual} bytes; expected 32")]
    ResponseLength { index: usize, actual: usize },
    #[error("native Poseidon field {index} is not a canonical BN254 scalar")]
    NoncanonicalField { index: usize },
    #[error("Poseidon input length exceeds u64")]
    InputLength,
}

/// Alias for [`PoseidonError`].
pub type Error = PoseidonError;
