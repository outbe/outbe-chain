//! This Barretenberg backend generates and verifies UltraHonkKeccak proofs for the canonical circuits.
//! It uses the `barretenberg-rs` foreign function interface (FFI) with `libbb-external`.
//! It executes on the device without a `bb` subprocess.
//!
//! Like the reference `outbe-zk-circuit-noir` backend, this backend uses the keccak oracle hash for on-chain EVM verification.
//! Its optional low-memory mode stores polynomials in files.
//! This mode uses much less RAM but takes approximately twice as long to generate proofs.
//! The committed bytecode and the output from `WitnessStack::serialize()` use gzip compression.
//! The native API requires uncompressed msgpack bytecode and witness data.
//!
//! [`witness::solved_witness`] solves the witness with ACVM.
//! This backend sends the solved witness, committed bytecode, and verification key (VK) to `circuit_prove`.
//! [`CircuitId`] supplies the bytecode and VK.
//! For verification, this backend derives the public inputs from the claim and calls `circuit_verify`.
//! The pinned `barretenberg-rs` version is `5.0.0-nightly.20260522`.
//! This version matches the `bb` version that writes the committed VKs with `-t evm-no-zk`.

use barretenberg_rs::generated_types::{CircuitInput, ProofSystemSettings};
use base64::Engine;
use flate2::read::GzDecoder;
use std::io::Read;

use outbe_zk::error::Error;
use outbe_zk::protocol::zk::{Circuit, CircuitId, ProofGenerator, ProofVerifier};

use crate::errors::BackendProofError;
use crate::witness;

mod handler;
mod srs;

pub use handler::BarretenbergHandler;

/// Select an explicit G1 `.dat` file for the SRS loader.
/// For example, a mobile wallet can select its bundled SRS file.
/// Call once before proof generation.
/// Builds without `with-network-srs` require this setting because they cannot download SRS data.
/// See [`srs::set_srs_path`].
pub use srs::set_srs_path;

/// The largest CRS capacity that the canonical circuits require.
/// Ownership, Demo Tribute, and Niflheim tribute use a `2^16` proving domain.
/// Their SRS requires `2^16 + 1` G1 points.
/// Emit mint and Paynote spend use a `2^14` domain. PayNote merge uses a `2^15` domain.
pub const CANONICAL_SRS_POINTS: u32 = (1 << 16) + 1;

/// Initialize the process-global CRS with `num_points` G1 points.
/// Native initialization fixes the capacity for the process.
/// If the process uses different circuit sizes, call once at startup with the largest required capacity.
/// Otherwise, a smaller initial capacity prevents larger circuits from generating proofs.
/// The canonical circuits require [`CANONICAL_SRS_POINTS`] (`(1 << 16) + 1`).
///
/// Callers that use one circuit can omit this call.
/// The first proof operation reserves at least the canonical capacity.
pub fn preinit_srs(num_points: u32) -> Result<(), Error> {
    Ok(BarretenbergHandler::initialize(num_points)?.ensure_capacity(num_points)?)
}

/// Initialize the process-global CRS once for every canonical circuit.
///
/// If set, `OUTBE_BB_SRS_PATH` selects a local G1 SRS file.
/// The handler converts panics at the FFI boundary to proof errors.
/// Startup callers can return these errors instead of terminating with a panic.
pub fn init_crs() -> Result<(), Error> {
    preinit_srs(CANONICAL_SRS_POINTS)
}

/// Verify a decoded combined proof against its compile-time circuit identity.
///
/// The protocol decoder must validate the public words before this backend call.
pub fn verify_circuit<C: CircuitId>(combined_proof: &[u8]) -> Result<bool, Error> {
    Barretenberg::default().verify_combined(C::VK_BYTES, combined_proof)
}

/// An UltraHonkKeccak proof in the native field encoding (`Vec<Vec<u8>>`).
/// This type does not contain public inputs. Verification derives them from the claim.
#[derive(Clone, Debug)]
pub struct Proof {
    /// The proof fields.
    pub proof: Vec<Vec<u8>>,
}

/// Return UltraHonk settings with the keccak oracle hash for on-chain EVM verification.
/// These settings match the committed VKs from `bb write_vk -t evm-no-zk`.
fn settings_ultra_honk_keccak(disable_zk: bool) -> ProofSystemSettings {
    ProofSystemSettings {
        ipa_accumulation: false,
        oracle_hash_type: "keccak".to_string(),
        disable_zk,
        optimized_solidity_verifier: false,
    }
}

/// Configure low-memory mode, which stores polynomials in files.
/// This mode uses much less RAM but takes approximately twice as long to generate proofs.
/// Set `BB_SLOW_LOW_MEMORY` before native proof or VK operations.
/// Set `BB_STORAGE_BUDGET` if the caller supplies a budget.
fn configure_memory(enabled: bool, max_storage_usage: Option<u64>) {
    std::env::set_var("BB_SLOW_LOW_MEMORY", if enabled { "1" } else { "0" });
    if let Some(budget) = max_storage_usage {
        std::env::set_var("BB_STORAGE_BUDGET", budget.to_string());
    }
}

/// Decode the circuit's committed bytecode from base64.
/// Decompress the gzip data to produce the raw ACIR buffer for the native API.
fn acir_buffer_uncompressed<C: CircuitId>() -> Result<Vec<u8>, Error> {
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(C::BYTECODE_B64)
        .map_err(BackendProofError::AcirBytecodeBase64)?;
    let mut raw = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut raw)
        .map_err(BackendProofError::AcirDecompress)?;
    Ok(raw)
}

/// Settings for Barretenberg proof generation and verification on the device.
/// Like the reference backend, this type supports `disable_zk`, `low_memory`, and an optional storage budget.
/// The default for `disable_zk` is `false` to protect witnesses that contain secrets.
/// This type stores settings. It does not own a native engine.
/// All native operations use the same process-wide [`BarretenbergHandler`] and block the caller.
#[derive(Clone, Copy, Debug)]
pub struct Barretenberg {
    /// Disable zero-knowledge blinding. The default is `false`.
    /// This option is faster but does not provide zero knowledge.
    /// Use it only for statements that contain public inputs and no secrets.
    pub disable_zk: bool,
    /// Store polynomials in files to reduce RAM use.
    /// Proof generation takes approximately twice as long.
    pub low_memory: bool,
    /// Optional storage budget (bytes) for low-memory mode.
    pub max_storage_usage: Option<u64>,
}

#[allow(clippy::derivable_impls)] // manual impl documents the security-critical disable_zk = false default
impl Default for Barretenberg {
    fn default() -> Self {
        // Ownership and Demo Tribute witnesses contain a secret key.
        // Keep zero knowledge enabled to protect this key.
        // `disable_zk = true` is faster but does not provide zero knowledge.
        // Use it only for statements that contain public inputs and no secrets.
        Self {
            disable_zk: false,
            low_memory: false,
            max_storage_usage: None,
        }
    }
}

impl<C> ProofGenerator<C> for Barretenberg
where
    C: Circuit + CircuitId,
{
    type Proof = Proof;

    fn generate(
        &self,
        witness: &C::Witness,
        public: &C::PublicInputs,
    ) -> Result<Self::Proof, Error> {
        // Pure ACVM solving and decoding stay outside the native worker.
        let solved = witness::solved_witness::<C>(witness, public)?;
        let acir = acir_buffer_uncompressed::<C>()?;
        let circuit = CircuitInput {
            name: C::LABEL.to_string(),
            bytecode: acir,
            verification_key: C::VK_BYTES.to_vec(),
        };
        Ok(Proof {
            proof: BarretenbergHandler::global()?.prove(circuit, solved, *self)?,
        })
    }
}

impl<C> ProofVerifier<C> for Barretenberg
where
    C: Circuit + CircuitId,
{
    type Proof = Proof;

    fn verify(&self, public: &C::PublicInputs, proof: &Self::Proof) -> Result<bool, Error> {
        let public_inputs = witness::public_inputs::<C>(public);
        let acir = acir_buffer_uncompressed::<C>()?;
        let settings = settings_ultra_honk_keccak(self.disable_zk);

        Ok(BarretenbergHandler::global()?.verify(
            C::VK_BYTES.to_vec(),
            public_inputs,
            proof.proof.clone(),
            Some(acir),
            settings,
        )?)
    }
}

/// Verify an UltraHonkKeccak proof against a raw verification key without a compile-time [`Circuit`] type.
///
/// [`ProofVerifier`] requires a concrete circuit type `C`.
/// That type supplies `C::VK_BYTES` and defines how to convert `C::PublicInputs` to field words.
/// This interface instead accepts the verification key and proof as opaque bytes.
///
/// The chain's `zk_verify` precompile uses this interface without a concrete circuit type.
/// The precompile obtains `vk_bytes` from the canonical registry at runtime and accepts the caller's proof.
/// It does not need each circuit's public-input layout.
pub trait RawVerifier {
    /// Verify the self-describing EVM "combined" proof against `vk_bytes`.
    ///
    /// `combined_proof` is the byte layout the on-chain verifier consumes:
    ///
    /// ```text
    /// [4B BE num_public_inputs][32B × num_public_inputs][proof fields …]
    /// ```
    ///
    /// The buffer contains these sections in order:
    /// - A big-endian `u32` count of public inputs.
    /// - That number of 32-byte public-input words.
    /// - The proof as consecutive 32-byte field elements.
    ///
    /// The header identifies the section boundaries. The parser does not inspect the VK.
    ///
    /// Initialize the global CRS before this call. See [`preinit_srs`].
    /// This method does not size or download CRS data.
    /// It returns `Ok(false)` for a correctly formatted but invalid proof.
    /// It returns `Err` for malformed data or a backend failure.
    fn verify_combined(&self, vk_bytes: &[u8], combined_proof: &[u8]) -> Result<bool, Error>;
}

impl RawVerifier for Barretenberg {
    fn verify_combined(&self, vk_bytes: &[u8], combined_proof: &[u8]) -> Result<bool, Error> {
        // Header: big-endian u32 public-input count.
        let header = combined_proof
            .get(..4)
            .ok_or(BackendProofError::CombinedProofHeaderTooShort)?;
        let num_public = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;

        // [4 .. 4 + 32·N] public-input words.
        let pub_end = num_public
            .checked_mul(32)
            .and_then(|n| n.checked_add(4))
            .ok_or(BackendProofError::CombinedProofTruncatedPublicInputs)?;
        let pub_bytes = combined_proof
            .get(4..pub_end)
            .ok_or(BackendProofError::CombinedProofTruncatedPublicInputs)?;
        let public_inputs: Vec<Vec<u8>> = pub_bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(|c| c.to_vec())
            .collect();

        // Remainder is the proof, as 32-byte field words.
        let proof_bytes = &combined_proof[pub_end..];
        if !proof_bytes.len().is_multiple_of(32) {
            return Err(BackendProofError::CombinedProofMisalignedProofSection.into());
        }
        let proof: Vec<Vec<u8>> = proof_bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(|c| c.to_vec())
            .collect();

        let settings = settings_ultra_honk_keccak(self.disable_zk);

        Ok(BarretenbergHandler::initialized()?.verify(
            vk_bytes.to_vec(),
            public_inputs,
            proof,
            None,
            settings,
        )?)
    }
}

#[cfg(test)]
mod raw_verifier_tests {
    use super::*;

    // These tests check the parsing guards in `verify_combined`.
    // Each guard returns before the native `circuit_verify` call.
    // These tests require neither CRS data nor a real proof.
    // Integration tests initialize the SRS and check correctly formatted data.

    #[test]
    fn rejects_header_shorter_than_4_bytes() {
        assert!(Barretenberg::default()
            .verify_combined(b"vk", &[0u8; 3])
            .is_err());
    }

    #[test]
    fn rejects_public_inputs_truncated() {
        // Header claims 2 public inputs (needs 64 bytes) but only 10 follow.
        let mut blob = 2u32.to_be_bytes().to_vec();
        blob.extend_from_slice(&[0u8; 10]);
        assert!(Barretenberg::default()
            .verify_combined(b"vk", &blob)
            .is_err());
    }

    #[test]
    fn rejects_proof_not_multiple_of_32() {
        // Zero public inputs, then a 33-byte proof section.
        let mut blob = 0u32.to_be_bytes().to_vec();
        blob.extend_from_slice(&[0u8; 33]);
        assert!(Barretenberg::default()
            .verify_combined(b"vk", &blob)
            .is_err());
    }
}
