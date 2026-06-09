//! Stateless ZK proof + Poseidon hash precompiles backed by the
//! `outbe-circuits` canonical circuit table and Barretenberg FFI.
//!
//! Two precompiles are exposed:
//!
//! - `0xEE07` Poseidon-BN254 hash (raw bytes in → 32-byte hash out).
//! - `0xEE08` UltraHonkKeccak verifier (`abi.encode(bytes32 circuit_hash,
//!   bytes proof)` in → 32 bytes 0/1 out).
//!
//! Both are stateless; the `StorageHandle` argument is ignored by the
//! dispatch functions.

pub mod constants;
pub mod errors;
pub mod poseidon;
pub mod precompile;
pub mod verify;

#[cfg(test)]
mod tests;

pub use errors::ZkProofError;
pub use precompile::{dispatch_groth16, dispatch_poseidon, groth16_base_gas, poseidon_base_gas};
pub use verify::init_crs;
