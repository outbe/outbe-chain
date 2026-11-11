//! Node-side collection of the AMD certificates that accompany an SEV-SNP
//! attestation report in registration evidence.
//!
//! Report verification is deterministic and never uses the network. The
//! sender of the evidence therefore supplies the three certificates. This
//! module finds them in this order:
//!
//! 1. the certificate table that the host passes to the guest with a report;
//! 2. files in the directory that [`SEV_SNP_CERTIFICATE_DIR_ENV`] names;
//! 3. for the root key and the signing key only: the AMD certificates built
//!    into the `sev` crate.
//!
//! The root key certificate must hash to the value pinned in the attestation
//! policy. The chip certificate (VCEK) has no built-in source: when the host
//! table does not carry it, an operator downloads it one time from the AMD key
//! distribution service and puts it in the directory.

use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::MAX_SEV_SNP_CERTIFICATE_BYTES;
use sev::certs::snp::{builtin, Certificate};
use sha2::{Digest as _, Sha256};

use crate::sev_snp_v1::{verify_sev_snp_report_v1, SevSnpCertificatesV1, SevSnpRejectV1};

/// Environment variable that names the directory with AMD certificate files.
///
/// Accepted files: `vcek.der` or `vcek.pem`, `ask.der` or `ask.pem`, `ark.der`
/// or `ark.pem`, and `cert_chain.pem` (the signing key certificate followed by
/// the root key certificate, as the AMD key distribution service returns it).
pub const SEV_SNP_CERTIFICATE_DIR_ENV: &str = "OUTBE_SEV_SNP_CERT_DIR";

const TSM_REPORT_DIR: &str = "/sys/kernel/config/tsm/report";
const MAX_TABLE_BYTES: u64 = 64 * 1024;
const TABLE_ENTRY_LEN: usize = 24;
const PEM_BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";

/// GUIDs of the certificate table. The table stores a GUID in the byte order
/// of its text form, as the host tool of the `sev` crate writes it.
const ARK_GUID: [u8; 16] = [
    0xc0, 0xb4, 0x06, 0xa4, 0xa8, 0x03, 0x49, 0x52, 0x97, 0x43, 0x3f, 0xb6, 0x01, 0x4c, 0xd0, 0xae,
];
const ASK_GUID: [u8; 16] = [
    0x4a, 0xb7, 0xb3, 0x79, 0xbb, 0xac, 0x4f, 0xe4, 0xa0, 0x2f, 0x05, 0xae, 0xf3, 0x27, 0xc7, 0x82,
];
const VCEK_GUID: [u8; 16] = [
    0x63, 0xda, 0x75, 0x8d, 0xe6, 0x64, 0x45, 0x64, 0xad, 0xc5, 0xf4, 0xb9, 0x3b, 0xe8, 0xac, 0xcd,
];

/// The three AMD certificates of one piece of evidence, DER-encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SevSnpHostCertificatesV1 {
    pub ark_der: Vec<u8>,
    pub ask_der: Vec<u8>,
    pub vcek_der: Vec<u8>,
}

/// Failure to collect a certificate chain that verifies the report.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SevSnpHostError {
    /// No source has the chip certificate.
    #[error(
        "no VCEK certificate: the host certificate table does not carry it \
         ({table}) and {SEV_SNP_CERTIFICATE_DIR_ENV} has no vcek.der or vcek.pem"
    )]
    VcekUnavailable { table: String },
    /// No source has the signing key certificate.
    #[error("no ASK certificate in the host certificate table, the certificate directory or the built-in AMD certificates")]
    AskUnavailable,
    /// No root key certificate hashes to the value pinned in the policy.
    #[error("no AMD root certificate with SHA-256 {pinned} in the host certificate table, the certificate directory or the built-in AMD certificates")]
    PinnedArkUnavailable { pinned: B256 },
    /// A certificate file is unreadable.
    #[error("read {path}: {detail}")]
    ReadFile { path: String, detail: String },
    /// A certificate is neither DER nor PEM, or exceeds the protocol limit.
    #[error("certificate from {source_name} is malformed or exceeds {MAX_SEV_SNP_CERTIFICATE_BYTES} bytes")]
    MalformedCertificate { source_name: String },
    /// The host certificate table does not have the kernel layout.
    #[error("malformed host certificate table: {0}")]
    MalformedTable(&'static str),
    /// The collected chain does not verify the report.
    #[error("the collected AMD certificates do not verify the report: {0:?}")]
    ReportRejected(SevSnpRejectV1),
}

/// Collect the certificates for `report` and check that they verify it.
///
/// `pinned_ark_sha256` is `intel_root_der_hash` of the attestation policy.
pub fn acquire_sev_snp_certificates_v1(
    report: &[u8],
    pinned_ark_sha256: &B256,
) -> Result<SevSnpHostCertificatesV1, SevSnpHostError> {
    let table = read_host_certificate_table();
    let directory = std::env::var_os(SEV_SNP_CERTIFICATE_DIR_ENV).map(PathBuf::from);
    let certificates = select_certificates(table, directory.as_deref(), pinned_ark_sha256)?;
    verify_sev_snp_report_v1(
        report,
        SevSnpCertificatesV1 {
            ark_der: &certificates.ark_der,
            ask_der: &certificates.ask_der,
            vcek_der: &certificates.vcek_der,
        },
        pinned_ark_sha256,
    )
    .map_err(SevSnpHostError::ReportRejected)?;
    Ok(certificates)
}

/// Read the certificate table through the kernel TSM report interface.
///
/// The table does not depend on the report data, so the request binds zeros.
fn read_host_certificate_table() -> Result<Vec<u8>, String> {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicU64, Ordering};

    static REQUEST: AtomicU64 = AtomicU64::new(0);
    let entry = Path::new(TSM_REPORT_DIR).join(format!(
        "outbe-node-{}-{}",
        std::process::id(),
        REQUEST.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&entry).map_err(|error| format!("create {}: {error}", entry.display()))?;
    let result = (|| {
        // Open without truncation: a configfs attribute has no truncate operation.
        std::fs::OpenOptions::new()
            .write(true)
            .open(entry.join("inblob"))
            .and_then(|mut file| file.write_all(&[0; 64]))
            .map_err(|error| format!("write inblob: {error}"))?;
        let mut table = Vec::new();
        std::fs::File::open(entry.join("auxblob"))
            .and_then(|file| file.take(MAX_TABLE_BYTES + 1).read_to_end(&mut table))
            .map_err(|error| format!("read auxblob: {error}"))?;
        if table.len() as u64 > MAX_TABLE_BYTES {
            return Err("auxblob exceeds the size limit".to_owned());
        }
        Ok(table)
    })();
    // The kernel frees the request state when its directory is removed.
    let _ = std::fs::remove_dir(&entry);
    result
}

#[derive(Default)]
struct Found {
    ark: Vec<Vec<u8>>,
    ask: Option<Vec<u8>>,
    vcek: Option<Vec<u8>>,
}

fn select_certificates(
    table: Result<Vec<u8>, String>,
    directory: Option<&Path>,
    pinned_ark_sha256: &B256,
) -> Result<SevSnpHostCertificatesV1, SevSnpHostError> {
    let mut found = Found::default();
    let table_state = match &table {
        Ok(bytes) if bytes.is_empty() => "the table is empty".to_owned(),
        Ok(bytes) => {
            add_table(&mut found, bytes)?;
            "the table has no VCEK entry".to_owned()
        }
        Err(error) => format!("the table is unavailable: {error}"),
    };
    if let Some(directory) = directory {
        add_directory(&mut found, directory)?;
    }

    let generations: [(&[u8], &[u8]); 3] = [
        (builtin::milan::ARK, builtin::milan::ASK),
        (builtin::genoa::ARK, builtin::genoa::ASK),
        (builtin::turin::ARK, builtin::turin::ASK),
    ];
    let is_pinned = |der: &[u8]| Sha256::digest(der).as_slice() == pinned_ark_sha256.as_slice();
    let mut builtin_ask = None;
    let mut ark = found.ark.into_iter().find(|der| is_pinned(der));
    for (ark_pem, ask_pem) in generations {
        let ark_der = normalize(ark_pem, "built-in ARK")?;
        if is_pinned(&ark_der) {
            builtin_ask = Some(normalize(ask_pem, "built-in ASK")?);
            ark.get_or_insert(ark_der);
        }
    }
    let ark_der = ark.ok_or(SevSnpHostError::PinnedArkUnavailable {
        pinned: *pinned_ark_sha256,
    })?;
    let ask_der = found
        .ask
        .or(builtin_ask)
        .ok_or(SevSnpHostError::AskUnavailable)?;
    let vcek_der = found
        .vcek
        .ok_or(SevSnpHostError::VcekUnavailable { table: table_state })?;
    Ok(SevSnpHostCertificatesV1 {
        ark_der,
        ask_der,
        vcek_der,
    })
}

/// Add the certificates of a kernel certificate table.
///
/// The table is a list of 24-byte entries (GUID, offset, length) that a zero
/// entry terminates. Offsets count from the start of the table.
fn add_table(found: &mut Found, table: &[u8]) -> Result<(), SevSnpHostError> {
    let mut cursor = 0;
    loop {
        let entry = table
            .get(cursor..cursor + TABLE_ENTRY_LEN)
            .ok_or(SevSnpHostError::MalformedTable("no terminating entry"))?;
        if entry.iter().all(|byte| *byte == 0) {
            return Ok(());
        }
        let field = |at: usize| {
            u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]]) as usize
        };
        let (offset, length) = (field(16), field(20));
        let bytes = offset
            .checked_add(length)
            .and_then(|end| table.get(offset..end))
            .ok_or(SevSnpHostError::MalformedTable("entry outside the table"))?;
        let guid: [u8; 16] = entry[..16].try_into().unwrap_or([0; 16]);
        if guid == ARK_GUID {
            found.ark.push(normalize(bytes, "host table ARK")?);
        } else if guid == ASK_GUID {
            found.ask = Some(normalize(bytes, "host table ASK")?);
        } else if guid == VCEK_GUID {
            found.vcek = Some(normalize(bytes, "host table VCEK")?);
        }
        cursor += TABLE_ENTRY_LEN;
    }
}

/// Add the certificate files of `directory`. A table entry has priority.
fn add_directory(found: &mut Found, directory: &Path) -> Result<(), SevSnpHostError> {
    let read = |name: &str| -> Result<Option<Vec<u8>>, SevSnpHostError> {
        let path = directory.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SevSnpHostError::ReadFile {
                path: path.display().to_string(),
                detail: error.to_string(),
            }),
        }
    };
    let first = |names: [&str; 2]| -> Result<Option<Vec<u8>>, SevSnpHostError> {
        for name in names {
            if let Some(bytes) = read(name)? {
                return normalize(&bytes, name).map(Some);
            }
        }
        Ok(None)
    };
    if found.vcek.is_none() {
        found.vcek = first(["vcek.der", "vcek.pem"])?;
    }
    if found.ask.is_none() {
        found.ask = first(["ask.der", "ask.pem"])?;
    }
    if let Some(ark) = first(["ark.der", "ark.pem"])? {
        found.ark.push(ark);
    }
    if let Some(chain) = read("cert_chain.pem")? {
        let mut blocks = pem_blocks(&chain).into_iter();
        if let Some(ask) = blocks.next() {
            let ask = normalize(ask, "cert_chain.pem")?;
            found.ask.get_or_insert(ask);
        }
        if let Some(ark) = blocks.next() {
            found.ark.push(normalize(ark, "cert_chain.pem")?);
        }
    }
    Ok(())
}

/// Split a PEM bundle into its certificate blocks.
fn pem_blocks(bundle: &[u8]) -> Vec<&[u8]> {
    let starts: Vec<usize> = bundle
        .windows(PEM_BEGIN.len())
        .enumerate()
        .filter_map(|(index, window)| (window == PEM_BEGIN).then_some(index))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(bundle.len());
            &bundle[*start..end]
        })
        .collect()
}

/// Convert a DER or PEM certificate into canonical DER within the protocol limit.
fn normalize(bytes: &[u8], source_name: &str) -> Result<Vec<u8>, SevSnpHostError> {
    let malformed = || SevSnpHostError::MalformedCertificate {
        source_name: source_name.to_owned(),
    };
    let certificate = if bytes.trim_ascii_start().starts_with(PEM_BEGIN) {
        Certificate::from_pem(bytes.trim_ascii())
    } else {
        Certificate::from_der(bytes)
    }
    .map_err(|_| malformed())?;
    let der = certificate.to_der().map_err(|_| malformed())?;
    if der.is_empty() || der.len() > MAX_SEV_SNP_CERTIFICATE_BYTES {
        return Err(malformed());
    }
    Ok(der)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MILAN_REPORT: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-report.bin");
    const MILAN_VCEK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-vcek.der");
    const MILAN_ARK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-ark.der");
    const MILAN_ASK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-ask.der");
    const SIMULATOR_TABLE: &[u8] =
        include_bytes!("../tests/fixtures/sev_snp/simulator-cert-table.bin");

    fn sha256(bytes: &[u8]) -> B256 {
        B256::from_slice(&Sha256::digest(bytes))
    }

    fn no_table() -> Result<Vec<u8>, String> {
        Err("absent".to_owned())
    }

    /// Kernel certificate table with the given entries.
    fn table(entries: &[([u8; 16], &[u8])]) -> Vec<u8> {
        let mut offset = (entries.len() + 1) * TABLE_ENTRY_LEN;
        let mut header = Vec::new();
        let mut body = Vec::new();
        for (guid, bytes) in entries {
            header.extend_from_slice(guid);
            header.extend_from_slice(&(offset as u32).to_le_bytes());
            header.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            body.extend_from_slice(bytes);
            offset += bytes.len();
        }
        header.extend_from_slice(&[0; TABLE_ENTRY_LEN]);
        header.extend_from_slice(&body);
        header
    }

    fn verifies(certificates: &SevSnpHostCertificatesV1) -> bool {
        verify_sev_snp_report_v1(
            MILAN_REPORT,
            SevSnpCertificatesV1 {
                ark_der: &certificates.ark_der,
                ask_der: &certificates.ask_der,
                vcek_der: &certificates.vcek_der,
            },
            &sha256(MILAN_ARK),
        )
        .is_ok()
    }

    #[test]
    fn guid_constants_match_the_simulator_table() {
        // The simulator table carries an ASK entry and a VCEK entry.
        assert_eq!(SIMULATOR_TABLE[..16], ASK_GUID);
        assert_eq!(SIMULATOR_TABLE[24..40], VCEK_GUID);
    }

    #[test]
    fn host_table_with_vcek_uses_the_built_in_root_and_signing_key() {
        let table = table(&[(VCEK_GUID, MILAN_VCEK)]);
        let certificates = select_certificates(Ok(table), None, &sha256(MILAN_ARK)).unwrap();
        assert_eq!(certificates.ark_der, MILAN_ARK);
        assert_eq!(certificates.ask_der, MILAN_ASK);
        assert_eq!(certificates.vcek_der, MILAN_VCEK);
        assert!(verifies(&certificates));
    }

    #[test]
    fn full_host_table_is_used_as_is() {
        let table = table(&[
            (VCEK_GUID, MILAN_VCEK),
            (ASK_GUID, MILAN_ASK),
            (ARK_GUID, MILAN_ARK),
        ]);
        let certificates = select_certificates(Ok(table), None, &sha256(MILAN_ARK)).unwrap();
        assert!(verifies(&certificates));
    }

    #[test]
    fn empty_table_falls_back_to_the_certificate_directory() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("vcek.der"), MILAN_VCEK).unwrap();
        let chain = [builtin::milan::ASK, builtin::milan::ARK].concat();
        std::fs::write(directory.path().join("cert_chain.pem"), chain).unwrap();
        for table in [Ok(Vec::new()), no_table()] {
            let certificates =
                select_certificates(table, Some(directory.path()), &sha256(MILAN_ARK)).unwrap();
            assert!(verifies(&certificates));
        }
    }

    #[test]
    fn missing_vcek_names_the_table_state_and_the_directory_variable() {
        for (table, state) in [
            (Ok(Vec::new()), "the table is empty"),
            (no_table(), "the table is unavailable: absent"),
            (Ok(table(&[(ASK_GUID, MILAN_ASK)])), "no VCEK entry"),
        ] {
            let error = select_certificates(table, None, &sha256(MILAN_ARK)).unwrap_err();
            let message = error.to_string();
            assert!(message.contains(state), "{message}");
            assert!(message.contains(SEV_SNP_CERTIFICATE_DIR_ENV), "{message}");
        }
    }

    #[test]
    fn root_that_is_not_pinned_is_rejected() {
        let table = table(&[(VCEK_GUID, MILAN_VCEK), (ARK_GUID, MILAN_ARK)]);
        let pinned = B256::repeat_byte(0x11);
        assert_eq!(
            select_certificates(Ok(table), None, &pinned),
            Err(SevSnpHostError::PinnedArkUnavailable { pinned })
        );
    }

    #[test]
    fn malformed_table_and_certificate_are_rejected() {
        // No terminating entry.
        let mut truncated = table(&[(VCEK_GUID, MILAN_VCEK)]);
        truncated.truncate(TABLE_ENTRY_LEN);
        assert!(matches!(
            select_certificates(Ok(truncated), None, &sha256(MILAN_ARK)),
            Err(SevSnpHostError::MalformedTable(_))
        ));
        // Entry that points outside the table.
        let mut outside = table(&[(VCEK_GUID, MILAN_VCEK)]);
        outside[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            select_certificates(Ok(outside), None, &sha256(MILAN_ARK)),
            Err(SevSnpHostError::MalformedTable(_))
        ));
        // Entry that is not a certificate.
        let garbage = table(&[(VCEK_GUID, b"not a certificate")]);
        assert!(matches!(
            select_certificates(Ok(garbage), None, &sha256(MILAN_ARK)),
            Err(SevSnpHostError::MalformedCertificate { .. })
        ));
    }

    #[test]
    fn collected_chain_of_another_chip_does_not_verify_the_report() {
        // The simulator VCEK is not the key that signed the Milan report.
        let simulator_vcek_offset =
            u32::from_le_bytes(SIMULATOR_TABLE[40..44].try_into().unwrap()) as usize;
        let simulator_vcek_len =
            u32::from_le_bytes(SIMULATOR_TABLE[44..48].try_into().unwrap()) as usize;
        let simulator_vcek =
            &SIMULATOR_TABLE[simulator_vcek_offset..simulator_vcek_offset + simulator_vcek_len];
        let table = table(&[(VCEK_GUID, simulator_vcek)]);
        let certificates = select_certificates(Ok(table), None, &sha256(MILAN_ARK)).unwrap();
        assert!(!verifies(&certificates));
    }
}
