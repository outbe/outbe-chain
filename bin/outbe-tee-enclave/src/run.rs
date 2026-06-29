//! Shared entrypoint for the enclave binaries.
//!
//! Both the production `outbe-tee-enclave` and the dev `outbe-tee-enclave-mock`
//! binaries are thin shims over [`run`]; the only difference is the [`RunOpts`]
//! they pass. Keeping the orchestration here (arg parsing, key init, boot config,
//! listener selection, serve dispatch) guarantees the two binaries share one code
//! path — the node always talks to *an* enclave over the same Noise-IK channel.
//!
//! `run` returns a process exit code instead of calling `std::process::exit`
//! directly, so the shims own the single exit point and the body stays testable.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

use alloy_primitives::B256;
use rand_core::RngCore as _;

use crate::keys::EnclaveKeys;
use crate::seal::{
    seal_tribute_offer_and_group_sig, unseal_tribute_offer_and_group_sig, EnclaveBootConfig,
    KeyPolicy, SealHeader, SEAL_FORMAT,
};
use crate::transport::{serve, serve_tcp};

/// Per-binary behavior knobs. The production binary uses [`RunOpts::prod`]; the
/// dev mock binary uses [`RunOpts::mock`]. `mock` is wired into the attestation /
/// sealing surface later; today it only differentiates the startup banner.
#[derive(Clone, Copy, Debug)]
pub struct RunOpts {
    /// True for the dev mock binary (deterministic fake quote + stable sealing
    /// key + gramine-direct semantics). Never set in the production binary.
    pub mock: bool,
}

impl RunOpts {
    /// Production binary: real Gramine attestation/sealing surface, no mock code.
    pub fn prod() -> Self {
        Self { mock: false }
    }

    /// Dev mock binary (only compiled under `--features mock`).
    pub fn mock() -> Self {
        Self { mock: true }
    }
}

/// Run the enclave server. Returns a process exit code (0 = success).
pub fn run(opts: RunOpts) -> i32 {
    let args: Vec<String> = std::env::args().collect();

    // Diagnostic mode: read the real Gramine /dev/attestation surface and exit.
    // Run under `gramine-direct`/`gramine-sgx` to observe what hardware exposes.
    if args.iter().any(|a| a == "--probe-attestation") {
        crate::gramine::probe_to_stderr();
        return 0;
    }

    let Some(socket) = arg_value(&args, "--socket") else {
        eprintln!("usage: outbe-tee-enclave --socket <path>");
        return 2;
    };

    if opts.mock {
        eprintln!(
            "outbe-tee-enclave-mock: MOCK ENCLAVE — deterministic quote + stable sealing key, \
             NOT confidential (dev/CI only, never production)"
        );
    }

    // Detect the real attestation environment up front so the startup banner
    // tells the truth (hardware-attested vs unattested) instead of claiming SGX.
    let attest = crate::gramine::attestation_type();
    if attest.is_hardware() {
        eprintln!(
            "outbe-tee-enclave: MODE = gramine-sgx — hardware attestation ENABLED ({})",
            attest.label()
        );
    } else if attest.sgx_present() {
        // Real SGX, but remote attestation not configured: EGETKEY sealing works
        // (confidential at rest), yet no DCAP quote can be produced. Not "no SGX".
        eprintln!(
            "outbe-tee-enclave: MODE = gramine-sgx — remote attestation DISABLED ({}); \
             EGETKEY sealing available, NOT remote-attested (configure \
             sgx.remote_attestation = \"dcap\" for production)",
            attest.label()
        );
    } else {
        eprintln!(
            "outbe-tee-enclave: MODE = {} — attestation DISABLED, NOT confidential \
             (no SGX hardware; use gramine-sgx for production)",
            attest.label()
        );
    }

    let tribute_offer_secret = dev_bytes_from_env("TEE_DEV_OFFER_SECRET", [0x07; 32]);
    // Explicit, deterministic DKG identity seed (dev/CI): `--dkg-seed <hex32>` or
    // env `TEE_DEV_DKG_SEED`. The gramine-direct mock path uses this since it has
    // no EGETKEY and so cannot persist a self-generated identity across restart.
    let cli_dkg_seed = arg_value(&args, "--dkg-seed")
        .and_then(|hex| parse_hex32(&hex))
        .or_else(|| {
            std::env::var("TEE_DEV_DKG_SEED")
                .ok()
                .and_then(|h| parse_hex32(&h))
        });
    // Resolve the identity seed: explicit seed > self-generated-and-sealed (real
    // SGX) > offer-secret fallback. See `resolve_dkg_identity_seed`.
    let (dkg_seed, dkg_id) = resolve_dkg_identity_seed(&args, cli_dkg_seed);
    let keys = match EnclaveKeys::new(tribute_offer_secret, dkg_seed) {
        Ok(keys) => keys,
        Err(err) => {
            eprintln!("outbe-tee-enclave: key init failed: {err}");
            return 1;
        }
    };

    // Optional seal/unseal boot configuration. Present only when the
    // launcher passes `--tee-dir`; absent → sealing disabled and the offer key is
    // re-derived from the DKG each boot.
    let boot = match build_boot_config(&args, &keys) {
        Ok(boot) => boot,
        Err(code) => return code,
    };

    // Shared, write-once DKG-derived offer-key slot. If a sealed blob
    // exists (and a sealing key is available), restore it now — the restart
    // fast-path that skips the DKG ceremony; otherwise the ceremony populates it.
    let offer_key: crate::transport::SharedTributeOfferKey =
        std::sync::Arc::new(std::sync::OnceLock::new());
    if let Some(cfg) = boot.as_deref() {
        if let Some(derived) = crate::transport::unseal_tribute_offer_and_group_sig_on_boot(cfg) {
            let _ = offer_key.set(derived);
        }
    }

    let keys = std::sync::Arc::new(keys);

    // A `host:port` endpoint listens on TCP (required under Gramine, whose
    // pathname UDS are process-internal so a host process cannot reach them);
    // anything else is a UDS path. The Noise-IK handshake authenticates +
    // encrypts every byte either way, so the carrier choice does not weaken the
    // channel.
    let result = if socket.contains(':') {
        // The TCP carrier is loopback-only. It exists solely because Gramine
        // pathname UDS are process-internal (unreachable from the host); it must
        // never expose the enclave off-host. Reject any non-loopback bind. (Noise-IK
        // still authenticates + encrypts every byte regardless of carrier — this
        // guard is defense-in-depth so a misconfigured `--socket 0.0.0.0:port`
        // cannot accidentally publish the enclave.)
        if !is_loopback_endpoint(&socket) {
            eprintln!(
                "outbe-tee-enclave: refusing to bind non-loopback TCP endpoint {socket} \
                 (the TCP carrier is 127.0.0.1/::1 only)"
            );
            return 2;
        }
        let listener = match std::net::TcpListener::bind(&socket) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("outbe-tee-enclave: bind tcp {socket} failed: {err}");
                return 1;
            }
        };
        eprintln!(
            "outbe-tee-enclave: listening on tcp://{socket} (attestation: {}; DKG identity: {dkg_id})",
            attest.label()
        );
        serve_tcp(&listener, keys, boot, offer_key)
    } else {
        // Fresh socket; UDS mode 0600 (owner-only), per plan §"Transport".
        let _ = std::fs::remove_file(&socket);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("outbe-tee-enclave: bind {socket} failed: {err}");
                return 1;
            }
        };
        // Best-effort 0600 (non-fatal under Gramine — the bound UDS is an
        // emulated socket object, not a chmod-able host file).
        if let Err(err) = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        {
            eprintln!("outbe-tee-enclave: chmod 0600 {socket} failed (continuing): {err}");
        }
        eprintln!(
            "outbe-tee-enclave: listening on {socket} (attestation: {}; DKG identity: {dkg_id})",
            attest.label()
        );
        serve(&listener, keys, boot, offer_key)
    };
    if let Err(err) = result {
        eprintln!("outbe-tee-enclave: serve error: {err}");
        return 1;
    }
    0
}

/// Resolve this enclave's DKG identity seed and a label for the startup banner.
///
/// Priority:
///  1. **Explicit** `--dkg-seed` / `TEE_DEV_DKG_SEED` (deterministic). Used by the
///     gramine-direct mock e2e/CI, which has no EGETKEY and so cannot persist a
///     self-generated identity across restart.
///  2. **Self-generated + sealed** (the honest SGX path): when no explicit seed is
///     given but `--tee-dir` is set and EGETKEY sealing is available (real
///     `gramine-sgx`), generate a RANDOM identity seed from hardware RNG on first
///     boot and SEAL it to `<tee-dir>/sealed_identity.bin`, so the enclave is an
///     INDEPENDENT DKG participant whose identity SURVIVES restart — with no
///     host-supplied `--dkg-seed`. The 32-byte seed is sealed (reusing the
///     offer-key seal with an empty group-sig) and the existing HKDF in
///     `EnclaveKeys::new` reconstructs the full BLS+share-decrypt identity from it.
///  3. **None**: no seed and no sealing → `EnclaveKeys` falls back to the shared
///     offer secret (fine only for a single-enclave dev run, degenerate for a real
///     ceremony).
fn resolve_dkg_identity_seed(
    args: &[String],
    cli_seed: Option<[u8; 32]>,
) -> (Option<[u8; 32]>, &'static str) {
    if let Some(seed) = cli_seed {
        return (Some(seed), "explicit --dkg-seed");
    }
    // Self-gen needs a tee-dir to persist the sealed identity and real EGETKEY.
    let Some(tee_dir) = arg_value(args, "--tee-dir").map(std::path::PathBuf::from) else {
        return (None, "offer-secret fallback (no seed, no --tee-dir)");
    };
    let sealing_key = match crate::gramine::sealing_key_256(true) {
        Ok(k) => k,
        Err(_) => return (None, "offer-secret fallback (no EGETKEY — not real SGX)"),
    };
    let chain_id = B256::from(
        arg_value(args, "--chain-id")
            .and_then(|h| parse_hex32(&h))
            .unwrap_or([0u8; 32]),
    );
    // Running SGX SVN for the anti-rollback floor, read from the local report.
    let isv_svn = crate::gramine::local_report_measurements(&[0u8; 64])
        .map(|m| m.isv_svn)
        .unwrap_or(0);
    // The tee dir is also created later by `build_boot_config`; ensure it exists
    // now so first-boot identity sealing can persist.
    let _ = std::fs::create_dir_all(&tee_dir);
    let path = tee_dir.join("sealed_identity.bin");

    // Restore a previously self-generated identity if one is sealed here.
    if let Ok(blob) = std::fs::read(&path) {
        match unseal_tribute_offer_and_group_sig(&blob, &sealing_key, chain_id, isv_svn) {
            Ok((seed, _empty_group_sig, _hdr)) => {
                return (Some(*seed), "self-generated (restored from seal)");
            }
            Err(err) => {
                eprintln!(
                    "outbe-tee-enclave: sealed identity at {} did not unseal ({err}); \
                     regenerating",
                    path.display()
                );
            }
        }
    }

    // First boot (or unreadable blob): generate a fresh random identity and seal it.
    let mut seed = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut seed);
    let mut nonce = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut nonce);
    let header = SealHeader {
        format_version: SEAL_FORMAT,
        key_policy: KeyPolicy::MrSigner,
        isv_svn,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        nonce,
    };
    match seal_tribute_offer_and_group_sig(&seed, &[], &sealing_key, chain_id, &header) {
        Ok(blob) => match std::fs::write(&path, blob) {
            Ok(()) => (Some(seed), "self-generated (fresh, sealed)"),
            Err(err) => {
                eprintln!(
                    "outbe-tee-enclave: could not persist sealed identity to {} ({err}); \
                     identity will NOT survive restart",
                    path.display()
                );
                (Some(seed), "self-generated (fresh, NOT persisted)")
            }
        },
        Err(err) => {
            eprintln!("outbe-tee-enclave: identity seal failed ({err}); identity not persisted");
            (Some(seed), "self-generated (fresh, NOT persisted)")
        }
    }
}

/// Build the optional seal/unseal boot configuration from CLI args.
///
/// Returns `Ok(Some)` only when `--tee-dir <path>` is supplied; the directory is
/// created (best-effort 0700) and `--chain-id <hex32>` binds the sealing AAD.
/// Absent `--tee-dir` → `Ok(None)`: sealing is disabled and behavior is unchanged
/// (the offer key is re-derived from the DKG each boot). `Err(code)` propagates a
/// fatal setup failure as a process exit code. `isv_svn` is the running enclave's
/// SVN, the anti-rollback floor for unseal.
fn build_boot_config(
    args: &[String],
    keys: &EnclaveKeys,
) -> Result<Option<std::sync::Arc<EnclaveBootConfig>>, i32> {
    let Some(tee_dir) = arg_value(args, "--tee-dir").map(std::path::PathBuf::from) else {
        return Ok(None);
    };
    if let Err(err) = std::fs::create_dir_all(&tee_dir) {
        eprintln!(
            "outbe-tee-enclave: create --tee-dir {} failed: {err}",
            tee_dir.display()
        );
        return Err(1);
    }
    // Owner-only (0700); best-effort under Gramine (emulated FS is not chmod-able).
    if let Err(err) = std::fs::set_permissions(&tee_dir, std::fs::Permissions::from_mode(0o700)) {
        eprintln!(
            "outbe-tee-enclave: chmod 0700 {} failed (continuing): {err}",
            tee_dir.display()
        );
    }
    let chain_id = arg_value(args, "--chain-id")
        .and_then(|h| parse_hex32(&h))
        .unwrap_or_else(|| {
            eprintln!(
                "outbe-tee-enclave: --tee-dir set without a valid --chain-id; \
                 sealing AAD uses ZERO chain_id"
            );
            [0u8; 32]
        });
    let cfg = EnclaveBootConfig::new(chain_id, tee_dir, keys.isv_svn());
    eprintln!(
        "outbe-tee-enclave: sealing enabled (tee_dir={}, isv_svn={})",
        cfg.tee_dir.display(),
        cfg.isv_svn,
    );
    Ok(Some(std::sync::Arc::new(cfg)))
}

/// Return the value following `flag` in `args`, if present.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Read a 32-byte hex value from `var` (optional `0x`); fall back to `default`
/// on absence or malformed input.
fn dev_bytes_from_env(var: &str, default: [u8; 32]) -> [u8; 32] {
    std::env::var(var)
        .ok()
        .and_then(|value| parse_hex32(&value))
        .unwrap_or(default)
}

/// Parse a 32-byte hex string (optional `0x`); `None` on malformed input.
fn parse_hex32(value: &str) -> Option<[u8; 32]> {
    let trimmed = value.strip_prefix("0x").unwrap_or(value);
    match hex::decode(trimmed) {
        Ok(bytes) if bytes.len() == 32 => {
            let mut out = [0u8; 32];
            out.copy_from_slice(&bytes);
            Some(out)
        }
        _ => None,
    }
}

/// True iff `endpoint` is an `ip:port` whose IP is loopback (`127.0.0.0/8` or
/// `::1`). A non-IP host (e.g. `example.com:7000`) does not parse as a
/// `SocketAddr` and is rejected — the TCP carrier must never bind a routable
/// address.
fn is_loopback_endpoint(endpoint: &str) -> bool {
    endpoint
        .parse::<std::net::SocketAddr>()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::is_loopback_endpoint;

    #[test]
    fn loopback_endpoint_accepts_only_loopback_ips() {
        assert!(is_loopback_endpoint("127.0.0.1:7000"));
        assert!(is_loopback_endpoint("127.0.0.5:7000"));
        assert!(is_loopback_endpoint("[::1]:7000"));
        // Non-loopback / routable → reject.
        assert!(!is_loopback_endpoint("0.0.0.0:7000"));
        assert!(!is_loopback_endpoint("10.0.0.5:7000"));
        assert!(!is_loopback_endpoint("192.168.1.2:7000"));
        // Non-IP host → reject (cannot prove loopback).
        assert!(!is_loopback_endpoint("example.com:7000"));
        assert!(!is_loopback_endpoint("localhost:7000"));
    }
}
