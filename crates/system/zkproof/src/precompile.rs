//! Outbe `DispatchFn` adapters for the two precompiles, plus per-call
//! base-gas helpers consumed by the EVM precompile registry.

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::dispatch::reject_value;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{POSEIDON_GAS_BASE, POSEIDON_GAS_PER_INPUT, ZK_VERIFY_GAS};
use crate::{poseidon, verify};

/// Selectors on the Poseidon precompile (`0xEE07`) that accept native value. The
/// route table binds this to that address's `ValuePolicy` at compile time.
///
/// One constant per address: sharing a single list across both zkproof routes
/// would make populating one silently authorize the other. Both dispatchers take
/// raw hash/proof bytes rather than an ABI selector, so a populated list here
/// could not be enforced module-side either.
pub const POSEIDON_PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Selectors on the UltraHonkKeccak verifier precompile (`0xEE08`) that accept
/// native value. See [`POSEIDON_PAYABLE_SELECTORS`].
pub const GROTH16_PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Dispatch for the Poseidon-BN254 hash precompile (`0xEE07`).
pub fn dispatch_poseidon(
    _storage: StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    let hash = poseidon::poseidon_hash(data)?;
    Ok(Bytes::copy_from_slice(&hash))
}

/// Dispatch for the UltraHonkKeccak verifier precompile (`0xEE08`).
pub fn dispatch_groth16(
    _storage: StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    let out = verify::zk_verify(data)?;
    Ok(Bytes::copy_from_slice(&out))
}

/// Base gas charged by the registry before invoking [`dispatch_poseidon`].
pub fn poseidon_base_gas(input: &[u8]) -> u64 {
    let n = (input.len() / 32) as u64;
    POSEIDON_GAS_BASE + POSEIDON_GAS_PER_INPUT * n
}

/// Base gas charged by the registry before invoking [`dispatch_groth16`].
pub fn groth16_base_gas(_input: &[u8]) -> u64 {
    ZK_VERIFY_GAS
}
