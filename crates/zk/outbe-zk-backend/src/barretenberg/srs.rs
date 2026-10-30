//! Load and validate the fixed CRS when the process-wide handler starts.
//! Read G1 points from the local cache, or download them when the network feature is enabled.
//! Verify the G1 prefix against [`PINNED_G1_SHA256`] before native initialization.
//! G2 is fixed. The handler retains the installed capacity and rejects larger proving circuits.

use std::sync::OnceLock;

use barretenberg_rs::api::BarretenbergApi;
use barretenberg_rs::backends::FfiBackend;
use barretenberg_rs::generated_types::{CircuitInput, ProofSystemSettings};
use sha2::{Digest, Sha256};

use crate::barretenberg::CANONICAL_SRS_POINTS;
use crate::errors::{BackendProofError, CrsError};
use outbe_zk::error::Error;

/// Capacity of the native CRS installed by the process-wide handler at startup.
pub(super) struct Crs {
    points: u32,
}

impl Crs {
    pub fn new(points: u32) -> Self {
        Self { points }
    }
    pub(super) fn ensure_capacity(&self, requested: u32) -> Result<(), Error> {
        if requested > self.points {
            return Err(CrsError::SrsAlreadyInitialized {
                initialized: self.points,
                requested,
            }
            .into());
        }
        Ok(())
    }

    pub(super) fn validate_circuit(
        &self,
        api: &mut BarretenbergApi<FfiBackend>,
        acir: &[u8],
        settings: &ProofSystemSettings,
    ) -> Result<(), Error> {
        self.ensure_capacity(subgroup_size(api, acir, settings)?.saturating_add(1))
    }
}

static BB_CRS_PATH_ENV_VAR: &str = "BB_CRS_PATH";

/// Explicit path to a BN254 G1 SRS `.dat`.
static CRS_PATH_OVERRIDE: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Point the SRS loader at an explicit G1 `.dat` file. The embedding app calls
/// this **once, before handler initialization** (e.g. with a bundled SRS asset path) so
/// the SRS loader reads locally instead of downloading. Required when built
/// without `with-network-srs`; optional otherwise (it just overrides the cache
/// location).
pub fn set_srs_path(path: std::path::PathBuf) {
    let _ = CRS_PATH_OVERRIDE.set(path);
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
/// closed). The handler's canonical capacity is `(1<<16)+1`.
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
    // Niflheim tribute) and the handler's fixed capacity.
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
fn verify_g1(num_points: u32, bytes: &[u8]) -> Result<(), CrsError> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    match PINNED_G1_SHA256.iter().find(|(n, _)| *n == num_points) {
        Some((_, expected)) if &digest == expected => Ok(()),
        Some((_, expected)) => Err(CrsError::SrsChecksumMismatch {
            num_points,
            got: hex32(&digest),
            pinned: hex32(expected),
        }),
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
    circuit_size.checked_next_power_of_two().unwrap_or(u32::MAX)
}

/// Resolves path to CRS file.
fn local_g1_path() -> std::path::PathBuf {
    if let Some(p) = CRS_PATH_OVERRIDE.get() {
        return p.clone();
    }
    if let Ok(p) = std::env::var(BB_CRS_PATH_ENV_VAR) {
        return p.into();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let home_path = std::path::Path::new(&home).join(".bb-crs/bn254_g1.dat");
    if home_path.exists() {
        return home_path;
    }
    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        if !cargo_home.is_empty() {
            return std::path::Path::new(&cargo_home).join(".bb-crs/bn254_g1.dat");
        }
    }
    home_path
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
fn local_g1(num_points: u32) -> Result<Vec<u8>, CrsError> {
    let path = local_g1_path();
    let bytes = std::fs::read(&path).map_err(|source| CrsError::ReadSrs {
        path: path.clone(),
        source,
    })?;
    let need = (num_points * G1_POINT_SIZE) as usize;
    if bytes.len() < need {
        return Err(CrsError::CachedSrsTooShort {
            path,
            actual: bytes.len(),
            need,
            num_points,
        });
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
fn fetch_g1_once(num_points: u32) -> Result<Vec<u8>, CrsError> {
    let g1_end = num_points * G1_POINT_SIZE - 1;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| CrsError::SrsDownload(e.to_string()))?;
    let resp = client
        .get(AZTEC_CRS_G1)
        .header(reqwest::header::RANGE, format!("bytes=0-{g1_end}"))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| CrsError::SrsDownload(e.to_string()))?;
    let bytes = resp
        .bytes()
        .map_err(|e| CrsError::SrsDownloadBody(e.to_string()))?
        .to_vec();
    verify_g1(num_points, &bytes)?;
    Ok(bytes)
}

#[cfg(feature = "with-network-srs")]
fn net_g1(num_points: u32) -> Result<Vec<u8>, CrsError> {
    const MAX_ATTEMPTS: u64 = 3;
    let mut last_err = None;
    for attempt in 1..=MAX_ATTEMPTS {
        if attempt > 1 {
            if let Ok(cached) = local_g1(num_points) {
                if verify_g1(num_points, &cached).is_ok() {
                    return Ok(cached);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(attempt));
        }
        match fetch_g1_once(num_points) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.expect("MAX_ATTEMPTS is non-zero"))
}

/// Atomic write-through of verified G1 bytes to the local cache, so concurrent
/// test processes never observe a truncated file and subsequent calls read
/// locally instead of re-downloading.
/// Only with `with-network-srs` (nothing is ever downloaded to cache otherwise).
#[cfg(feature = "with-network-srs")]
fn cache_g1(bytes: &[u8]) {
    let path = local_g1_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp_path = path.with_extension(format!("dat.{}.tmp", std::process::id()));
    if std::fs::write(&tmp_path, bytes).is_ok() {
        let _ = std::fs::rename(&tmp_path, &path);
        let _ = std::fs::remove_file(&tmp_path);
    }
}

pub(crate) fn load_srs() -> Result<(Vec<u8>, u32, Vec<u8>), CrsError> {
    let num_points = CANONICAL_SRS_POINTS;
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
                return Err(CrsError::SrsUnavailableWithoutNetwork {
                    path: local_g1_path(),
                    reason: _local_err.to_string(),
                });
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

    Ok((g1, num_points, G2.to_vec()))
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
