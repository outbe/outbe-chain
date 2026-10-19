//! BN254 Poseidon2 with the Barretenberg permutation and Noir sponge framing.
//!
//! ```rust
//! use ark_bn254::Fr;
//! use outbe_poseidon::{Poseidon2, PoseidonHasher};
//! let hash = Poseidon2::<Fr>::new().hash(&[Fr::from(1u64), Fr::from(2u64)]).unwrap();
//! ```

use ark_ff::PrimeField;

pub mod errors;
pub mod poseidon2;
pub use errors::{Error, PoseidonError};
pub use poseidon2::Poseidon2;

pub trait PoseidonHasher<F: PrimeField> {
    /// Hash field elements using the Poseidon2 sponge.
    fn hash(&mut self, inputs: &[F]) -> Result<F, PoseidonError>;
}
