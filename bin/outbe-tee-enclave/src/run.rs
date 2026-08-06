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
use zeroize::Zeroizing;

use crate::initialization::InitializationState;
use crate::keys::EnclaveKeys;
use crate::seal::{
    seal_tribute_offer_and_group_sig, unseal_tribute_offer_and_group_sig, EnclaveBootConfig,
    SealHeader, SEAL_FORMAT,
};
use crate::transport::{serve, serve_tcp};

/// Per-binary behavior knobs. The production binary uses [`RunOpts::prod`]; the
/// dev mock binary has its feature-gated constructor. The private field prevents
/// an external production caller from selecting development behavior.
#[derive(Clone, Copy, Debug)]
pub struct RunOpts {
    /// True for the unattested dev mock binary (stable sealing key and
    /// gramine-direct semantics). Never set in the production binary.
    #[cfg(feature = "mock")]
    mock: bool,
}

impl RunOpts {
    /// Production binary: real Gramine attestation/sealing surface, no mock code.
    pub fn prod() -> Self {
        Self {
            #[cfg(feature = "mock")]
            mock: false,
        }
    }

    /// Dev mock binary (only compiled under `--features mock`).
    #[cfg(feature = "mock")]
    pub fn mock() -> Self {
        Self { mock: true }
    }

    #[cfg(feature = "mock")]
    fn is_mock(self) -> bool {
        self.mock
    }

    #[cfg(not(feature = "mock"))]
    fn is_mock(self) -> bool {
        false
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

    #[cfg(feature = "mock")]
    if opts.is_mock() {
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

    #[cfg(feature = "mock")]
    let tribute_offer_secret = if opts.is_mock() {
        dev_bytes_from_env("TEE_DEV_OFFER_SECRET", [0x07; 32])
    } else {
        [0u8; 32]
    };
    #[cfg(not(feature = "mock"))]
    let tribute_offer_secret = [0u8; 32];
    // Explicit, deterministic DKG identity seed (dev/CI): `--dkg-seed <hex32>` or
    // env `TEE_DEV_DKG_SEED`. The gramine-direct mock path uses this since it has
    // no EGETKEY and so cannot persist a self-generated identity across restart.
    let cli_dkg_seed = arg_value(&args, "--dkg-seed").and_then(|hex| parse_hex32(&hex));
    #[cfg(feature = "mock")]
    let cli_dkg_seed = if opts.is_mock() {
        cli_dkg_seed.or_else(|| {
            std::env::var("TEE_DEV_DKG_SEED")
                .ok()
                .and_then(|h| parse_hex32(&h))
        })
    } else {
        cli_dkg_seed
    };
    // Production accepts only an enclave-generated, successfully sealed seed.
    // Explicit deterministic seeds and the offer-secret fallback are mock-only.
    let (identity_seed, identity_id) =
        match resolve_enclave_identity_seed(&args, cli_dkg_seed, opts.is_mock()) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!("outbe-tee-enclave: identity init failed: {error}");
                return 1;
            }
        };
    let keys = match EnclaveKeys::new_with_identity_seed(tribute_offer_secret, identity_seed) {
        Ok(keys) => keys,
        Err(err) => {
            eprintln!("outbe-tee-enclave: key init failed: {err}");
            return 1;
        }
    };

    // Seal/unseal boot configuration. Production requires it; an explicitly
    // selected development process may omit it and starts as a fresh keyless
    // identity on its separate chain.
    let boot = match build_boot_config(&args, &keys) {
        Ok(boot) => boot,
        Err(code) => return code,
    };

    // Resident chain id from `--chain-id` (default ZERO), bound INDEPENDENTLY of
    // sealing: it scopes every state-key derivation and the owner-authorized
    // fidelity query, which cross-checks it against the node's chain. Sourcing it
    // from `--chain-id` here (not from the seal-only boot config) is what lets a
    // non-sealing enclave still answer chain-scoped queries.
    let chain_id = B256::from(
        arg_value(&args, "--chain-id")
            .and_then(|h| parse_hex32(&h))
            .unwrap_or([0u8; 32]),
    );

    // Shared, write-once permanent offer-key slot. A valid sealed blob restores
    // it. An existing blob that cannot be unsealed is terminal and is never
    // converted into a fresh ceremony or another key.
    let offer_key: crate::transport::SharedTributeOfferKey =
        std::sync::Arc::new(std::sync::OnceLock::new());
    if let Some(cfg) = boot.as_deref() {
        match crate::transport::unseal_tribute_offer_and_group_sig_on_boot(cfg) {
            Ok(Some(derived)) => {
                let _ = offer_key.set(derived);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("outbe-tee-enclave: {error}");
                return 1;
            }
        }
    }

    #[cfg(feature = "mock")]
    let initialization = if opts.is_mock() {
        InitializationState::development()
    } else {
        let Some(boot) = boot.clone() else {
            eprintln!(
                "outbe-tee-enclave: production mode requires --tee-dir for write-once node authorization"
            );
            return 2;
        };
        match InitializationState::production(boot, &keys) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("outbe-tee-enclave: initialization state failed: {error}");
                return 1;
            }
        }
    };
    #[cfg(not(feature = "mock"))]
    let initialization = {
        let Some(boot) = boot.clone() else {
            eprintln!(
                "outbe-tee-enclave: production mode requires --tee-dir for write-once node authorization"
            );
            return 2;
        };
        match InitializationState::production(boot, &keys) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("outbe-tee-enclave: initialization state failed: {error}");
                return 1;
            }
        }
    };
    let keys = std::sync::Arc::new(keys);
    let initialization = std::sync::Arc::new(initialization);

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
            "outbe-tee-enclave: listening on tcp://{socket} (attestation: {}; enclave identity: {identity_id})",
            attest.label()
        );
        serve_tcp(&listener, keys, boot, offer_key, initialization, chain_id)
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
            "outbe-tee-enclave: listening on {socket} (attestation: {}; enclave identity: {identity_id})",
            attest.label()
        );
        serve(&listener, keys, boot, offer_key, initialization, chain_id)
    };
    if let Err(err) = result {
        eprintln!("outbe-tee-enclave: serve error: {err}");
        return 1;
    }
    0
}

/// Resolve the one seed backing every persistent enclave identity key.
///
/// Development may use an explicit deterministic seed or the legacy offer-secret
/// fallback. Production permits neither: it restores an existing valid sealed
/// seed or creates and durably seals one on the first boot. Existing unreadable
/// state is never overwritten. If node authorization exists but the identity is
/// missing, startup fails: there is no recovery or implicit identity rotation.
fn resolve_enclave_identity_seed(
    args: &[String],
    cli_seed: Option<[u8; 32]>,
    development: bool,
) -> Result<(Option<Zeroizing<[u8; 32]>>, &'static str), String> {
    #[cfg(feature = "mock")]
    if development {
        return Ok(match cli_seed {
            Some(seed) => (Some(Zeroizing::new(seed)), "explicit development seed"),
            None => (None, "development offer-secret fallback"),
        });
    }
    #[cfg(not(feature = "mock"))]
    let _ = development;
    if cli_seed.is_some() {
        return Err("--dkg-seed and TEE_DEV_DKG_SEED are development-only".to_string());
    }

    let tee_dir = arg_value(args, "--tee-dir")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "production requires --tee-dir for sealed identity".to_string())?;
    let chain_id_hex = arg_value(args, "--chain-id")
        .ok_or_else(|| "production requires --chain-id <hex32>".to_string())?;
    let chain_id = B256::from(
        parse_hex32(&chain_id_hex)
            .ok_or_else(|| "production --chain-id must be exactly 32 bytes of hex".to_string())?,
    );
    let (sealing_key, key_policy) = crate::transport::sealing_key()
        .ok_or_else(|| "production identity requires an SGX sealing key".to_string())?;

    // Running SGX SVN for the anti-rollback floor, read from the local report.
    let isv_svn = crate::gramine::local_report_measurements(&[0u8; 64])
        .map(|m| m.isv_svn)
        .unwrap_or(0);
    std::fs::create_dir_all(&tee_dir)
        .map_err(|error| format!("create identity directory {}: {error}", tee_dir.display()))?;
    std::fs::set_permissions(&tee_dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("chmod identity directory {}: {error}", tee_dir.display()))?;
    let path = tee_dir.join("sealed_identity.bin");

    match std::fs::read(&path) {
        Ok(blob) => {
            let (seed, empty_group_signature, _) =
                unseal_tribute_offer_and_group_sig(&blob, &sealing_key, chain_id, isv_svn)
                    .map_err(|error| {
                        format!(
                            "sealed identity at {} is unusable and will not be replaced: {error}",
                            path.display()
                        )
                    })?;
            if !empty_group_signature.is_empty() {
                return Err(format!(
                    "sealed identity at {} contains unexpected group-signature bytes",
                    path.display()
                ));
            }
            return Ok((Some(seed), "self-generated (restored from seal)"));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!("read sealed identity {}: {error}", path.display()));
        }
    }

    let authorization_path = tee_dir.join("sealed_node_authorization_v1.bin");
    match std::fs::symlink_metadata(&authorization_path) {
        Ok(_) => {
            return Err(format!(
                "sealed identity is missing while node authorization exists at {}; old identity cannot be recovered",
                authorization_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "inspect sealed node authorization {}: {error}",
                authorization_path.display()
            ));
        }
    }

    // First boot only: generate and seal before any node authorization exists.
    let mut seed = Zeroizing::new([0u8; 32]);
    rand_core::OsRng.fill_bytes(&mut *seed);
    let mut nonce = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut nonce);
    let header = SealHeader {
        format_version: SEAL_FORMAT,
        key_policy,
        isv_svn,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        nonce,
    };
    let blob = seal_tribute_offer_and_group_sig(&seed, &[], &sealing_key, chain_id, &header)
        .map_err(|error| format!("seal first enclave identity: {error}"))?;
    crate::transport::write_once_0600(&path, &blob)
        .map_err(|error| format!("persist first sealed identity {}: {error}", path.display()))?;
    Ok((Some(seed), "self-generated (fresh, sealed)"))
}

/// Build the optional seal/unseal boot configuration from CLI args.
///
/// Returns `Ok(Some)` only when `--tee-dir <path>` is supplied; the directory is
/// created (best-effort 0700) and `--chain-id <hex32>` binds the sealing AAD.
/// Absent `--tee-dir` is permitted only for the explicitly selected development
/// process and returns `Ok(None)` with no durable permanent key. Production
/// rejects that configuration before serving. `Err(code)` propagates a fatal
/// setup failure; `isv_svn` is the anti-rollback floor for unseal.
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
#[cfg(feature = "mock")]
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
    use std::os::unix::fs::PermissionsExt as _;

    use super::{is_loopback_endpoint, resolve_enclave_identity_seed};

    fn production_args(root: &std::path::Path) -> Vec<String> {
        vec![
            "outbe-tee-enclave".to_string(),
            "--tee-dir".to_string(),
            root.display().to_string(),
            "--chain-id".to_string(),
            hex::encode([0x41; 32]),
        ]
    }

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

    #[test]
    fn production_identity_rejects_development_seed() {
        let root = tempfile::tempdir().unwrap();
        let error =
            resolve_enclave_identity_seed(&production_args(root.path()), Some([0x77; 32]), false)
                .unwrap_err();
        assert!(error.contains("development-only"));
        assert!(!root.path().join("sealed_identity.bin").exists());
    }

    #[test]
    fn first_production_identity_is_owner_only_and_restores_exactly() {
        let root = tempfile::tempdir().unwrap();
        let args = production_args(root.path());
        let (first, _) = resolve_enclave_identity_seed(&args, None, false).unwrap();
        let path = root.path().join("sealed_identity.bin");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let original_blob = std::fs::read(&path).unwrap();
        let (restored, _) = resolve_enclave_identity_seed(&args, None, false).unwrap();
        assert_eq!(first, restored);
        assert_eq!(std::fs::read(path).unwrap(), original_blob);
    }

    #[test]
    fn corrupt_production_identity_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sealed_identity.bin");
        let corrupt = b"corrupt-sealed-identity";
        std::fs::write(&path, corrupt).unwrap();
        let error =
            resolve_enclave_identity_seed(&production_args(root.path()), None, false).unwrap_err();
        assert!(error.contains("will not be replaced"));
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    #[test]
    fn missing_identity_with_existing_authorization_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let authorization = root.path().join("sealed_node_authorization_v1.bin");
        std::fs::write(&authorization, b"existing-authorization").unwrap();
        let error =
            resolve_enclave_identity_seed(&production_args(root.path()), None, false).unwrap_err();
        assert!(error.contains("old identity cannot be recovered"));
        assert!(!root.path().join("sealed_identity.bin").exists());
        assert_eq!(
            std::fs::read(authorization).unwrap(),
            b"existing-authorization"
        );
    }
}
