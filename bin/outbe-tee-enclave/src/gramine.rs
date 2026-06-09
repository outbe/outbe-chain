//! Real Gramine attestation surface (`/dev/attestation/*`).
//!
//! This is the genuine SGX integration — no mock constants. Under `gramine-sgx`
//! these pseudo-files are backed by hardware: `quote` is a real DCAP quote and
//! the keys under `keys/` come from `EGETKEY`. Under `gramine-direct` there is no
//! SGX hardware, so `attestation_type` reads `none`, `quote` is unavailable, and
//! `keys/` does not exist — this module reports that honestly (no fabricated
//! quote, no fixed sealing key). Outside Gramine entirely (bare process) every
//! pseudo-file is absent and every call returns the "unavailable" path.
//!
//! Hardware MRENCLAVE/MRSIGNER/ISVSVN are parsed out of the real quote's embedded
//! SGX report body — they are never hardcoded.

use std::fs;

// The DCAP quote parser is shared with the host (single source of truth).
pub use outbe_tee::quote::{parse_quote_measurements, ReportMeasurements, MIN_QUOTE_LEN};

const ATTEST_DIR: &str = "/dev/attestation";

/// What the running environment can attest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttestationType {
    /// `gramine-direct` (or any non-SGX Gramine): no hardware, no real quote.
    None,
    /// `gramine-sgx` with DCAP remote attestation.
    Dcap,
    /// `gramine-sgx` with legacy EPID.
    Epid,
    /// Gramine reported a type we do not specifically handle.
    Other(String),
    /// Not running under Gramine at all (`/dev/attestation` absent).
    Unavailable,
}

impl AttestationType {
    /// True only when a real SGX hardware quote can be produced.
    pub fn is_hardware(&self) -> bool {
        matches!(self, AttestationType::Dcap | AttestationType::Epid)
    }

    pub fn label(&self) -> String {
        match self {
            AttestationType::None => "none (gramine-direct / no SGX)".to_string(),
            AttestationType::Dcap => "dcap (gramine-sgx)".to_string(),
            AttestationType::Epid => "epid (gramine-sgx)".to_string(),
            AttestationType::Other(s) => format!("other:{s}"),
            AttestationType::Unavailable => "unavailable (not under Gramine)".to_string(),
        }
    }
}

/// Classify the attestation environment.
///
/// Grounded in observed behaviour: under `gramine-sgx` the
/// `/dev/attestation/attestation_type` pseudo-file reads `dcap`/`epid`; under
/// `gramine-direct` the `/dev/attestation` directory exists but real attestation
/// is unavailable (the type file is absent/`none`, `keys/` is empty, and
/// `user_report_data` is not writable); a bare process has no `/dev/attestation`
/// at all.
pub fn attestation_type() -> AttestationType {
    if let Ok(s) = fs::read_to_string(format!("{ATTEST_DIR}/attestation_type")) {
        return match s.trim() {
            "dcap" => AttestationType::Dcap,
            "epid" => AttestationType::Epid,
            "none" | "" => AttestationType::None,
            other => AttestationType::Other(other.to_string()),
        };
    }
    // No readable type file. If the Gramine attestation dir exists at all we are
    // under Gramine without working hardware attestation (gramine-direct);
    // otherwise we are not under Gramine.
    if fs::metadata(ATTEST_DIR).is_ok() {
        AttestationType::None
    } else {
        AttestationType::Unavailable
    }
}

/// Produce a real SGX DCAP quote binding `report_data` (64 bytes): write
/// `user_report_data`, then read back `quote`. Only succeeds under `gramine-sgx`
/// with DCAP remote attestation; under `gramine-direct` the `quote` read fails
/// and this returns `Err` (the caller degrades honestly rather than faking).
pub fn dcap_quote(report_data: &[u8; 64]) -> Result<Vec<u8>, String> {
    let urd = format!("{ATTEST_DIR}/user_report_data");
    fs::write(&urd, &report_data[..]).map_err(|e| format!("write {urd}: {e}"))?;
    let qf = format!("{ATTEST_DIR}/quote");
    let quote = fs::read(&qf).map_err(|e| format!("read {qf}: {e}"))?;
    if quote.len() < MIN_QUOTE_LEN {
        return Err(format!(
            "quote too short: {} < {MIN_QUOTE_LEN}",
            quote.len()
        ));
    }
    Ok(quote)
}

/// EGETKEY-derived sealing key from Gramine's `/dev/attestation/keys/<name>`.
/// `mrsigner` survives an enclave update by the same signer; `mrenclave` is
/// strict per-build. Gramine returns a 128-bit key; we expand it to 256 bits with
/// the caller's HKDF. Only available under `gramine-sgx`.
pub fn sealing_key_raw(mrsigner_policy: bool) -> Result<Vec<u8>, String> {
    let name = if mrsigner_policy {
        "_sgx_mrsigner"
    } else {
        "_sgx_mrenclave"
    };
    let path = format!("{ATTEST_DIR}/keys/{name}");
    let key = fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
    if key.is_empty() {
        return Err(format!("{path} returned empty key"));
    }
    Ok(key)
}

/// 256-bit sealing key for the `TSEAL` blob, HKDF-expanded from the real 128-bit
/// EGETKEY key. This is the production sealing-key source: it ties the sealed
/// root seed to MRSIGNER (survives a same-signer enclave update). Returns `Err`
/// when no SGX hardware is present (gramine-direct/bare), where there is no
/// confidential at-rest persistence — the caller must not silently substitute a
/// fixed key.
pub fn sealing_key_256(mrsigner_policy: bool) -> Result<[u8; 32], String> {
    let raw = sealing_key_raw(mrsigner_policy)?;
    crate::crypto::hkdf_sha256(&raw, b"", b"outbe/tee/seal-key/v1").map_err(|e| e.to_string())
}

/// Diagnostic dump of the whole attestation surface — used by the
/// `--probe-attestation` startup mode to observe real Gramine behaviour instead
/// of guessing it. Prints to stderr; performs no secret-revealing reads (the
/// sealing key bytes are summarised by length only).
pub fn probe_to_stderr() {
    eprintln!("=== gramine /dev/attestation probe ===");
    let at = attestation_type();
    eprintln!("attestation_type: {}", at.label());

    match fs::read_dir(ATTEST_DIR) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect();
            names.sort();
            eprintln!("/dev/attestation entries: {names:?}");
        }
        Err(e) => eprintln!("/dev/attestation readdir: {e}"),
    }
    match fs::read_dir(format!("{ATTEST_DIR}/keys")) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect();
            names.sort();
            eprintln!("/dev/attestation/keys entries: {names:?}");
        }
        Err(e) => eprintln!("/dev/attestation/keys readdir: {e}"),
    }

    let rd = [0u8; 64];
    match dcap_quote(&rd) {
        Ok(q) => {
            eprintln!("dcap_quote: {} bytes", q.len());
            match parse_quote_measurements(&q) {
                Ok(m) => eprintln!(
                    "  mrenclave={} mrsigner={} isv_svn={}",
                    hex::encode(m.mrenclave),
                    hex::encode(m.mrsigner),
                    m.isv_svn
                ),
                Err(e) => eprintln!("  parse: {e}"),
            }
        }
        Err(e) => eprintln!("dcap_quote: UNAVAILABLE ({e})"),
    }
    for policy in [true, false] {
        match sealing_key_raw(policy) {
            Ok(k) => eprintln!(
                "sealing_key({}): {} bytes",
                if policy { "mrsigner" } else { "mrenclave" },
                k.len()
            ),
            Err(e) => eprintln!(
                "sealing_key({}): UNAVAILABLE ({e})",
                if policy { "mrsigner" } else { "mrenclave" }
            ),
        }
    }
    eprintln!("=== end probe ===");
}
