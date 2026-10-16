//! SRS (CRS) setup for barretenberg, ported from the reference
//! `outbe-zk-circuit-noir`/noir_rs: size the SRS to the circuit (gate count via
//! `circuit_stats` → next power of two) and load just that many G1 points —
//! from a local cache if present (`$BB_CRS_PATH` or `~/.bb-crs/bn254_g1.dat`,
//! the file `bb` itself downloads), otherwise range-downloaded from the Aztec
//! CRS. G2 is a small fixed group, hardcoded. The G1 prefix — cached or
//! downloaded — is verified against a pinned SHA-256 ([`PINNED_G1_SHA256`])
//! before bb trusts it, so a poisoned cache or tampered download can't seed a
//! forged-proof-accepting setup.

use std::sync::OnceLock;

use barretenberg_rs::api::BarretenbergApi;
use barretenberg_rs::backends::FfiBackend;
use barretenberg_rs::generated_types::{CircuitInput, ProofSystemSettings};
use sha2::{Digest, Sha256};

use outbe_zk::error::Error;

use crate::errors::BackendProofError;

/// bb's global CRS is **one-shot**: `srs_init_srs` takes effect once per process
/// and a later call with more points is ignored. So we init exactly once (to
/// the first circuit's need, or to `preinit_srs`'s size) and remember it.
static SRS_POINTS: OnceLock<u32> = OnceLock::new();

/// Explicit path to a BN254 G1 SRS `.dat`, set by the embedding application
/// (e.g. a mobile wallet pointing at a bundled SRS asset). Consulted *before*
/// `$BB_CRS_PATH` / `~/.bb-crs` by [`local_g1_path`]. Set once; later sets are
/// ignored. When built without `with-network-srs` this is the only way to
/// provide the SRS — there is no network fallback.
static SRS_PATH_OVERRIDE: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Point the SRS loader at an explicit G1 `.dat` file. The embedding app calls
/// this **once, before any proof** (e.g. with a bundled SRS asset path) so
/// the SRS loader reads locally instead of downloading. Required when built
/// without `with-network-srs`; optional otherwise (it just overrides the cache
/// location).
pub fn set_srs_path(path: std::path::PathBuf) {
    let _ = SRS_PATH_OVERRIDE.set(path);
}

/// The BN254 G2 point of the SRS (fixed; same every time, so not read from the
/// G1 `.dat`). Matches the reference / Aztec CRS `g2.dat`.
const G2: [u8; 128] = [
    1, 24, 196, 213, 184, 55, 188, 194, 188, 137, 181, 179, 152, 181, 151, 78, 159, 89, 68, 7, 59,
    50, 7, 139, 126, 35, 31, 236, 147, 136, 131, 176, 38, 14, 1, 178, 81, 246, 241, 199, 231, 255,
    78, 88, 7, 145, 222, 232, 234, 81, 216, 122, 53, 142, 3, 139, 78, 254, 48, 250, 192, 147, 131,
    193, 34, 254, 189, 163, 192, 192, 99, 42, 86, 71, 91, 66, 20, 229, 97, 94, 17, 230, 221, 63,
    150, 230, 206, 162, 133, 74, 135, 212, 218, 204, 94, 85, 4, 252, 99, 105, 247, 17, 15, 227,
    210, 81, 86, 193, 187, 154, 114, 133, 156, 242, 160, 70, 65, 249, 155, 164, 238, 65, 60, 128,
    218, 106, 95, 228,
];

/// Each G1 point is 64 bytes in the `.dat` encoding.
const G1_POINT_SIZE: u32 = 64;

/// Pinned SHA-256 of the BN254 G1 SRS prefix, keyed by point count.
///
/// The SRS is the proving system's trusted setup: a tampered G1 (a poisoned
/// local cache, or a download from a compromised source) lets forged proofs
/// verify. So the exact bytes handed to `srs_init_srs` are checked against a
/// pinned digest first. A prefix hash does **not** compose — the hash of `N`
/// points says nothing about `N′ ≠ N` points — so the pin is per-point-count:
/// a pinned size that mismatches is rejected (tampered CRS); an unpinned size
/// is allowed through but its hash is reported so an operator can pin it (pin
/// every size you deploy, or flip the `None` arm in [`verify_g1`] to fail
/// closed). The canonical set's [`super::preinit_srs`] size is `(1<<16)+1`.
/// The `(1<<20)+1` prefix stays pinned for future circuits without increasing
/// the default initialization size. Every digest is a prefix of the canonical
/// Aztec `g1.dat`.
const PINNED_G1_SHA256: &[(u32, [u8; 32])] = &[
    // (1<<14)+1 — Emit mint + Paynote circuits (same 2^14 domain).
    (
        (1 << 14) + 1,
        [
            0x8d, 0x53, 0x95, 0x34, 0x50, 0xcd, 0x6e, 0x90, 0xa9, 0x45, 0x83, 0x7d, 0x49, 0x82,
            0x98, 0x15, 0x13, 0xe2, 0xf4, 0x4d, 0xb7, 0xb2, 0x0f, 0xd4, 0x77, 0xf1, 0x04, 0xb0,
            0x41, 0xbe, 0x30, 0x4d,
        ],
    ),
    // (1<<16)+1 — the canonical set's largest domain (ownership, demo tribute,
    // Niflheim tribute) and the `preinit_srs` size.
    (
        (1 << 16) + 1,
        [
            0x9f, 0xd9, 0xfa, 0xd5, 0x4c, 0xfd, 0xc6, 0x1e, 0xc3, 0x09, 0xe8, 0x18, 0xfe, 0x7b,
            0xbd, 0xda, 0xd1, 0xca, 0x7d, 0xde, 0x72, 0xcc, 0x9b, 0x1a, 0xfd, 0x12, 0x6b, 0x74,
            0x4f, 0x88, 0x70, 0x78,
        ],
    ),
    // (1<<20)+1 — reserved for future circuits; not initialized by default.
    (
        (1 << 20) + 1,
        [
            0x0f, 0x23, 0x88, 0x56, 0xe5, 0x57, 0x22, 0xf1, 0x5a, 0x4d, 0x64, 0xef, 0x0d, 0xe1,
            0x2b, 0x42, 0x60, 0xe2, 0x18, 0x24, 0x55, 0x90, 0xbd, 0x3a, 0x6c, 0x90, 0x0a, 0xee,
            0x18, 0x8d, 0xe8, 0xe5,
        ],
    ),
];

/// Lower-case hex of a 32-byte digest (for pin messages).
fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Verify the exact G1 prefix bound for bb against the pin for its point count
/// (see [`PINNED_G1_SHA256`]). Called on **both** the cached and the
/// freshly-downloaded bytes, before they reach `srs_init_srs`.
#[allow(clippy::print_stderr)] // emits an SRS-not-pinned security warning
fn verify_g1(num_points: u32, bytes: &[u8]) -> Result<(), Error> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    match PINNED_G1_SHA256.iter().find(|(n, _)| *n == num_points) {
        Some((_, expected)) if &digest == expected => Ok(()),
        Some((_, expected)) => Err(BackendProofError::SrsChecksumMismatch {
            num_points,
            got: hex32(&digest),
            pinned: hex32(expected),
        }
        .into()),
        None => {
            eprintln!(
                "outbe-zk-backend: SRS size {num_points} points is not pinned; \
                 sha256={} — add it to PINNED_G1_SHA256 to verify it",
                hex32(&digest)
            );
            Ok(())
        }
    }
}

/// The Aztec CRS G1 endpoint (range-served). Only with `with-network-srs`.
#[cfg(feature = "with-network-srs")]
const AZTEC_CRS_G1: &str = "https://crs.aztec.network/g1.dat";

/// Next power of two `>= circuit_size` (the SRS subgroup size).
fn compute_subgroup_size(circuit_size: u32) -> u32 {
    let log_value = (circuit_size as f64).log2().ceil() as u32;
    2u32.pow(log_value)
}

/// Path to a locally available BN254 G1 SRS: the [`set_srs_path`] override if
/// set, else `$BB_CRS_PATH`, else `~/.bb-crs/bn254_g1.dat` (the file `bb`
/// downloads/caches).
fn local_g1_path() -> std::path::PathBuf {
    if let Some(p) = SRS_PATH_OVERRIDE.get() {
        return p.clone();
    }
    if let Ok(p) = std::env::var("BB_CRS_PATH") {
        return p.into();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::Path::new(&home).join(".bb-crs/bn254_g1.dat")
}

/// The SRS subgroup size (G1 points) the prover needs, via bb `circuit_stats`.
/// Uses `num_gates_dyadic` — the power-of-two proving domain — because plain
/// `num_gates` excludes the ZK/honk padding rows and under-sizes the SRS
/// (`prover trying to get too many points`). Guards with the next-pow2 of
/// `num_gates` too, in case `num_gates_dyadic` is ever the smaller of the two.
fn subgroup_size(
    api: &mut BarretenbergApi<FfiBackend>,
    acir_uncompressed: &[u8],
    settings: &ProofSystemSettings,
) -> Result<u32, Error> {
    let circuit = CircuitInput {
        name: String::new(),
        bytecode: acir_uncompressed.to_vec(),
        verification_key: Vec::new(),
    };
    let info = api
        .circuit_stats(circuit, false, settings.clone())
        .map_err(|e| BackendProofError::BbCircuitStats(e.to_string()))?;
    Ok(info
        .num_gates_dyadic
        .max(compute_subgroup_size(info.num_gates)))
}

/// Load `num_points` G1 points (`num_points * 64` bytes) from the local cache.
fn local_g1(num_points: u32) -> Result<Vec<u8>, Error> {
    let path = local_g1_path();
    let bytes = std::fs::read(&path).map_err(|source| BackendProofError::ReadSrs {
        path: path.clone(),
        source,
    })?;
    let need = (num_points * G1_POINT_SIZE) as usize;
    if bytes.len() < need {
        return Err(BackendProofError::CachedSrsTooShort {
            path,
            actual: bytes.len(),
            need,
            num_points,
        }
        .into());
    }
    Ok(bytes[..need].to_vec())
}

/// Range-download `num_points` G1 points from the Aztec CRS (fallback when no
/// local cache is available). Does **not** touch the cache — the caller writes
/// it through [`cache_g1`] only *after* the bytes pass [`verify_g1`], so an
/// unverified/tampered download can never poison the on-disk cache.
///
/// Only with `with-network-srs` (off for mobile): an on-device prover must never
/// fetch the trusted setup at proving time — `reqwest::blocking` also panics on
/// the mobile event loop. The SRS is provided via [`set_srs_path`] instead.
#[cfg(feature = "with-network-srs")]
fn net_g1(num_points: u32) -> Result<Vec<u8>, Error> {
    let g1_end = num_points * G1_POINT_SIZE - 1;
    let resp = reqwest::blocking::Client::new()
        .get(AZTEC_CRS_G1)
        .header(reqwest::header::RANGE, format!("bytes=0-{g1_end}"))
        .send()
        .map_err(|e| BackendProofError::SrsDownload(e.to_string()))?;
    let bytes = resp
        .bytes()
        .map_err(|e| BackendProofError::SrsDownloadBody(e.to_string()))?
        .to_vec();
    Ok(bytes)
}

/// Best-effort write-through of verified G1 bytes to the local cache, so the
/// next call — and any smaller circuit — reads locally instead of re-downloading.
/// Only with `with-network-srs` (nothing is ever downloaded to cache otherwise).
#[cfg(feature = "with-network-srs")]
fn cache_g1(bytes: &[u8]) {
    let path = local_g1_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, bytes);
}

/// Initialize bb's global CRS with `num_points` G1 points (local cache, else
/// network) + the fixed G2 — **once** per process (one-shot). A later request
/// for ≤ the init'd size is a no-op; a request for *more* errors, since bb
/// can't grow the CRS — pre-size with [`super::preinit_srs`].
pub fn ensure_srs(api: &mut BarretenbergApi<FfiBackend>, num_points: u32) -> Result<(), Error> {
    if let Some(&n) = SRS_POINTS.get() {
        if num_points <= n {
            return Ok(());
        }
        return Err(BackendProofError::SrsAlreadyInitialized {
            initialized: n,
            requested: num_points,
        }
        .into());
    }
    // Source the G1 prefix: local first (the set_srs_path override / cache),
    // else the network — except without `with-network-srs`, where a missing local
    // SRS is a hard error (no download). Integrity-check the bytes (whether
    // local or downloaded) before bb trusts them, and before persisting a fresh
    // download so a tampered fetch can't poison the cache for later runs.
    let g1 = match local_g1(num_points) {
        Ok(g1) => {
            verify_g1(num_points, &g1)?;
            g1
        }
        Err(_local_err) => {
            #[cfg(not(feature = "with-network-srs"))]
            {
                return Err(BackendProofError::SrsUnavailableWithoutNetwork {
                    path: local_g1_path(),
                    reason: _local_err.to_string(),
                }
                .into());
            }
            #[cfg(feature = "with-network-srs")]
            {
                let g1 = net_g1(num_points)?;
                verify_g1(num_points, &g1)?;
                cache_g1(&g1);
                g1
            }
        }
    };
    api.srs_init_srs(&g1, num_points, &G2)
        .map_err(|e| BackendProofError::SrsInit(e.to_string()))?;
    let _ = SRS_POINTS.set(num_points);
    Ok(())
}

/// Ensure the CRS is sized for `acir`: `circuit_stats` dyadic domain `+ 1`
/// points (≥ the prover's need, which is `circuit_size` + ZK padding).
pub fn ensure_srs_for(
    api: &mut BarretenbergApi<FfiBackend>,
    acir_uncompressed: &[u8],
    settings: &ProofSystemSettings,
) -> Result<(), Error> {
    let num_points = subgroup_size(api, acir_uncompressed, settings)? + 1;
    // bb's SRS is process-global and cannot grow. A small canonical circuit
    // may run before a larger one (PayNote spend, then merge), so reserve the
    // already-pinned canonical capacity at the first operation.
    ensure_srs(api, num_points.max(super::CANONICAL_SRS_POINTS))
}

#[cfg(test)]
mod tests {
    use super::{verify_g1, G1_POINT_SIZE};

    #[test]
    fn emit_paynote_crs_rejects_untrusted_bytes() {
        let num_points = (1 << 14) + 1;
        let untrusted = vec![0; num_points as usize * G1_POINT_SIZE as usize];
        assert!(verify_g1(num_points, &untrusted).is_err());
    }
}
