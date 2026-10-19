//! This Poseidon2 hash supports generic prime fields and includes BN254 support.
//! Its BN254 results match Noir's in-circuit results bit for bit:
//!
//! * [`Poseidon2::permutation`] matches `bn254_blackbox_solver::poseidon2`, the `Poseidon2Permutation` blackbox.
//! * The [`PoseidonHasher`](crate::PoseidonHasher) sponge matches `std::hash::poseidon2`.
//!
//! The sponge uses these steps:
//!
//! 1. Set the capacity to `iv = len << 64`.
//! 2. Absorb the input in blocks of `RATE` elements.
//! 3. Apply the final permutation.
//! 4. Return `state[0]`.
//!
//! The parameters are width `t = 4`, S-box `x^5`, `R_F = 8` full rounds, and `R_P = 56` partial rounds.
//! The private `constants` submodule contains exact copies of the BN254 constants.
//! Poseidon2 for any other field requires a [`Poseidon2Config`](crate::poseidon2::Poseidon2Config) for that field.

use ark_ff::PrimeField;

use crate::{PoseidonError, PoseidonHasher};

mod constants;
pub use constants::Poseidon2Config;

/// Sponge rate. The state width is 4, with a rate of 3 and a capacity of 1.
const RATE: usize = 3;
/// Permutation state width.
const T: usize = 4;

/// This Poseidon2 hasher supports the generic prime field `F`.
///
/// The round constants and matrix diagonal depend on the field.
/// The hasher borrows a `'static` [`Poseidon2Config<F>`](Poseidon2Config) for these values.
/// [`Poseidon2::<Fr>::new`](Poseidon2::new) provides BN254 support.
/// Its output matches Noir's in-circuit `poseidon2` bit for bit.
/// For another field, use [`with_config`](Poseidon2::with_config) with constants for that field.
#[derive(Clone, Copy)]
pub struct Poseidon2<F: PrimeField> {
    config: &'static Poseidon2Config<F>,
}

impl<F: PrimeField> Poseidon2<F> {
    /// This constructor returns a Poseidon2 hasher with the given constants for the field.
    pub fn with_config(config: &'static Poseidon2Config<F>) -> Self {
        Self { config }
    }

    /// This S-box raises `x` to the fifth power.
    #[inline(always)]
    fn single_box(x: F) -> F {
        let s = x * x; // x^2
        s * s * x // x^5
    }

    fn s_box(state: &mut [F; T]) {
        for x in state.iter_mut() {
            *x = Self::single_box(*x);
        }
    }

    fn add_round_constants(&self, state: &mut [F; T], round: usize) {
        for (s, c) in state
            .iter_mut()
            .zip(self.config.round_constant[round].iter())
        {
            *s += *c;
        }
    }

    /// The external linear layer uses Barretenberg's 4×4 matrix.
    /// Each step matches the Barretenberg / `bn254_blackbox_solver` implementation.
    fn matrix_multiplication_4x4(input: &mut [F; T]) {
        let t0 = input[0] + input[1];
        let t1 = input[2] + input[3];
        let mut t2 = input[1] + input[1];
        t2 += t1;
        let mut t3 = input[3] + input[3];
        t3 += t0;
        let mut t4 = t1 + t1;
        t4 += t4;
        t4 += t3;
        let mut t5 = t0 + t0;
        t5 += t5;
        t5 += t2;
        let t6 = t3 + t5;
        let t7 = t2 + t4;
        input[0] = t6;
        input[1] = t5;
        input[2] = t7;
        input[3] = t4;
    }

    /// The internal linear layer uses a diagonal matrix and the sum of the state elements.
    fn internal_m_multiplication(&self, state: &mut [F; T]) {
        let mut sum = F::ZERO;
        for x in state.iter() {
            sum += *x;
        }
        let diag = &self.config.internal_matrix_diagonal;
        for (i, x) in state.iter_mut().enumerate() {
            *x *= diag[i];
            *x += sum;
        }
    }

    /// This method applies the Poseidon2 permutation to a state with four elements.
    pub fn permutation(&self, inputs: &[F; T]) -> [F; T] {
        let cfg = self.config;
        let mut state = *inputs;
        Self::matrix_multiplication_4x4(&mut state);

        // Apply the first half of the external (full) rounds.
        let rf_first = (cfg.rounds_f / 2) as usize;
        for r in 0..rf_first {
            self.add_round_constants(&mut state, r);
            Self::s_box(&mut state);
            Self::matrix_multiplication_4x4(&mut state);
        }

        // Apply the internal (partial) rounds, with the S-box on element 0 only.
        let p_end = rf_first + cfg.rounds_p as usize;
        for r in rf_first..p_end {
            state[0] += cfg.round_constant[r][0];
            state[0] = Self::single_box(state[0]);
            self.internal_m_multiplication(&mut state);
        }

        // Apply the second half of the external rounds.
        let num_rounds = (cfg.rounds_f + cfg.rounds_p) as usize;
        for r in p_end..num_rounds {
            self.add_round_constants(&mut state, r);
            Self::s_box(&mut state);
            Self::matrix_multiplication_4x4(&mut state);
        }
        state
    }

    /// This method hashes `inputs` with a fixed-arity Poseidon2 sponge that matches Noir's `std::hash::poseidon2`.
    /// The sponge uses these steps:
    ///
    /// 1. Set the capacity to `iv = len << 64`.
    /// 2. Absorb the input in blocks of [`RATE`] elements.
    /// 3. Apply the final permutation.
    /// 4. Return `state[0]`.
    fn sponge(&self, inputs: &[F]) -> F {
        let len = inputs.len();
        // iv = len << 64
        let iv = F::from(len as u64) * F::from(1u128 << 64);
        let mut state = [F::ZERO, F::ZERO, F::ZERO, iv];

        let full = len / RATE;
        for block in 0..full {
            state[0] += inputs[block * RATE];
            state[1] += inputs[block * RATE + 1];
            state[2] += inputs[block * RATE + 2];
            state = self.permutation(&state);
        }
        for i in 0..(len % RATE) {
            state[i] += inputs[full * RATE + i];
        }
        self.permutation(&state)[0]
    }
}

impl Poseidon2<ark_bn254::Fr> {
    /// This constructor returns a BN254 Poseidon2 hasher.
    /// Its output matches Noir's in-circuit `poseidon2` bit for bit.
    pub fn new() -> Self {
        Self::with_config(&constants::BN254_CONFIG)
    }
}

impl Default for Poseidon2<ark_bn254::Fr> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: PrimeField> PoseidonHasher<F> for Poseidon2<F> {
    /// This method hashes `inputs` with the Poseidon2 sponge.
    /// The sponge cannot fail, so the method always returns `Ok`.
    /// The `Result` type preserves the shared protocol hasher trait.
    fn hash(&mut self, inputs: &[F]) -> Result<F, PoseidonError> {
        Ok(self.sponge(inputs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_bn254::Fr;
    use ark_ff::MontFp;

    /// This test checks the permutation against the reference value from `bn254_blackbox_solver::smoke_test`.
    /// The reference value comes from the C++ Barretenberg implementation.
    #[test]
    fn permutation_kat_zeroes() {
        let out = Poseidon2::<Fr>::new().permutation(&[Fr::from(0u64); 4]);
        let expected = [
            MontFp!("0x18dfb8dc9b82229cff974efefc8df78b1ce96d9d844236b496785c698bc6732e"),
            MontFp!("0x095c230d1d37a246e8d2d5a63b165fe0fade040d442f61e25f0590e5fb76f839"),
            MontFp!("0x0bb9545846e1afa4fa3c97414a60a20fc4949f537a68cceca34c5ce71e28aa59"),
            MontFp!("0x18a4f34c9c6f99335ff7638b82aeed9018026618358873c982bbdde265b2ed6d"),
        ];
        assert_eq!(
            out, expected,
            "Poseidon2 permutation must match barretenberg"
        );
    }

    /// This test checks the sponge against reference values from `nargo execute`.
    /// It uses the same `poseidon2_permutation` sequence as the circuits, with `iv = len << 64`.
    /// For `hash_3`, the sponge absorbs the input and applies a permutation.
    /// It then applies another permutation to return the hash.
    /// This test checks that the hash outside the circuit matches the circuit's result.
    #[test]
    fn hash_kat_from_noir() {
        let mut hasher = Poseidon2::<Fr>::new();
        let h2 = hasher.hash(&[Fr::from(1u64), Fr::from(2u64)]).unwrap();
        assert_eq!(
            h2,
            MontFp!("0x038682aa1cb5ae4e0a3f13da432a95c77c5c111f6f030faf9cad641ce1ed7383"),
            "hash_2 must match noir"
        );
        let h3 = hasher
            .hash(&[Fr::from(1u64), Fr::from(2u64), Fr::from(3u64)])
            .unwrap();
        assert_eq!(
            h3,
            MontFp!("0x16f5da1a6b40e7d71bcdf29687e7908cdf74da44c09058fe36a0a99e269c6972"),
            "hash_3 must match noir"
        );
    }

    /// With the included BN254 constants, `with_config` must match `new`.
    #[test]
    fn with_config_matches_new() {
        let inputs = [Fr::from(7u64), Fr::from(8u64), Fr::from(9u64)];
        let via_new =
            Poseidon2::<Fr>::new().permutation(&[inputs[0], inputs[1], inputs[2], Fr::from(0u64)]);
        let via_cfg = Poseidon2::with_config(&constants::BN254_CONFIG).permutation(&[
            inputs[0],
            inputs[1],
            inputs[2],
            Fr::from(0u64),
        ]);
        assert_eq!(via_new, via_cfg);
    }
}
