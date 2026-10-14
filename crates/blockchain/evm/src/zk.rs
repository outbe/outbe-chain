//! Chain-owned canonical ZK verifier precompile.

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::dispatch::reject_value;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_zk::wire::decode_verify_call;
use outbe_zk_backend::barretenberg::{Barretenberg, RawVerifier};
use tracing::trace;

pub use outbe_primitives::storage::gas::ZK_VERIFY_GAS;

/// Selectors on the UltraHonkKeccak verifier precompile (`0xEE08`) that accept
/// native value.
pub const GROTH16_PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Dispatch the UltraHonkKeccak verifier precompile (`0xEE08`).
pub fn dispatch_groth16(
    _storage: StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    Ok(Bytes::copy_from_slice(&zk_verify(data)?))
}

/// Base gas charged before [`dispatch_groth16`].
pub fn groth16_base_gas(_input: &[u8]) -> u64 {
    ZK_VERIFY_GAS
}

pub(crate) fn zk_verify(input: &[u8]) -> Result<[u8; 32]> {
    let call =
        decode_verify_call(input).map_err(|error| PrecompileError::Revert(error.to_string()))?;
    let Some(descriptor) = outbe_zk::circuit_by_hash(&call.circuit_hash) else {
        trace!(circuit_hash = ?call.circuit_hash, "zk_verify: unknown circuit_hash");
        return Ok([0u8; 32]);
    };

    let verified = Barretenberg::default()
        .verify_combined(descriptor.vk_bytes, call.combined_proof)
        .map_err(|error| {
            PrecompileError::Revert(format!("zk verification backend failed: {error}"))
        })?;
    trace!(circuit = descriptor.label, verified, "zk_verify");

    let mut output = [0u8; 32];
    output[31] = u8::from(verified);
    Ok(output)
}
