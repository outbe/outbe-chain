//! Barretenberg backend: UltraHonkKeccak proving and
//! verification over the canonical circuits, via `barretenberg-rs`'s FFI
//! (`libbb-external`) — no `bb` subprocess, so it runs on-device.
//!
//! Mirrors the reference `outbe-zk-circuit-noir` backend: keccak oracle hash for
//! on-chain (EVM) verification and an optional
//! **low-memory mode** that file-backs barretenberg's
//! polynomials (~2× slower, much less RAM — for proving on constrained
//! devices). Bytecode and witness are handed to bb **uncompressed** (the
//! committed bytecode and `WitnessStack::serialize()` are gzipped; bb wants raw
//! msgpack).
//!
//! Thin shell: [`witness::solved_witness`]
//! produces the ACVM-solved witness, this hands it to `circuit_prove` with the
//! circuit's committed bytecode + VK ([`CircuitId`]);
//! verification re-derives the public inputs from the claim and calls
//! `circuit_verify`. Pinned to `barretenberg-rs` =5.0.0-nightly.20260522,
//! matching the `bb` that writes the committed VKs (`-t evm-no-zk`).

use std::io::Read;
use std::sync::{Mutex, MutexGuard, OnceLock};

use barretenberg_rs::api::BarretenbergApi;
use barretenberg_rs::backends::FfiBackend;
use barretenberg_rs::generated_types::{CircuitInput, ProofSystemSettings};
use base64::Engine;
use flate2::read::GzDecoder;

use outbe_zk::error::Error;
use outbe_zk::protocol::zk::{Circuit, CircuitId, ProofGenerator, ProofVerifier};

use crate::witness;

mod srs;

/// Point the SRS loader at an explicit G1 `.dat` (e.g. a mobile wallet's bundled
/// SRS asset). Call once before proving; required when built without
/// `with-network-srs` (there is no network fallback then). See
/// [`srs::set_srs_path`].
pub use srs::set_srs_path;

/// Largest CRS required by the canonical circuit set: ownership, the Demo
/// Tribute proof, and the Niflheim tribute all prove in a 2^16 domain, so the
/// SRS needs `2^16 + 1` G1 points (domain + one). Emit mint and Paynote spend are
/// smaller (2^14 domain); PayNote merge uses a 2^15 domain.
pub const CANONICAL_SRS_POINTS: u32 = (1 << 16) + 1;

/// Barretenberg's CRS factory, low-memory globals, and prover are **process-
/// global** C++ state, and the API is not reentrant — so every bb operation
/// (SRS init through prove/verify) must be serialized. This lock makes the
/// backend safe to call from multiple threads: concurrent callers block here
/// instead of racing the global CRS. Poison is recovered (a panicked prior call
/// leaves the global state to be reset by the next `setup_srs` anyway).
static BB_LOCK: Mutex<()> = Mutex::new(());

fn bb_lock() -> MutexGuard<'static, ()> {
    BB_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Pre-initialize bb's (one-shot, process-global) CRS to `num_points` G1 points.
/// Call once at startup with the largest circuit's size when a process proves
/// circuits of *different* sizes — otherwise the first, smaller circuit fixes
/// the CRS and larger ones fail. For our canonical set that is
/// [`CANONICAL_SRS_POINTS`] (`(1 << 16) + 1`). Single-circuit callers can skip
/// this; the first prove reserves at least the canonical set's capacity.
pub fn preinit_srs(num_points: u32) -> Result<(), Error> {
    let _bb = bb_lock();
    let mut api = Barretenberg::api()?;
    srs::ensure_srs(&mut api, num_points)
}

/// Initialize the process-global CRS once for every canonical circuit.
///
/// `OUTBE_BB_SRS_PATH`, when set, selects a pre-staged G1 SRS file. Panics from
/// the FFI boundary become proof errors so startup callers can fail cleanly.
pub fn init_crs() -> Result<(), Error> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let srs_path = std::env::var("OUTBE_BB_SRS_PATH").ok();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(path) = srs_path {
                set_srs_path(path.into());
            }
            preinit_srs(CANONICAL_SRS_POINTS)
        }));
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("Barretenberg SRS initialization panicked".to_string()),
        }
    })
    .clone()
    .map_err(Error::Proof)
}

/// Verify a decoded combined proof against its compile-time circuit identity.
///
/// The owning protocol decoder must validate the public words before this
/// backend call.
pub fn verify_circuit<C: CircuitId>(combined_proof: &[u8]) -> Result<bool, Error> {
    Barretenberg::default().verify_combined(C::VK_BYTES, combined_proof)
}

/// UltraHonkKeccak proof, as bb's field encoding (`Vec<Vec<u8>>`). The public
/// inputs are not carried here — verification re-derives them from the claim.
#[derive(Clone, Debug)]
pub struct Proof {
    /// The proof fields.
    pub proof: Vec<Vec<u8>>,
}

/// UltraHonk + keccak oracle settings (EVM/on-chain verification), matching how
/// the committed VKs are generated (`bb write_vk -t evm-no-zk`).
fn settings_ultra_honk_keccak(disable_zk: bool) -> ProofSystemSettings {
    ProofSystemSettings {
        ipa_accumulation: false,
        oracle_hash_type: "keccak".to_string(),
        disable_zk,
        optimized_solidity_verifier: false,
    }
}

/// Toggle barretenberg's low-memory mode (file-backed polynomial storage): much
/// less RAM at ~2× proving time, for constrained devices. Sets the env vars bb
/// reads (`BB_SLOW_LOW_MEMORY` / `BB_STORAGE_BUDGET`) before prove/VK ops.
fn configure_memory(enabled: bool, max_storage_usage: Option<u64>) {
    std::env::set_var("BB_SLOW_LOW_MEMORY", if enabled { "1" } else { "0" });
    if let Some(budget) = max_storage_usage {
        std::env::set_var("BB_STORAGE_BUDGET", budget.to_string());
    }
}

/// Base64-decode then gunzip a circuit's committed bytecode into the raw ACIR
/// buffer bb's FFI consumes.
fn acir_buffer_uncompressed<C: CircuitId>() -> Result<Vec<u8>, Error> {
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(C::BYTECODE_B64)
        .map_err(|e| Error::Proof(format!("acir bytecode base64: {e}")))?;
    let mut raw = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut raw)
        .map_err(|e| Error::Proof(format!("acir decompress: {e}")))?;
    Ok(raw)
}

/// The barretenberg prover/verifier. Carries the proof-mode knobs the reference
/// exposes: `disable_zk` (off by default — keep ZK for secret-bearing witnesses)
/// and `low_memory` (+ optional storage budget) for on-device proving.
#[derive(Clone, Copy, Debug)]
pub struct Barretenberg {
    /// Disable zero-knowledge blinding (faster, but not ZK — only for
    /// public-input-only statements). Default `false`.
    pub disable_zk: bool,
    /// File-back polynomial memory: less RAM, ~2× slower.
    pub low_memory: bool,
    /// Optional storage budget (bytes) for low-memory mode.
    pub max_storage_usage: Option<u64>,
}

#[allow(clippy::derivable_impls)] // manual impl documents the security-critical disable_zk = false default
impl Default for Barretenberg {
    fn default() -> Self {
        // Zero-knowledge ON: the ownership/demo tribute witnesses carry a secret
        // key, so the proof must not leak it. `disable_zk = true` is faster but
        // not ZK — opt into it only for public-input-only statements.
        Self {
            disable_zk: false,
            low_memory: false,
            max_storage_usage: None,
        }
    }
}

impl Barretenberg {
    fn api() -> Result<BarretenbergApi<FfiBackend>, Error> {
        let backend = FfiBackend::new().map_err(|e| Error::Proof(format!("bb init: {e}")))?;
        Ok(BarretenbergApi::new(backend))
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
        // Pure (ACVM solve + decode) — no bb global state, so outside the lock.
        let solved = witness::solved_witness::<C>(witness, public)?;
        let acir = acir_buffer_uncompressed::<C>()?;
        let settings = settings_ultra_honk_keccak(self.disable_zk);

        // Everything below touches bb's process-global CRS / memory / prover.
        let _bb = bb_lock();
        configure_memory(self.low_memory, self.max_storage_usage);
        let mut api = Self::api()?;
        srs::ensure_srs_for(&mut api, &acir, &settings)?;
        let circuit = CircuitInput {
            name: C::LABEL.to_string(),
            bytecode: acir,
            verification_key: C::VK_BYTES.to_vec(),
        };
        let response = api
            .circuit_prove(circuit, &solved, settings)
            .map_err(|e| Error::Proof(format!("bb prove: {e}")))?;
        Ok(Proof {
            proof: response.proof,
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

        // bb's global CRS / prover — serialize against concurrent prove/verify.
        let _bb = bb_lock();
        let mut api = Self::api()?;
        srs::ensure_srs_for(&mut api, &acir, &settings)?;
        let response = api
            .circuit_verify(C::VK_BYTES, public_inputs, proof.proof.clone(), settings)
            .map_err(|e| Error::Proof(format!("bb verify: {e}")))?;
        Ok(response.verified)
    }
}

/// Verify an UltraHonkKeccak proof against a raw verification key — without a
/// compile-time [`Circuit`] type.
///
/// Where [`ProofVerifier`] needs the concrete circuit type `C` (to supply
/// `C::VK_BYTES` and reshape a typed `C::PublicInputs` into field words), this
/// takes the verification key and the proof as opaque bytes. That makes it the
/// right seam for a circuit-agnostic consumer such as the chain's `zk_verify`
/// precompile, which only has a `vk_bytes` looked up at runtime from the
/// canonical registry plus the proof the caller submitted — and must stay
/// decoupled from every circuit's public-input layout.
pub trait RawVerifier {
    /// Verify the self-describing EVM "combined" proof against `vk_bytes`.
    ///
    /// `combined_proof` is the byte layout the on-chain verifier consumes:
    ///
    /// ```text
    /// [4B BE num_public_inputs][32B × num_public_inputs][proof fields …]
    /// ```
    ///
    /// i.e. a big-endian `u32` count, then that many 32-byte public-input
    /// words, then the proof as concatenated 32-byte field elements. The split
    /// is recovered from the header alone — no VK introspection needed.
    ///
    /// The global bb CRS must already be initialized (see [`preinit_srs`]);
    /// this neither sizes nor fetches it. Returns `Ok(false)` for a
    /// well-formed-but-invalid proof and `Err` only for a malformed blob or an
    /// FFI failure.
    fn verify_combined(&self, vk_bytes: &[u8], combined_proof: &[u8]) -> Result<bool, Error>;
}

impl RawVerifier for Barretenberg {
    fn verify_combined(&self, vk_bytes: &[u8], combined_proof: &[u8]) -> Result<bool, Error> {
        // Header: big-endian u32 public-input count.
        let header = combined_proof
            .get(..4)
            .ok_or_else(|| Error::Proof("combined proof shorter than 4-byte header".into()))?;
        let num_public = u32::from_be_bytes(header.try_into().expect("4 bytes")) as usize;

        // [4 .. 4 + 32·N] public-input words.
        let pub_end = 4 + num_public * 32;
        let pub_bytes = combined_proof
            .get(4..pub_end)
            .ok_or_else(|| Error::Proof("combined proof truncated in public inputs".into()))?;
        let public_inputs: Vec<Vec<u8>> = pub_bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(|c| c.to_vec())
            .collect();

        // Remainder is the proof, as 32-byte field words.
        let proof_bytes = &combined_proof[pub_end..];
        if !proof_bytes.len().is_multiple_of(32) {
            return Err(Error::Proof(
                "combined proof: proof section not a multiple of 32 bytes".into(),
            ));
        }
        let proof: Vec<Vec<u8>> = proof_bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(|c| c.to_vec())
            .collect();

        let settings = settings_ultra_honk_keccak(self.disable_zk);

        // bb's global CRS / prover — serialize against concurrent prove/verify.
        let _bb = bb_lock();
        let mut api = Self::api()?;
        let response = api
            .circuit_verify(vk_bytes, public_inputs, proof, settings)
            .map_err(|e| Error::Proof(format!("bb raw verify: {e}")))?;
        Ok(response.verified)
    }
}

#[cfg(test)]
mod raw_verifier_tests {
    use super::*;

    // These exercise `verify_combined`'s blob-parsing guards, which all return
    // before the FFI `circuit_verify` call — so they need neither a CRS nor a
    // real proof. A well-formed blob would reach bb and is covered by the
    // integration tests that stand up the SRS.

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
