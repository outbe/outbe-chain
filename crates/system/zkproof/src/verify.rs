//! UltraHonkKeccak verifier core.
//!
//! Looks `circuit_hash` up against the canonical-circuit table from
//! `outbe-zk-canonical` and dispatches the proof bytes to the Barretenberg
//! FFI vendored by `outbe-zk-circuit-noir`. Unknown circuits return
//! `false` rather than erroring.

use std::sync::Once;

use outbe_zk_canonical::{CircuitDescriptor, ALL_CIRCUITS};
use tracing::{info, trace, warn};

use crate::errors::ZkProofError;

/// Maximum Barretenberg SRS size needed by the canonical circuits in
/// `outbe-zk-canonical` (`flat_aggregation_n64` is the largest at
/// ~2¹⁷ gates).
const SRS_POINTS: u32 = 1 << 17;

/// One-shot initialization of the Barretenberg global CRS.
///
/// **Must be called from a synchronous context before the tokio runtime
/// starts** — `outbe_zk_circuit_noir::setup_srs` uses `reqwest::blocking`
/// internally and panics if invoked from inside an async task. Calling
/// this once at node startup is what allows the `0xEE08` zkVerify
/// precompile to actually verify proofs at runtime; without it
/// `verify_ultra_honk_keccak` errors and the precompile returns
/// `0x..00` for every input.
///
/// The optional environment variable `OUTBE_BB_SRS_PATH` selects a
/// pre-staged `g1.dat` SRS file; if unset the underlying
/// `outbe-zk-circuit-noir` runtime downloads it once from
/// `crs.aztec.network`.
///
/// Idempotent — repeated calls are no-ops.
pub fn init_crs() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let srs_path = std::env::var("OUTBE_BB_SRS_PATH").ok();
        // `setup_srs` downloads the Barretenberg CRS over HTTPS and `unwrap()`s
        // internally, so an offline/cert/network failure panics rather than
        // returning `Err`. `init_crs` is meant to be non-fatal (a missing CRS
        // only degrades `zk_verify` to `false`), so catch the panic too: a node
        // must still start (consensus, TEE, RPC) when the CRS endpoint is
        // unreachable. Set `OUTBE_BB_SRS_PATH` to a local SRS file to avoid the
        // download entirely.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            outbe_zk_circuit_noir::barretenberg::srs::setup_srs(SRS_POINTS, srs_path.as_deref())
        }));
        match outcome {
            Ok(Ok(n)) => info!(num_points = n, path = ?srs_path, "Barretenberg SRS initialized"),
            Ok(Err(e)) => {
                warn!(err = %e, "Barretenberg SRS init failed; zk_verify will return false")
            }
            Err(_) => warn!(
                "Barretenberg SRS init panicked (offline / CRS endpoint unreachable); \
                 zk_verify will return false"
            ),
        }
    });
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

    let ok = verify_inner(descriptor.vk_bytes, combined_proof);
    trace!(circuit = descriptor.label, ok, "zk_verify");

    Ok(bool_to_32b(ok))
}

/// Stateless lookup against `outbe-zk-canonical`'s static circuit table.
/// Activation/deprecation timing is enforced by consumer contracts, so
/// the on-chain verifier is unconditionally permissive over registered
/// circuits.
fn find_canonical(circuit_hash: &[u8; 32]) -> Option<&'static CircuitDescriptor> {
    ALL_CIRCUITS
        .iter()
        .copied()
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
    let offset = offset as usize;
    if input.len() < offset + 32 {
        return Err(ZkProofError::MalformedAbi("offset past end"));
    }

    let length = read_u64_be_padded(&input[offset..offset + 32])
        .ok_or(ZkProofError::MalformedAbi("length too large"))?;
    let length = length as usize;

    let data_start = offset + 32;
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

/// Dispatch the actual UltraHonkKeccak verification.
///
/// Barretenberg's global CRS must be initialized before the first call;
/// `verify_ultra_honk_keccak` consumes it. The outbe-chain runtime
/// populates the CRS at process start via
/// `outbe_zk_circuit_noir::barretenberg::srs::setup_srs`.
fn verify_inner(vk_bytes: &[u8], combined_proof: &[u8]) -> bool {
    outbe_zk_circuit_noir::barretenberg::verify::verify_ultra_honk_keccak(
        combined_proof.to_vec(),
        vk_bytes.to_vec(),
        false,
    )
    .unwrap_or(false)
}
