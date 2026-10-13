//! Tribute circuit statements, claim hashing, and verifier wire decoding.
//!
//! Noir sources and frozen artifacts live in the repository `circuits/` tree.
//! This crate embeds them at build time and does not link the Barretenberg FFI.

#![forbid(unsafe_code)]

pub mod claim;
pub mod codec;
pub mod demo_tribute;
pub mod error;
pub mod primitive;
pub mod protocol;
pub mod wire;

pub use ark_bn254::Fr;
pub use codec::{FieldElement, FieldEncode};
pub use error::Error;
pub use protocol::zk::CircuitId;

/// Depth of the TributeDraft commitment tree. Matches `demo_tribute` and
/// `niflheim_tribute`.
pub const INCLUSION_DEPTH: usize = 32;

/// One L2 circuit selected by the offerTribute version string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L2CircuitVersion {
    /// Wire selector (`offerTribute.version`), not an independent package semver.
    pub version: &'static str,
    /// `keccak256` of the decoded ACIR bytecode.
    pub circuit_hash: [u8; 32],
    /// `keccak256` of [`Self::vk_bytes`].
    pub vk_hash: [u8; 32],
    /// UltraHonkKeccak verification key.
    pub vk_bytes: &'static [u8],
    /// Nargo package name with dots, for example `demo.tribute`.
    pub label: &'static str,
    /// Proof system the key verifies under.
    pub proof_system: &'static str,
}

/// Circuits enabled for one L2 chain, in manifest order.
#[derive(Clone, Copy, Debug)]
pub struct L2ChainEntry {
    pub chain_id: u64,
    pub circuits: &'static [L2CircuitVersion],
}

/// Enabled versions for an L2 chain, or an empty slice when none are configured.
pub fn l2_circuits(chain_id: u64) -> &'static [L2CircuitVersion] {
    noir::L2_CIRCUITS_REGISTRY
        .iter()
        .find(|entry| entry.chain_id == chain_id)
        .map(|entry| entry.circuits)
        .unwrap_or(&[])
}

/// Shipped circuit whose bytecode hash matches, if any.
///
/// Emit and paynote hashes are absent: the verifier precompile returns zero
/// for them. This is not an L2 chain binding.
pub fn circuit_by_hash(circuit_hash: &[u8; 32]) -> Option<&'static L2CircuitVersion> {
    noir::L2_CIRCUITS_REGISTRY
        .iter()
        .flat_map(|entry| entry.circuits)
        .find(|circuit| &circuit.circuit_hash == circuit_hash)
}

/// Witness types and identity constants generated from frozen artifacts.
#[allow(dead_code)]
#[allow(clippy::all)]
pub mod noir {
    include!(concat!(env!("OUT_DIR"), "/noir_generated.rs"));
}
