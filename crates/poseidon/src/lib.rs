//! Outbe protocol sponge over native Barretenberg Poseidon2 (BN254 only).
//! Complete rate blocks retain an extra final permutation, unlike the upstream
//! standard sponge. Hashing does not initialize or fetch a proving SRS.
//!
//! ```rust
//! use ark_bn254::Fr;
//! use outbe_poseidon::{Poseidon2, PoseidonHasher};
//! let hash = Poseidon2::new().hash(&[Fr::from(1u64), Fr::from(2u64)])?;
//! # Ok::<(), outbe_poseidon::PoseidonError>(())
//! ```

use ark_bn254::Fr;

pub mod errors;
pub mod poseidon2;
pub use errors::{Error, PoseidonError};
pub use poseidon2::Poseidon2;

pub trait PoseidonHasher {
    /// Hash field elements using the Poseidon2 sponge.
    fn hash(&mut self, inputs: &[Fr]) -> Result<Fr, PoseidonError>;
}

#[cfg(feature = "test-utils")]
pub mod testing;
