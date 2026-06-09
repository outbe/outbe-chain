//! Outbe `DispatchFn` adapters for the two precompiles, plus per-call
//! base-gas helpers consumed by the EVM precompile registry.

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::dispatch::reject_value;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{POSEIDON_GAS_BASE, POSEIDON_GAS_PER_INPUT, ZK_VERIFY_GAS};
use crate::{poseidon, verify};

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
