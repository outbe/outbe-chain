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

use crate::keys::EnclaveKeys;
use crate::seal::EnclaveBootConfig;
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
    // DKG participant identity seed. An explicit `--dkg-seed <hex32>` (or env
    // `TEE_DEV_DKG_SEED`) is honored for dev/test. Otherwise the enclave derives a
    // STABLE, DISTINCT identity from a random seed sealed under `--tee-dir` — so it
    // is a real distinct DKG participant WITHOUT a manually injected seed. We never
    // fall back to the shared dev offer secret (which makes all `n` enclaves the
    // same participant and stalls the ceremony): with no `--dkg-seed` and no sealed
    // identity we fail-fast rather than hang.
    let explicit_seed = arg_value(&args, "--dkg-seed")
        .and_then(|hex| parse_hex32(&hex))
        .or_else(|| {
            std::env::var("TEE_DEV_DKG_SEED")
                .ok()
                .and_then(|h| parse_hex32(&h))
        });
    let (dkg_seed, dkg_id): (zeroize::Zeroizing<[u8; 32]>, &str) = match explicit_seed {
        Some(seed) => (zeroize::Zeroizing::new(seed), "explicit --dkg-seed"),
        None => {
            let chain_id = arg_value(&args, "--chain-id")
                .and_then(|h| parse_hex32(&h))
                .unwrap_or([0u8; 32]);
            match arg_value(&args, "--tee-dir") {
                Some(dir) => match crate::transport::load_or_create_sealed_dkg_seed(
                    std::path::Path::new(&dir),
                    chain_id,
                ) {
                    Ok(seed) => (seed, "sealed auto-identity"),
                    Err(err) => {
                        eprintln!("outbe-tee-enclave: DKG identity setup failed: {err}");
                        return 1;
                    }
                },
                None => {
                    eprintln!(
                        "outbe-tee-enclave: refusing to start — no --dkg-seed and no --tee-dir. \
                         Without a sealed identity every enclave shares the dev DKG seed and the \
                         ceremony stalls. Run under gramine-sgx (or the mock build) with --tee-dir \
                         for an automatic sealed identity, or pass --dkg-seed for a dev run."
                    );
                    return 1;
                }
            }
        }
    };
    let keys = match EnclaveKeys::new(tribute_offer_secret, Some(*dkg_seed)) {
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
