//! Poseidon2 hash errors.

/// The Poseidon2 sponge never fails. The shared hasher trait requires an error type.
pub type PoseidonError = std::convert::Infallible;

/// Alias for [`PoseidonError`].
pub type Error = PoseidonError;
