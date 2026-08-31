//! UltraHonkKeccak verifier core.
//!
//! Looks `circuit_hash` up against the canonical-circuit registry from
//! `outbe-zk-canonical` and dispatches the proof bytes to the Barretenberg
//! FFI in `outbe-zk-backend`. Unknown circuits return `false` rather than
//! erroring.

use std::sync::OnceLock;

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_zk_canonical::noir::full_proof::FullProof;
use outbe_zk_canonical::noir::CIRCUIT_REGISTRY;
use outbe_zk_canonical::{CircuitId, RegistryEntry};
use tracing::{info, trace};

use crate::errors::ZkProofError;

/// Maximum Barretenberg SRS size needed by the canonical circuits in
/// `outbe-zk-canonical` (`flat_aggregation_n64` is the largest at 2^2^0
/// gates). `verify_combined` does not size the CRS itself, so the startup
/// `preinit_srs` must cover the largest circuit the registry can verify.
/// This is the upstream-pinned preinit size (see `outbe-zk-backend`'s
/// `PINNED_G1_SHA256`).
const SRS_POINTS: u32 = (1 << 20) + 1;
/// The only payload offset a well-formed `abi.encode(bytes32, bytes)` carries:
/// one static word for `circuit_hash` plus the offset word itself.
const CANONICAL_ABI_OFFSET: u64 = 64;
const FULL_PROOF_PUBLIC_INPUT_COUNT: usize = 4;
const FULL_PROOF_PROOF_FIELD_COUNT: usize = 274;
pub const FULL_PROOF_COMBINED_LEN: usize =
    4 + (FULL_PROOF_PUBLIC_INPUT_COUNT + FULL_PROOF_PROOF_FIELD_COUNT) * 32;

/// Public claim carried by the canonical `outbe.full_proof@1.0.0`
/// combined-proof format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullProofPublicInputs {
    pub derived_owner: [u8; 32],
    pub nft_hash: [u8; 32],
    pub binding_hash: [u8; 32],
    pub merkle_root: [u8; 32],
}

/// One-shot initialization of the Barretenberg global CRS.
///
/// **Must be called from a synchronous context before the tokio runtime
/// starts** - `outbe-zk-backend`'s SRS loader uses `reqwest::blocking`
/// internally (under the default `with-network-srs` feature) and panics if
/// invoked from inside an async task. Calling this once at node startup is
/// what allows the `0xEE08` zkVerify precompile to actually verify proofs at
/// runtime; `verify_combined` neither sizes nor fetches the CRS, so without
/// this the precompile returns `0x..00` for every input.
///
/// The optional environment variable `OUTBE_BB_SRS_PATH` selects a
/// pre-staged `g1.dat` SRS file (via `set_srs_path`); if unset the backend
/// downloads it once from `crs.aztec.network`.
///
/// Idempotent - repeated calls return the first initialization result.
///
/// The verifier participates in consensus-critical Tribute admission, so a
/// node must not execute blocks without the hash-pinned CRS. Startup callers
/// must propagate this error and stop.
pub fn init_crs() -> Result<(), ZkProofError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let srs_path = std::env::var("OUTBE_BB_SRS_PATH").ok();
        // `preinit_srs` validates the pinned SRS digest. Catch an FFI panic so
        // it becomes a typed startup error rather than unwinding through main.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(path) = srs_path.as_deref() {
                outbe_zk_backend::barretenberg::set_srs_path(path.into());
            }
            outbe_zk_backend::barretenberg::preinit_srs(SRS_POINTS)
        }));
        match outcome {
            Ok(Ok(())) => {
                info!(num_points = SRS_POINTS, path = ?srs_path, "Barretenberg SRS initialized");
                Ok(())
            }
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("Barretenberg SRS initialization panicked".to_string()),
        }
    })
    .clone()
    .map_err(ZkProofError::CrsInitialization)
}

/// Verify an UltraHonkKeccak proof against a registered canonical
/// circuit. Returns 32 bytes: `0x..01` on a valid proof, `0x..00`
/// otherwise (invalid proof OR unknown `circuit_hash`).
pub fn zk_verify(input: &[u8]) -> Result<[u8; 32], ZkProofError> {
    let (circuit_hash, combined_proof) = decode_input(input)?;

    let descriptor = match find_canonical(&circuit_hash) {
        Some(d) => d,
        None => {
            trace!(circuit_hash = ?circuit_hash, "zk_verify: unknown circuit_hash");
            return Ok(bool_to_32b(false));
        }
    };

    let ok = verify_inner(descriptor.vk_bytes, combined_proof)?;
    trace!(circuit = descriptor.label, ok, "zk_verify");

    Ok(bool_to_32b(ok))
}

/// Decode and validate the four public inputs embedded in a canonical full
/// proof. The proof bytes remain self-contained and are passed unchanged to
/// Barretenberg after callers compare this claim with their expected values.
pub fn decode_full_proof_public_inputs(
    combined_proof: &[u8],
) -> Result<FullProofPublicInputs, ZkProofError> {
    let header = combined_proof
        .get(..4)
        .ok_or(ZkProofError::CombinedProofTooShort(combined_proof.len()))?;
    let count = u32::from_be_bytes(header.try_into().expect("four-byte slice")) as usize;
    if count != FULL_PROOF_PUBLIC_INPUT_COUNT {
        return Err(ZkProofError::WrongPublicInputCount {
            expected: FULL_PROOF_PUBLIC_INPUT_COUNT,
            actual: count,
        });
    }
    let public_end = 4 + count * 32;
    if combined_proof.len() < public_end {
        return Err(ZkProofError::TruncatedPublicInputs {
            expected: public_end,
            actual: combined_proof.len(),
        });
    }
    if combined_proof.len() != FULL_PROOF_COMBINED_LEN {
        return Err(ZkProofError::WrongCombinedProofLength {
            expected: FULL_PROOF_COMBINED_LEN,
            actual: combined_proof.len(),
        });
    }
    let mut words = [[0u8; 32]; FULL_PROOF_PUBLIC_INPUT_COUNT];
    for (index, word) in combined_proof[4..public_end].chunks_exact(32).enumerate() {
        words[index].copy_from_slice(word);
        if !is_canonical_field_word(&words[index]) {
            return Err(ZkProofError::NonCanonicalPublicInput(index));
        }
    }

    Ok(FullProofPublicInputs {
        derived_owner: words[0],
        nft_hash: words[1],
        binding_hash: words[2],
        merkle_root: words[3],
    })
}

/// Verify the pinned canonical `outbe.full_proof@1.0.0` circuit.
///
/// Malformed combined proofs are errors. Well-formed proofs that do not verify
/// return `Ok(false)`.
pub fn verify_full_proof(combined_proof: &[u8]) -> Result<bool, ZkProofError> {
    decode_full_proof_public_inputs(combined_proof)?;
    verify_inner(FullProof::VK_BYTES, combined_proof)
}

/// Stateless lookup against `outbe-zk-canonical`'s static circuit registry.
/// Activation/deprecation timing is enforced by consumer contracts, so
/// the on-chain verifier is unconditionally permissive over registered
/// circuits.
fn find_canonical(circuit_hash: &[u8; 32]) -> Option<&'static RegistryEntry> {
    CIRCUIT_REGISTRY
        .iter()
        .find(|d| &d.circuit_hash == circuit_hash)
}

/// Decode `abi.encode(bytes32, bytes)`.
fn decode_input(input: &[u8]) -> Result<([u8; 32], &[u8]), ZkProofError> {
    if input.len() < 64 {
        return Err(ZkProofError::InputTooShort(input.len()));
    }

    let mut circuit_hash = [0u8; 32];
    circuit_hash.copy_from_slice(&input[0..32]);

    let offset =
        read_u64_be_padded(&input[32..64]).ok_or(ZkProofError::MalformedAbi("offset too large"))?;
    // Reject a non-canonical offset before it reaches any arithmetic: an
    // offset near `u64::MAX` wraps `offset + 32` in a release build
    // (overflow-checks off by default) and slips past the bounds guard below
    // into an out-of-range slice index.
    if offset != CANONICAL_ABI_OFFSET {
        return Err(ZkProofError::MalformedAbi("non-canonical offset"));
    }
    let offset = offset as usize;
    let header_end = offset
        .checked_add(32)
        .ok_or(ZkProofError::MalformedAbi("offset overflow"))?;
    if input.len() < header_end {
        return Err(ZkProofError::MalformedAbi("offset past end"));
    }

    let length = read_u64_be_padded(&input[offset..header_end])
        .ok_or(ZkProofError::MalformedAbi("length too large"))?;
    let length = length as usize;

    let data_start = header_end;
    let data_end = data_start
        .checked_add(length)
        .ok_or(ZkProofError::MalformedAbi("length overflow"))?;
    if input.len() < data_end {
        return Err(ZkProofError::MalformedAbi("payload truncated"));
    }

    Ok((circuit_hash, &input[data_start..data_end]))
}

/// Read a u64 from the right-aligned 8 bytes of a 32-byte big-endian
/// uint256 slot. Returns None if the upper 24 bytes are non-zero.
fn read_u64_be_padded(slot: &[u8]) -> Option<u64> {
    if slot.len() != 32 {
        return None;
    }
    if slot[..24].iter().any(|&b| b != 0) {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&slot[24..32]);
    Some(u64::from_be_bytes(buf))
}

fn bool_to_32b(b: bool) -> [u8; 32] {
    let mut out = [0u8; 32];
    if b {
        out[31] = 1;
    }
    out
}

fn is_canonical_field_word(word: &[u8; 32]) -> bool {
    let field = Fr::from_be_bytes_mod_order(word);
    let bytes = field.into_bigint().to_bytes_be();
    let mut canonical = [0u8; 32];
    canonical[32 - bytes.len()..].copy_from_slice(&bytes);
    canonical == *word
}

/// Dispatch the actual UltraHonkKeccak verification.
///
/// Barretenberg's global CRS must be initialized before the first call;
/// `verify_combined` neither sizes nor fetches it. The outbe-chain runtime
/// populates the CRS at process start via [`init_crs`] (which calls
/// `outbe_zk_backend::barretenberg::preinit_srs`).
///
/// `Barretenberg::default()` keeps `disable_zk = false`, matching the prover
/// (commitment-bearing witnesses are proved with ZK on).
fn verify_inner(vk_bytes: &[u8], combined_proof: &[u8]) -> Result<bool, ZkProofError> {
    use outbe_zk_backend::barretenberg::{Barretenberg, RawVerifier};
    Barretenberg::default()
        .verify_combined(vk_bytes, combined_proof)
        .map_err(|error| ZkProofError::VerificationBackend(error.to_string()))
}
