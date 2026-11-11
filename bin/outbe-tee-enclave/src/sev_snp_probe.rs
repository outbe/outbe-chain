//! Operator diagnostics for an AMD SEV-SNP guest.
//!
//! The probe prints the facts that decide how a validator in a confidential VM
//! is configured: the fields of the attestation report, the certificates that
//! the host hands to the guest, and whether the processor derives a sealing
//! key for this VM. Nothing here authenticates the report.

use crate::errors::{EnclaveResult, TsmError};
use crate::tsm::SEV_SNP_REPORT_LEN;

const CERT_TABLE_ENTRY_LEN: usize = 24;

const ARK_GUID: [u8; 16] = [
    0xc0, 0xb4, 0x06, 0xa4, 0xa8, 0x03, 0x49, 0x52, 0x97, 0x43, 0x3f, 0xb6, 0x01, 0x4c, 0xd0, 0xae,
];
const ASK_GUID: [u8; 16] = [
    0x4a, 0xb7, 0xb3, 0x79, 0xbb, 0xac, 0x4f, 0xe4, 0xa0, 0x2f, 0x05, 0xae, 0xf3, 0x27, 0xc7, 0x82,
];
const VCEK_GUID: [u8; 16] = [
    0x63, 0xda, 0x75, 0x8d, 0xe6, 0x64, 0x45, 0x64, 0xad, 0xc5, 0xf4, 0xb9, 0x3b, 0xe8, 0xac, 0xcd,
];
const VLEK_GUID: [u8; 16] = [
    0xa8, 0x07, 0x4b, 0xc2, 0xa2, 0x5a, 0x48, 0x3e, 0xaa, 0xe6, 0x39, 0xc0, 0x45, 0xa0, 0xb8, 0xa1,
];

/// Kind of a certificate in the SEV-SNP certificate table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SevSnpCertificateKind {
    /// AMD root key.
    Ark,
    /// AMD SEV signing key.
    Ask,
    /// Versioned chip endorsement key.
    Vcek,
    /// Versioned loaded endorsement key.
    Vlek,
    /// An entry with a GUID that this code does not know.
    Other([u8; 16]),
}

impl SevSnpCertificateKind {
    const fn from_guid(guid: [u8; 16]) -> Self {
        match guid {
            ARK_GUID => Self::Ark,
            ASK_GUID => Self::Ask,
            VCEK_GUID => Self::Vcek,
            VLEK_GUID => Self::Vlek,
            other => Self::Other(other),
        }
    }

    /// Short name for operator output.
    pub fn name(self) -> String {
        match self {
            Self::Ark => "ARK".to_owned(),
            Self::Ask => "ASK".to_owned(),
            Self::Vcek => "VCEK".to_owned(),
            Self::Vlek => "VLEK".to_owned(),
            Self::Other(guid) => format!("unknown:{}", hex::encode(guid)),
        }
    }
}

/// One certificate of the SEV-SNP certificate table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SevSnpCertificate<'a> {
    pub kind: SevSnpCertificateKind,
    pub bytes: &'a [u8],
}

/// Parse the certificate table that the kernel returns with an SEV-SNP report.
///
/// The table starts with 24-byte entries: a 16-byte GUID, a 32-bit offset and a
/// 32-bit length, both little-endian and relative to the start of the table.
/// An all-zero entry ends the list. An empty table is valid: the host is not
/// required to supply certificates.
pub fn sev_snp_certificate_table(table: &[u8]) -> EnclaveResult<Vec<SevSnpCertificate<'_>>> {
    let malformed = |detail: &str| TsmError::MalformedReport {
        platform: "SEV-SNP",
        detail: format!("certificate table: {detail}"),
    };
    let mut certificates = Vec::new();
    if table.is_empty() {
        return Ok(certificates);
    }
    let mut terminated = false;
    for entry in table.chunks_exact(CERT_TABLE_ENTRY_LEN) {
        if entry.iter().all(|byte| *byte == 0) {
            terminated = true;
            break;
        }
        let mut guid = [0_u8; 16];
        guid.copy_from_slice(&entry[..16]);
        let offset = u32::from_le_bytes([entry[16], entry[17], entry[18], entry[19]]) as usize;
        let length = u32::from_le_bytes([entry[20], entry[21], entry[22], entry[23]]) as usize;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| malformed("entry range overflows"))?;
        let bytes = table
            .get(offset..end)
            .ok_or_else(|| malformed("entry points outside the table"))?;
        certificates.push(SevSnpCertificate {
            kind: SevSnpCertificateKind::from_guid(guid),
            bytes,
        });
    }
    if !terminated {
        return Err(malformed("the entry list has no terminator").into());
    }
    Ok(certificates)
}

fn u32_at(report: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        report[offset],
        report[offset + 1],
        report[offset + 2],
        report[offset + 3],
    ])
}

fn u64_at(report: &[u8], offset: usize) -> u64 {
    u64::from(u32_at(report, offset)) | (u64::from(u32_at(report, offset + 4)) << 32)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
}

/// Describe the fields of a report that the protocol rules depend on.
pub fn describe_sev_snp_report(report: &[u8]) -> EnclaveResult<Vec<String>> {
    if report.len() != SEV_SNP_REPORT_LEN {
        return Err(TsmError::MalformedReport {
            platform: "SEV-SNP",
            detail: format!("length is {}, expected {SEV_SNP_REPORT_LEN}", report.len()),
        }
        .into());
    }
    let version = u32_at(report, 0x00);
    let policy = u64_at(report, 0x08);
    let key_info = u32_at(report, 0x48);
    let mut lines = vec![
        format!(
            "version={version} vmpl={} signature_algorithm={}",
            u32_at(report, 0x30),
            u32_at(report, 0x34)
        ),
        format!(
            "guest policy=0x{policy:016x} debug_allowed={} migration_agent_allowed={} smt_allowed={}",
            policy >> 19 & 1 == 1,
            policy >> 18 & 1 == 1,
            policy >> 16 & 1 == 1
        ),
        format!(
            "signing_key={} (0 is VCEK, 1 is VLEK) mask_chip_key={}",
            key_info >> 2 & 0b111,
            key_info >> 1 & 1 == 1
        ),
        format!(
            "tcb current=0x{:016x} reported=0x{:016x} committed=0x{:016x} launch=0x{:016x}",
            u64_at(report, 0x38),
            u64_at(report, 0x180),
            u64_at(report, 0x1E0),
            u64_at(report, 0x1F0)
        ),
        format!("chip_id_sha256={}", sha256_hex(&report[0x1A0..0x1E0])),
    ];
    if version >= 3 {
        // The processor generation selects the AMD root certificate.
        lines.push(format!(
            "cpuid family=0x{:02x} model=0x{:02x} stepping=0x{:02x}",
            report[0x188], report[0x189], report[0x18A]
        ));
    }
    Ok(lines)
}

/// Describe the certificates of a certificate table.
pub fn describe_sev_snp_certificates(table: &[u8]) -> Vec<String> {
    if table.is_empty() {
        return vec![
            "certificate table: EMPTY (the host supplies no certificates; the VCEK must come \
             from the AMD key distribution service)"
                .to_owned(),
        ];
    }
    match sev_snp_certificate_table(table) {
        Ok(certificates) => certificates
            .iter()
            .map(|certificate| {
                format!(
                    "certificate {} len={} sha256={}",
                    certificate.kind.name(),
                    certificate.bytes.len(),
                    sha256_hex(certificate.bytes)
                )
            })
            .collect(),
        Err(error) => vec![format!("certificate table: MALFORMED ({error})")],
    }
}

/// Ask the processor for a key derived from the launch measurement of this VM.
///
/// The probe requests the key two times and prints a fingerprint, never the
/// key. Equal fingerprints across two boots of the same image show that the key
/// can seal the state of the service.
#[cfg(feature = "sev-snp")]
pub fn describe_sev_snp_derived_key() -> String {
    use sev::firmware::guest::{DerivedKey, Firmware, GuestFieldSelect};
    use zeroize::Zeroize as _;

    let request = || {
        let mut fields = GuestFieldSelect::default();
        fields.set_guest_policy(true);
        fields.set_measurement(true);
        // Root key 0 is the chip key. The key does not depend on the firmware
        // version, so a firmware update keeps the sealed state readable.
        DerivedKey::new(false, fields, 0, 0, 0, None)
    };
    let mut firmware = match Firmware::open() {
        Ok(firmware) => firmware,
        Err(error) => return format!("derived key: UNAVAILABLE (open /dev/sev-guest: {error})"),
    };
    let mut first = match firmware.get_derived_key(None, request()) {
        Ok(key) => key,
        Err(error) => return format!("derived key: UNAVAILABLE ({error})"),
    };
    let second = firmware.get_derived_key(None, request());
    let line = match second {
        Ok(mut second) => {
            let line = format!(
                "derived key: AVAILABLE stable_within_boot={} fingerprint={}",
                first == second,
                &sha256_hex(&first)[..16]
            );
            second.zeroize();
            line
        }
        Err(error) => format!("derived key: UNSTABLE (second request failed: {error})"),
    };
    first.zeroize();
    line
}

#[cfg(not(feature = "sev-snp"))]
pub fn describe_sev_snp_derived_key() -> String {
    "derived key: NOT PROBED (binary built without the sev-snp feature)".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::EnclaveError;

    const SIMULATOR_REPORT: &[u8] =
        include_bytes!("../../../crates/system/tee/tests/fixtures/sev_snp/simulator-report.bin");
    const SIMULATOR_CERT_TABLE: &[u8] = include_bytes!(
        "../../../crates/system/tee/tests/fixtures/sev_snp/simulator-cert-table.bin"
    );
    const MILAN_REPORT: &[u8] =
        include_bytes!("../../../crates/system/tee/tests/fixtures/sev_snp/milan-report.bin");

    fn table(entries: &[([u8; 16], &[u8])]) -> Vec<u8> {
        let header_len = (entries.len() + 1) * CERT_TABLE_ENTRY_LEN;
        let mut header = Vec::new();
        let mut body = Vec::new();
        for (guid, bytes) in entries {
            header.extend_from_slice(guid);
            header.extend_from_slice(&((header_len + body.len()) as u32).to_le_bytes());
            header.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            body.extend_from_slice(bytes);
        }
        header.extend_from_slice(&[0; CERT_TABLE_ENTRY_LEN]);
        header.extend_from_slice(&body);
        header
    }

    #[test]
    fn the_simulator_table_holds_the_ask_and_the_vcek() {
        let certificates = sev_snp_certificate_table(SIMULATOR_CERT_TABLE).unwrap();

        let kinds: Vec<_> = certificates.iter().map(|entry| entry.kind).collect();
        assert_eq!(
            kinds,
            [SevSnpCertificateKind::Ask, SevSnpCertificateKind::Vcek]
        );
        // Each entry is one DER certificate.
        for certificate in certificates {
            assert_eq!(certificate.bytes[0], 0x30);
        }
    }

    #[test]
    fn a_table_lists_every_known_kind_and_keeps_unknown_entries() {
        let unknown = [0x7f; 16];
        let bytes = table(&[
            (ARK_GUID, b"ark"),
            (ASK_GUID, b"ask"),
            (VCEK_GUID, b"vcek"),
            (VLEK_GUID, b"vlek"),
            (unknown, b"other"),
        ]);

        let certificates = sev_snp_certificate_table(&bytes).unwrap();

        assert_eq!(
            certificates,
            [
                SevSnpCertificate {
                    kind: SevSnpCertificateKind::Ark,
                    bytes: b"ark"
                },
                SevSnpCertificate {
                    kind: SevSnpCertificateKind::Ask,
                    bytes: b"ask"
                },
                SevSnpCertificate {
                    kind: SevSnpCertificateKind::Vcek,
                    bytes: b"vcek"
                },
                SevSnpCertificate {
                    kind: SevSnpCertificateKind::Vlek,
                    bytes: b"vlek"
                },
                SevSnpCertificate {
                    kind: SevSnpCertificateKind::Other(unknown),
                    bytes: b"other"
                },
            ]
        );
    }

    #[test]
    fn an_empty_table_has_no_certificates() {
        assert!(sev_snp_certificate_table(&[]).unwrap().is_empty());
        assert!(sev_snp_certificate_table(&[0; CERT_TABLE_ENTRY_LEN])
            .unwrap()
            .is_empty());
        assert!(describe_sev_snp_certificates(&[])[0].contains("EMPTY"));
    }

    #[test]
    fn a_table_with_a_bad_range_or_without_a_terminator_is_rejected() {
        let mut outside = table(&[(ASK_GUID, b"ask")]);
        outside[20..24].copy_from_slice(&1_000_u32.to_le_bytes());
        let mut overflow = table(&[(ASK_GUID, b"ask")]);
        overflow[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        overflow[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut unterminated = Vec::new();
        unterminated.extend_from_slice(&ASK_GUID);
        unterminated.extend_from_slice(&0_u32.to_le_bytes());
        unterminated.extend_from_slice(&4_u32.to_le_bytes());

        for bytes in [outside, overflow, unterminated] {
            assert!(matches!(
                sev_snp_certificate_table(&bytes),
                Err(EnclaveError::Tsm(TsmError::MalformedReport { .. }))
            ));
        }
    }

    #[test]
    fn report_description_names_the_fields_that_the_rules_depend_on() {
        let lines = describe_sev_snp_report(MILAN_REPORT).unwrap().join("\n");

        assert!(lines.contains("version=2 vmpl=0 signature_algorithm=1"));
        assert!(lines.contains("debug_allowed=false migration_agent_allowed=false"));
        assert!(lines.contains("signing_key=0"));
        assert!(lines.contains("current=0x7308000000000003"));
        // Version 2 has no CPUID fields.
        assert!(!lines.contains("cpuid"));
    }

    #[test]
    fn report_description_reads_the_simulator_report_and_rejects_a_short_one() {
        assert!(describe_sev_snp_report(SIMULATOR_REPORT).is_ok());
        assert!(describe_sev_snp_report(&SIMULATOR_REPORT[..100]).is_err());
    }
}
