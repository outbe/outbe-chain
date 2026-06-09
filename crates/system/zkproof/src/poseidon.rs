//! Poseidon-BN254 hash core.
//!
//! ABI mirrors the wallet / CLI side so the on-chain hash and off-chain
//! hash agree byte-for-byte for the same inputs.

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};
use tracing::trace;

use crate::constants::MAX_INPUTS;
use crate::errors::ZkProofError;

/// Compute Poseidon-BN254 hash over `N` BE-encoded uint256 field
/// elements packed into `input` (no length prefix). `1 ≤ N ≤ 12`.
pub fn poseidon_hash(input: &[u8]) -> Result<[u8; 32], ZkProofError> {
    if input.is_empty() {
        return Err(ZkProofError::EmptyInput);
    }
    if !input.len().is_multiple_of(32) {
        return Err(ZkProofError::UnalignedInput(input.len()));
    }
    let n = input.len() / 32;
    if n > MAX_INPUTS {
        return Err(ZkProofError::TooManyInputs(n));
    }

    let inputs: Vec<Fr> = input.chunks(32).map(Fr::from_be_bytes_mod_order).collect();

    let mut poseidon =
        Poseidon::<Fr>::new_circom(n).map_err(|e| ZkProofError::SetupFailed(e.to_string()))?;
    let hash = poseidon
        .hash(&inputs)
        .map_err(|e| ZkProofError::HashFailed(e.to_string()))?;

    let be = hash.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    let off = 32 - be.len().min(32);
    out[off..].copy_from_slice(&be[be.len().saturating_sub(32)..]);

    trace!(n_inputs = n, "poseidon precompile");
    Ok(out)
}
