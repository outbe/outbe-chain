//! One interface to the attestation evidence of the platform that runs this
//! service.
//!
//! The service runs either as an SGX enclave under Gramine or as a process in a
//! confidential VM. The two environments supply evidence through different
//! interfaces:
//!
//! | Platform | Interface | Evidence |
//! |---|---|---|
//! | Intel SGX | Gramine `/dev/attestation` | DCAP quote |
//! | AMD SEV-SNP | kernel `configfs-tsm` | attestation report and certificate table |
//! | Intel TDX | kernel `configfs-tsm` | TDX quote |
//!
//! [`AttestationSource`] hides that difference. A caller detects the source once
//! and then requests evidence for 64 bytes of report data.

use crate::errors::{EnclaveResult, TsmError};
use crate::gramine::{self, AttestationType};
use crate::tsm::{self, TsmProvider, TsmReportSource};

/// Platform that produced a piece of attestation evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestationPlatform {
    /// Intel SGX enclave under Gramine, DCAP attestation.
    SgxDcap,
    /// AMD SEV-SNP confidential VM.
    SevSnp,
    /// Intel TDX confidential VM.
    Tdx,
}

impl AttestationPlatform {
    /// Stable short name for logs and operator output.
    pub fn name(self) -> &'static str {
        match self {
            Self::SgxDcap => "sgx-dcap",
            Self::SevSnp => "sev-snp",
            Self::Tdx => "tdx",
        }
    }
}

/// Evidence that binds caller-supplied report data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformEvidence {
    pub platform: AttestationPlatform,
    /// DCAP quote, SEV-SNP attestation report or TDX quote.
    pub evidence: Vec<u8>,
    /// Certificate table that accompanies an SEV-SNP report. Empty otherwise.
    pub auxiliary: Vec<u8>,
}

/// Where this process obtains its attestation evidence.
#[derive(Clone, Debug)]
pub enum AttestationSource {
    /// Gramine `/dev/attestation` in an SGX enclave with DCAP attestation.
    GramineSgx,
    /// Kernel TSM report interface in a confidential VM.
    Tsm {
        platform: AttestationPlatform,
        source: TsmReportSource,
    },
}

impl AttestationSource {
    /// Detect the source of the running process.
    ///
    /// Returns `None` when the process has no hardware attestation: it runs
    /// outside a TEE, or under Gramine without DCAP attestation.
    pub fn detect() -> EnclaveResult<Option<Self>> {
        Self::detect_from(&gramine::attestation_type(), TsmReportSource::system())
    }

    /// Detection over explicit inputs.
    pub fn detect_from(
        gramine_attestation: &AttestationType,
        tsm: TsmReportSource,
    ) -> EnclaveResult<Option<Self>> {
        if *gramine_attestation == AttestationType::Dcap {
            return Ok(Some(Self::GramineSgx));
        }
        if !tsm.is_available() {
            return Ok(None);
        }
        let platform = match tsm.provider()? {
            TsmProvider::SevGuest => AttestationPlatform::SevSnp,
            TsmProvider::TdxGuest => AttestationPlatform::Tdx,
            TsmProvider::Other(name) => return Err(TsmError::UnsupportedProvider(name).into()),
        };
        Ok(Some(Self::Tsm {
            platform,
            source: tsm,
        }))
    }

    /// Platform of this source.
    pub fn platform(&self) -> AttestationPlatform {
        match self {
            Self::GramineSgx => AttestationPlatform::SgxDcap,
            Self::Tsm { platform, .. } => *platform,
        }
    }

    /// Request evidence that binds `report_data`.
    pub fn evidence(&self, report_data: &[u8; 64]) -> EnclaveResult<PlatformEvidence> {
        match self {
            Self::GramineSgx => Ok(PlatformEvidence {
                platform: AttestationPlatform::SgxDcap,
                evidence: gramine::dcap_quote(report_data)?,
                auxiliary: Vec::new(),
            }),
            Self::Tsm { platform, source } => {
                let report = source.report(report_data)?;
                Ok(PlatformEvidence {
                    platform: *platform,
                    evidence: report.report,
                    auxiliary: report.auxiliary,
                })
            }
        }
    }
}

/// Evidence for a registration intent: the document that a node submits to
/// the registry together with the intent.
///
/// An SGX enclave returns a DCAP quote. A service in an SEV-SNP confidential VM
/// returns an attestation report.
pub fn registration_evidence(report_data: &[u8; 64]) -> EnclaveResult<Vec<u8>> {
    #[cfg(feature = "sev-snp")]
    if crate::confidential_vm::configured() {
        return crate::confidential_vm::registration_report(report_data);
    }
    gramine::dcap_quote(report_data)
}

/// Print what the kernel TSM report interface of this machine provides.
///
/// For an SEV-SNP guest this prints the launch measurement and the host data,
/// the two values an operator pins in the attestation policy.
pub fn probe_tsm_to_stderr() {
    eprintln!("=== kernel TSM report probe ({}) ===", tsm::TSM_REPORT_DIR);
    let source = TsmReportSource::system();
    if !source.is_available() {
        eprintln!("tsm: UNAVAILABLE (interface directory is absent)");
        return;
    }
    match source.provider() {
        Ok(provider) => eprintln!("tsm provider: {}", provider.name()),
        Err(error) => eprintln!("tsm provider: UNAVAILABLE ({error})"),
    }
    let detected = AttestationSource::detect_from(&AttestationType::Unavailable, source);
    let Ok(Some(source)) = detected else {
        if let Err(error) = detected {
            eprintln!("tsm source: UNSUPPORTED ({error})");
        }
        return;
    };
    match source.evidence(&[0_u8; 64]) {
        Ok(evidence) => {
            eprintln!(
                "tsm evidence: platform={} evidence_len={} auxiliary_len={}",
                evidence.platform.name(),
                evidence.evidence.len(),
                evidence.auxiliary.len()
            );
            if evidence.platform == AttestationPlatform::SevSnp {
                match tsm::sev_snp_identity(&evidence.evidence) {
                    Ok(identity) => eprintln!(
                        "sev-snp identity: measurement={} host_data={}",
                        hex::encode(identity.measurement),
                        hex::encode(identity.host_data)
                    ),
                    Err(error) => eprintln!("sev-snp identity: UNAVAILABLE ({error})"),
                }
                match crate::sev_snp_probe::describe_sev_snp_report(&evidence.evidence) {
                    Ok(lines) => {
                        for line in lines {
                            eprintln!("sev-snp report: {line}");
                        }
                    }
                    Err(error) => eprintln!("sev-snp report: UNAVAILABLE ({error})"),
                }
                for line in crate::sev_snp_probe::describe_sev_snp_certificates(&evidence.auxiliary)
                {
                    eprintln!("sev-snp {line}");
                }
            }
            if evidence.platform == AttestationPlatform::Tdx {
                for line in describe_tdx_quote(&evidence.evidence) {
                    eprintln!("tdx {line}");
                }
            }
        }
        Err(error) => eprintln!("tsm evidence: UNAVAILABLE ({error})"),
    }
}

/// Lines that describe a TDX quote: the values that the attestation policy
/// pins and the facts that the verifier checks.
fn describe_tdx_quote(quote: &[u8]) -> Vec<String> {
    use outbe_tee::tdx_report::{
        parse_tdx_quote_measurements, tdx_quote_canonical_len, tdx_quote_pck_certification_data,
    };

    let mut lines = Vec::new();
    let version = quote.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let tee_type = quote
        .get(4..8)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    lines.push(format!(
        "quote: version={version:?} tee_type={tee_type:#x?} (the verifier accepts version 4 and TEE type 0x81)"
    ));
    match parse_tdx_quote_measurements(quote) {
        Ok(measurements) => {
            lines.push(format!("identity: mrtd={}", hex::encode(measurements.mrtd)));
            for (index, rtmr) in measurements.rtmrs.iter().enumerate() {
                lines.push(format!("identity: rtmr{index}={}", hex::encode(rtmr)));
            }
            lines.push(format!(
                "identity: mrconfigid={}",
                hex::encode(measurements.mr_config_id)
            ));
            lines.push(format!(
                "report: td_attributes={} debug={} (debug must be false)",
                hex::encode(measurements.td_attributes),
                measurements.is_debug()
            ));
        }
        Err(_) => lines.push("identity: UNAVAILABLE (the quote has no TD report body)".to_owned()),
    }
    match tdx_quote_canonical_len(quote) {
        Ok(len) => {
            lines.push(format!(
                "quote: canonical_len={len} bytes_after_signature_data={}",
                quote.len() - len
            ));
            match tdx_quote_pck_certification_data(&quote[..len]) {
                Ok((kind, data)) => lines.push(format!(
                    "quote: pck certification data type={kind} len={} (type 5 is a PCK certificate chain)",
                    data.len()
                )),
                Err(_) => lines.push("quote: pck certification data UNAVAILABLE".to_owned()),
            }
        }
        Err(_) => lines.push("quote: signature data does not end the quote".to_owned()),
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::EnclaveError;
    use std::fs;

    #[test]
    fn tdx_quote_description_prints_the_policy_values() {
        let quote = include_bytes!(
            "../../../crates/system/tee/tests/fixtures/intel-tdx-quote-v4/tdx-quote-v4.bin"
        );
        let lines = describe_tdx_quote(quote);
        assert!(lines[0].contains("version=Some(4)"));
        assert!(lines.iter().any(|line| line.starts_with(
            "identity: mrtd=91eb2b44d141d4ece09f0c75c2c53d247a3c68edd7fafe8a3520c942a604a407"
        )));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("identity: rtmr"))
                .count(),
            3
        );
        assert!(lines.iter().any(|line| line.contains("debug=false")));
        assert!(lines
            .iter()
            .any(|line| line.contains("canonical_len=4936 bytes_after_signature_data=70")));
        assert!(lines
            .iter()
            .any(|line| line.contains("pck certification data type=5")));

        assert!(describe_tdx_quote(&quote[..100])
            .iter()
            .any(|line| line.contains("UNAVAILABLE")));
    }

    fn interface_with_provider(provider: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("provider"), provider).unwrap();
        root
    }

    fn detect(
        gramine_attestation: AttestationType,
        root: &std::path::Path,
    ) -> EnclaveResult<Option<AttestationSource>> {
        AttestationSource::detect_from(&gramine_attestation, TsmReportSource::at(root))
    }

    #[test]
    fn gramine_dcap_wins_over_the_tsm_interface() {
        let root = interface_with_provider("sev_guest");

        let source = detect(AttestationType::Dcap, root.path()).unwrap().unwrap();

        assert_eq!(source.platform(), AttestationPlatform::SgxDcap);
    }

    #[test]
    fn sev_guest_provider_selects_sev_snp() {
        let root = interface_with_provider("sev_guest\n");

        let source = detect(AttestationType::Unavailable, root.path())
            .unwrap()
            .unwrap();

        assert_eq!(source.platform(), AttestationPlatform::SevSnp);
        assert_eq!(source.platform().name(), "sev-snp");
    }

    #[test]
    fn tdx_guest_provider_selects_tdx() {
        let root = interface_with_provider("tdx_guest\n");

        let source = detect(AttestationType::Unavailable, root.path())
            .unwrap()
            .unwrap();

        assert_eq!(source.platform(), AttestationPlatform::Tdx);
    }

    #[test]
    fn no_source_without_dcap_and_without_the_tsm_interface() {
        let root = tempfile::tempdir().unwrap();
        let absent = root.path().join("absent");

        for gramine_attestation in [
            AttestationType::Unavailable,
            AttestationType::None,
            AttestationType::SgxNoAttest,
        ] {
            assert!(detect(gramine_attestation, &absent).unwrap().is_none());
        }
    }

    #[test]
    fn unknown_tsm_provider_is_an_error() {
        let root = interface_with_provider("arm_cca_guest");

        let result = detect(AttestationType::Unavailable, root.path());

        assert!(matches!(
            result,
            Err(EnclaveError::Tsm(TsmError::UnsupportedProvider(name))) if name == "arm_cca_guest"
        ));
    }

    #[test]
    fn tsm_evidence_carries_the_platform_report_and_certificates() {
        let root = interface_with_provider("sev_guest");
        let entry = root.path().join("request");
        fs::create_dir(&entry).unwrap();
        fs::write(entry.join("outblob"), b"snp-report").unwrap();
        fs::write(entry.join("auxblob"), b"certificates").unwrap();
        let source = AttestationSource::detect_from(
            &AttestationType::Unavailable,
            TsmReportSource::with_prepared_entry(root.path(), "request"),
        )
        .unwrap()
        .unwrap();

        let evidence = source.evidence(&[7_u8; 64]).unwrap();

        assert_eq!(evidence.platform, AttestationPlatform::SevSnp);
        assert_eq!(evidence.evidence, b"snp-report");
        assert_eq!(evidence.auxiliary, b"certificates");
        assert_eq!(fs::read(entry.join("inblob")).unwrap(), [7_u8; 64]);
    }
}
