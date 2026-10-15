//! Outbe protocol sponge over Barretenberg Poseidon2, for BN254 scalars only.
//! Unlike the upstream standard sponge, a complete rate block is followed by
//! an unconditional final permutation. In particular lengths three and six
//! intentionally differ from `poseidon2_hash`.

use crate::{PoseidonError, PoseidonHasher};
use ark_bn254::Fr;
use ark_ff::{BigInt, BigInteger, PrimeField};
use outbe_barretenberg::{with_api, NativeApi, RuntimeError};

const RATE: usize = 3;

/// Lightweight BN254 protocol hasher. Native setup occurs in fallible operations.
#[derive(Clone, Copy, Default)]
pub struct Poseidon2;

impl Poseidon2 {
    pub const fn new() -> Self {
        Self
    }

    /// Apply the native permutation within its own shared dispatcher scope.
    pub fn permutation(&self, inputs: &[Fr; 4]) -> Result<[Fr; 4], PoseidonError> {
        with_api(|api| permutation_scoped(api, inputs))
    }
}

/// Canonical integer representation, not Montgomery memory or Ark serialization.
pub(crate) fn encode(value: Fr) -> Vec<u8> {
    value.into_bigint().to_bytes_be()
}

fn decode(bytes: &[u8], index: usize) -> Result<Fr, PoseidonError> {
    if bytes.len() != 32 {
        return Err(PoseidonError::ResponseLength {
            index,
            actual: bytes.len(),
        });
    }
    let mut limbs = [0_u64; 4];
    for (limb, chunk) in limbs.iter_mut().rev().zip(bytes.chunks_exact(8)) {
        let mut word = [0_u8; 8];
        word.copy_from_slice(chunk);
        *limb = u64::from_be_bytes(word);
    }
    // Checked integer construction rejects noncanonical values without reduction.
    Fr::from_bigint(BigInt::new(limbs)).ok_or(PoseidonError::NoncanonicalField { index })
}

fn permutation_scoped(api: &mut NativeApi, inputs: &[Fr; 4]) -> Result<[Fr; 4], PoseidonError> {
    #[cfg(feature = "test-utils")]
    crate::testing::before_permutation()?;
    let response = api
        .poseidon2_permutation(inputs.map(encode))
        .map_err(|error| RuntimeError::Backend(std::sync::Arc::new(error)))?;
    Ok([
        decode(&response.outputs[0], 0)?,
        decode(&response.outputs[1], 1)?,
        decode(&response.outputs[2], 2)?,
        decode(&response.outputs[3], 3)?,
    ])
}

impl PoseidonHasher for Poseidon2 {
    fn hash(&mut self, inputs: &[Fr]) -> Result<Fr, PoseidonError> {
        let len = u64::try_from(inputs.len()).map_err(|_| PoseidonError::InputLength)?;
        with_api(|api| {
            let iv = Fr::from(len) * Fr::from(1u128 << 64);
            let mut state = [Fr::from(0u64), Fr::from(0u64), Fr::from(0u64), iv];
            let mut blocks = inputs.chunks_exact(RATE);
            for block in &mut blocks {
                for i in 0..RATE {
                    state[i] += block[i];
                }
                state = permutation_scoped(api, &state)?;
            }
            for (i, value) in blocks.remainder().iter().enumerate() {
                state[i] += value;
            }
            Ok(permutation_scoped(api, &state)?[0])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::MontFp;

    #[test]
    fn permutation_kat_zeroes() {
        let out = Poseidon2::new().permutation(&[Fr::from(0u64); 4]).unwrap();
        let expected = [
            MontFp!("0x18dfb8dc9b82229cff974efefc8df78b1ce96d9d844236b496785c698bc6732e"),
            MontFp!("0x095c230d1d37a246e8d2d5a63b165fe0fade040d442f61e25f0590e5fb76f839"),
            MontFp!("0x0bb9545846e1afa4fa3c97414a60a20fc4949f537a68cceca34c5ce71e28aa59"),
            MontFp!("0x18a4f34c9c6f99335ff7638b82aeed9018026618358873c982bbdde265b2ed6d"),
        ];
        assert_eq!(out, expected);
    }

    // Existing nargo execute fixtures, retaining the circuit's exact framing.
    #[test]
    fn hash_kat_from_noir() {
        assert_eq!(
            Poseidon2::new()
                .hash(&[Fr::from(1u64), Fr::from(2u64)])
                .unwrap(),
            MontFp!("0x038682aa1cb5ae4e0a3f13da432a95c77c5c111f6f030faf9cad641ce1ed7383")
        );
        assert_eq!(
            Poseidon2::new()
                .hash(&[Fr::from(1u64), Fr::from(2u64), Fr::from(3u64)])
                .unwrap(),
            MontFp!("0x16f5da1a6b40e7d71bcdf29687e7908cdf74da44c09058fe36a0a99e269c6972")
        );
    }

    #[test]
    fn canonical_response_validation() {
        for value in [Fr::from(0u64), Fr::from(1u64), -Fr::from(1u64)] {
            assert_eq!(encode(value).len(), 32);
            assert_eq!(decode(&encode(value), 0).unwrap(), value);
        }
        assert!(matches!(
            decode(&Fr::MODULUS.to_bytes_be(), 0),
            Err(PoseidonError::NoncanonicalField { .. })
        ));
        assert!(matches!(
            decode(&[255; 32], 0),
            Err(PoseidonError::NoncanonicalField { .. })
        ));
        for len in [0, 1, 31, 33, 64] {
            assert!(matches!(
                decode(&vec![0; len], 0),
                Err(PoseidonError::ResponseLength { .. })
            ));
        }
    }
}
