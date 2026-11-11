//! Verification of an AMD SEV-SNP attestation report.
//!
//! The verifier is a pure function of its inputs. It reads no clock, no file
//! and no network, so every validator computes the same result for the same
//! evidence.
//!
//! Checks, in order:
//!
//! 1. the root certificate (ARK) is the one that the attestation policy pins by
//!    SHA-256;
//! 2. the certificate chain ARK -> ASK -> VCEK is valid (RSA-PSS with SHA-384,
//!    the algorithm of the AMD key hierarchy);
//! 3. the VCEK signed the report (ECDSA P-384 with SHA-384);
//! 4. the report fields satisfy the fixed guest rules: no debug, no migration
//!    agent, privilege level 0, chip key as the signing key;
//! 5. the current, reported, committed and launch firmware versions are equal.
//!
//! The caller compares the returned identity and report data with the policy
//! and with the registration intent.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, SevSnpEvidenceV1, TeePolicyV1,
};
use sev::{
    certs::snp::{ca, Certificate, Chain, Verifiable},
    firmware::guest::AttestationReport,
    parser::ByteParser,
};
use sha2::{Digest as _, Sha256};

use crate::dcap_protocol::{DcapPckCaV1, DcapPlatformTcbStatusV1, DcapRejectCodeV1, DcapVerdictV1};

/// Length of an SEV-SNP attestation report.
pub const SEV_SNP_REPORT_LEN: usize = outbe_primitives::tee_attestation_v1::SEV_SNP_REPORT_BYTES;

/// Largest accepted DER certificate. AMD certificates are below 2 KiB.
pub const SEV_SNP_MAX_CERTIFICATE_LEN: usize =
    outbe_primitives::tee_attestation_v1::MAX_SEV_SNP_CERTIFICATE_BYTES;

pub use outbe_primitives::tee_attestation_v1::{
    sev_snp_measurement_rule_value, SEV_SNP_RULE_PRODUCT_ID_V1 as SEV_SNP_RULE_PRODUCT_ID,
    SEV_SNP_RULE_SVN_V1 as SEV_SNP_RULE_SVN,
};

/// Signature algorithm identifier of ECDSA P-384 with SHA-384 in a report.
const SIGNATURE_ALGORITHM_ECDSA_P384_SHA384: u32 = 1;

/// Signing key selector of the chip endorsement key (VCEK) in a report.
const SIGNING_KEY_VCEK: u32 = 0;

/// Reason why SEV-SNP evidence is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SevSnpRejectV1 {
    #[error("the report length is not 1184 bytes")]
    ReportLength,
    #[error("the report is malformed")]
    ReportMalformed,
    #[error("the report version is not supported")]
    ReportVersion,
    #[error("the report signature algorithm is not ECDSA P-384 with SHA-384")]
    ReportSignatureAlgorithm,
    #[error("a certificate is empty or too large")]
    CertificateLength,
    #[error("a certificate is not valid DER")]
    CertificateMalformed,
    #[error("the root certificate is not the one pinned by the policy")]
    RootNotPinned,
    #[error("the certificate chain is not valid")]
    CertificateChain,
    #[error("the report signature is not valid")]
    ReportSignature,
    #[error("the report was not requested at privilege level 0")]
    PrivilegeLevel,
    #[error("the guest policy allows debug")]
    DebugAllowed,
    #[error("the guest policy allows a migration agent")]
    MigrationAgentAllowed,
    #[error("the report is not signed with the chip endorsement key")]
    SigningKey,
    #[error("the firmware versions in the report differ from each other")]
    TcbVersionsDiffer,
}

impl SevSnpRejectV1 {
    /// Reject code of the shared verification protocol for this reason.
    ///
    /// SEV-SNP reuses the existing codes. A new code would change the protocol
    /// between the node and the enclave service.
    pub const fn protocol_code(self) -> DcapRejectCodeV1 {
        match self {
            Self::ReportLength
            | Self::ReportMalformed
            | Self::ReportVersion
            | Self::ReportSignatureAlgorithm => DcapRejectCodeV1::QuoteMalformed,
            Self::CertificateLength | Self::CertificateMalformed => {
                DcapRejectCodeV1::CollateralNonCanonical
            }
            Self::RootNotPinned => DcapRejectCodeV1::IntelRootMismatch,
            Self::CertificateChain | Self::ReportSignature => {
                DcapRejectCodeV1::NativeVerificationFailed
            }
            Self::PrivilegeLevel | Self::MigrationAgentAllowed | Self::SigningKey => {
                DcapRejectCodeV1::QuoteProfileMismatch
            }
            Self::DebugAllowed => DcapRejectCodeV1::DebugEnclaveRejected,
            Self::TcbVersionsDiffer => DcapRejectCodeV1::PlatformTcbRejected,
        }
    }
}

/// Verify canonical SEV-SNP evidence against the active policy.
///
/// The verdict has the shape of a DCAP verdict. The launch measurement and the
/// host data fill the measurement fields. The fields that describe Intel
/// collateral hold fixed values: an AMD report has no collateral that expires.
pub fn verify_sev_snp_evidence(
    evidence: &SevSnpEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
) -> Result<DcapVerdictV1, DcapRejectCodeV1> {
    AttestationEvidenceV1::SevSnp(evidence.clone())
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    let policy_hash = policy
        .policy_hash()
        .map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    if policy.attestation_mode != AttestationMode::SevSnpRequired
        || evidence.intent.chain_id != policy.chain_id
        || evidence.intent.genesis_hash != policy.genesis_hash
        || evidence.intent.policy_hash != policy_hash
    {
        return Err(DcapRejectCodeV1::PolicyBindingMismatch);
    }
    if block_timestamp > i64::MAX as u64 {
        return Err(DcapRejectCodeV1::TimestampInvalid);
    }

    let verified = verify_sev_snp_report_v1(
        &evidence.report,
        SevSnpCertificatesV1 {
            ark_der: &evidence.ark_der,
            ask_der: &evidence.ask_der,
            vcek_der: &evidence.vcek_der,
        },
        &policy.intel_root_der_hash,
    )
    .map_err(SevSnpRejectV1::protocol_code)?;

    let expected_report_data = evidence
        .intent
        .report_data()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    verdict_for_verified_report(&verified, &expected_report_data, policy)
}

/// Bind a verified report to the registration intent and to a policy rule.
fn verdict_for_verified_report(
    verified: &SevSnpVerifiedReportV1,
    expected_report_data: &[u8; 64],
    policy: &TeePolicyV1,
) -> Result<DcapVerdictV1, DcapRejectCodeV1> {
    if verified.report_data != *expected_report_data {
        return Err(DcapRejectCodeV1::ReportDataMismatch);
    }
    let mrenclave = verified.measurement_rule_value();
    let mrsigner = verified.host_data_rule_value();
    let measurement_accepted = policy.measurement_rules.iter().any(|rule| {
        rule.mrenclave == mrenclave
            && rule.mrsigner == mrsigner
            && rule.isv_prod_id == SEV_SNP_RULE_PRODUCT_ID
            && SEV_SNP_RULE_SVN >= rule.minimum_isv_svn
    });
    if !measurement_accepted {
        return Err(DcapRejectCodeV1::MeasurementRejected);
    }

    Ok(DcapVerdictV1 {
        mrenclave,
        mrsigner,
        isv_prod_id: SEV_SNP_RULE_PRODUCT_ID,
        isv_svn: SEV_SNP_RULE_SVN,
        pck_ca: DcapPckCaV1::Processor,
        fmspc: [0; 6],
        pce_id: 0,
        platform_tcb_status: DcapPlatformTcbStatusV1::UpToDate,
        advisory_ids: Vec::new(),
        tcb_evaluation_data_number: 0,
        qe_tcb_evaluation_data_number: 0,
        collateral_valid_until: u64::MAX,
    })
}

/// DER certificates of the AMD key hierarchy for one report.
#[derive(Clone, Copy, Debug)]
pub struct SevSnpCertificatesV1<'a> {
    /// AMD root key certificate.
    pub ark_der: &'a [u8],
    /// AMD SEV signing key certificate.
    pub ask_der: &'a [u8],
    /// Versioned chip endorsement key certificate.
    pub vcek_der: &'a [u8],
}

/// Fields of a report whose signature and certificate chain are valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SevSnpVerifiedReportV1 {
    /// Launch measurement of the VM.
    pub measurement: [u8; 48],
    /// Host data that the VMM bound at launch.
    pub host_data: [u8; 32],
    /// Report data that the guest supplied.
    pub report_data: [u8; 64],
}

impl SevSnpVerifiedReportV1 {
    /// Launch measurement as a 32-byte policy rule value.
    pub fn measurement_rule_value(&self) -> B256 {
        sev_snp_measurement_rule_value(&self.measurement)
    }

    /// Host data as a 32-byte policy rule value.
    pub fn host_data_rule_value(&self) -> B256 {
        B256::from(self.host_data)
    }
}

/// Verify an SEV-SNP attestation report against a pinned AMD root.
pub fn verify_sev_snp_report_v1(
    report: &[u8],
    certificates: SevSnpCertificatesV1<'_>,
    pinned_ark_sha256: &B256,
) -> Result<SevSnpVerifiedReportV1, SevSnpRejectV1> {
    if report.len() != SEV_SNP_REPORT_LEN {
        return Err(SevSnpRejectV1::ReportLength);
    }
    for certificate in [
        certificates.ark_der,
        certificates.ask_der,
        certificates.vcek_der,
    ] {
        if certificate.is_empty() || certificate.len() > SEV_SNP_MAX_CERTIFICATE_LEN {
            return Err(SevSnpRejectV1::CertificateLength);
        }
    }
    if Sha256::digest(certificates.ark_der).as_slice() != pinned_ark_sha256.as_slice() {
        return Err(SevSnpRejectV1::RootNotPinned);
    }

    let version = u32::from_le_bytes([report[0], report[1], report[2], report[3]]);
    // Version 5 adds the mitigation vectors (firmware with SEV-SNP API 1.58).
    // The fields that this verifier reads keep their offsets.
    if !matches!(version, 2 | 3 | 5) {
        return Err(SevSnpRejectV1::ReportVersion);
    }
    let parsed = AttestationReport::from_bytes_with(report, ())
        .map_err(|_| SevSnpRejectV1::ReportMalformed)?;
    if parsed.sig_algo != SIGNATURE_ALGORITHM_ECDSA_P384_SHA384 {
        return Err(SevSnpRejectV1::ReportSignatureAlgorithm);
    }

    let chain = Chain {
        ca: ca::Chain {
            ark: parse_certificate(certificates.ark_der)?,
            ask: parse_certificate(certificates.ask_der)?,
        },
        vek: parse_certificate(certificates.vcek_der)?,
    };
    let vcek = chain
        .verify()
        .map_err(|_| SevSnpRejectV1::CertificateChain)?;
    (vcek, &parsed)
        .verify()
        .map_err(|_| SevSnpRejectV1::ReportSignature)?;

    if parsed.vmpl != 0 {
        return Err(SevSnpRejectV1::PrivilegeLevel);
    }
    if parsed.policy.debug_allowed() {
        return Err(SevSnpRejectV1::DebugAllowed);
    }
    if parsed.policy.migrate_ma_allowed() {
        return Err(SevSnpRejectV1::MigrationAgentAllowed);
    }
    if parsed.key_info.mask_chip_key() || parsed.key_info.signing_key() != SIGNING_KEY_VCEK {
        return Err(SevSnpRejectV1::SigningKey);
    }

    // A platform whose current firmware differs from the committed or the
    // launch firmware is in the middle of an update or can roll back.
    if parsed.current_tcb != parsed.reported_tcb
        || parsed.current_tcb != parsed.committed_tcb
        || parsed.current_tcb != parsed.launch_tcb
    {
        return Err(SevSnpRejectV1::TcbVersionsDiffer);
    }

    // The identity comes from the signed bytes, not from the decoded structure.
    Ok(SevSnpVerifiedReportV1 {
        measurement: field(report, 0x90),
        host_data: field(report, 0xC0),
        report_data: field(report, 0x50),
    })
}

fn parse_certificate(der: &[u8]) -> Result<Certificate, SevSnpRejectV1> {
    Certificate::from_der(der).map_err(|_| SevSnpRejectV1::CertificateMalformed)
}

fn field<const N: usize>(report: &[u8], offset: usize) -> [u8; N] {
    let mut value = [0_u8; N];
    value.copy_from_slice(&report[offset..offset + N]);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;

    const MILAN_REPORT: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-report.bin");
    const MILAN_ARK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-ark.der");
    const MILAN_ASK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-ask.der");
    const MILAN_VCEK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/milan-vcek.der");
    const SIMULATOR_REPORT: &[u8] =
        include_bytes!("../tests/fixtures/sev_snp/simulator-report.bin");
    const SIMULATOR_ARK: &[u8] = include_bytes!("../tests/fixtures/sev_snp/simulator-ark.der");
    const SIMULATOR_CERT_TABLE: &[u8] =
        include_bytes!("../tests/fixtures/sev_snp/simulator-cert-table.bin");

    const ASK_GUID: [u8; 16] = [
        0x4a, 0xb7, 0xb3, 0x79, 0xbb, 0xac, 0x4f, 0xe4, 0xa0, 0x2f, 0x05, 0xae, 0xf3, 0x27, 0xc7,
        0x82,
    ];
    const VCEK_GUID: [u8; 16] = [
        0x63, 0xda, 0x75, 0x8d, 0xe6, 0x64, 0x45, 0x64, 0xad, 0xc5, 0xf4, 0xb9, 0x3b, 0xe8, 0xac,
        0xcd,
    ];

    const MILAN: SevSnpCertificatesV1<'static> = SevSnpCertificatesV1 {
        ark_der: MILAN_ARK,
        ask_der: MILAN_ASK,
        vcek_der: MILAN_VCEK,
    };

    fn sha256(bytes: &[u8]) -> B256 {
        B256::from_slice(Sha256::digest(bytes).as_slice())
    }

    fn verify_milan(report: &[u8]) -> Result<SevSnpVerifiedReportV1, SevSnpRejectV1> {
        verify_sev_snp_report_v1(report, MILAN, &sha256(MILAN_ARK))
    }

    /// Certificate with the given GUID from a kernel SEV-SNP certificate table.
    fn certificate_table_entry(table: &[u8], guid: [u8; 16]) -> &[u8] {
        for entry in table.chunks_exact(24) {
            if entry[..16] == [0_u8; 16] {
                break;
            }
            if entry[..16] == guid {
                let offset = u32::from_le_bytes(entry[16..20].try_into().unwrap()) as usize;
                let length = u32::from_le_bytes(entry[20..24].try_into().unwrap()) as usize;
                return &table[offset..offset + length];
            }
        }
        panic!("certificate table has no entry with the requested GUID");
    }

    #[test]
    fn accepts_a_report_signed_by_a_real_milan_processor() {
        let verified = verify_milan(MILAN_REPORT).unwrap();

        assert_eq!(verified.measurement[..], MILAN_REPORT[0x90..0xC0]);
        assert_eq!(verified.host_data[..], MILAN_REPORT[0xC0..0xE0]);
        assert_eq!(verified.report_data[..], MILAN_REPORT[0x50..0x90]);
        assert_eq!(
            verified.host_data_rule_value(),
            B256::from(verified.host_data)
        );
    }

    #[test]
    fn measurement_rule_value_is_the_domain_separated_keccak() {
        let measurement = [0x33_u8; 48];
        let mut preimage = b"outbe/tee/sev-snp/measurement/v1".to_vec();
        preimage.extend_from_slice(&measurement);

        assert_eq!(
            sev_snp_measurement_rule_value(&measurement),
            keccak256(preimage)
        );
    }

    #[test]
    fn rejects_a_root_that_the_policy_does_not_pin() {
        let result = verify_sev_snp_report_v1(MILAN_REPORT, MILAN, &B256::repeat_byte(0x11));

        assert_eq!(result, Err(SevSnpRejectV1::RootNotPinned));
    }

    #[test]
    fn rejects_a_report_with_a_wrong_length() {
        assert_eq!(
            verify_milan(&MILAN_REPORT[..SEV_SNP_REPORT_LEN - 1]),
            Err(SevSnpRejectV1::ReportLength)
        );
        assert_eq!(verify_milan(&[]), Err(SevSnpRejectV1::ReportLength));
    }

    #[test]
    fn rejects_an_unsupported_report_version() {
        for version in [0_u8, 1, 4] {
            let mut report = MILAN_REPORT.to_vec();
            report[0] = version;

            assert_eq!(verify_milan(&report), Err(SevSnpRejectV1::ReportVersion));
        }
    }

    #[test]
    fn rejects_a_change_of_any_identity_field() {
        for offset in [0x50_usize, 0x90, 0xC0] {
            let mut report = MILAN_REPORT.to_vec();
            report[offset] ^= 0x01;

            assert_eq!(verify_milan(&report), Err(SevSnpRejectV1::ReportSignature));
        }
    }

    #[test]
    fn rejects_a_changed_signature() {
        let mut report = MILAN_REPORT.to_vec();
        report[0x2A0] ^= 0x01;

        assert_eq!(verify_milan(&report), Err(SevSnpRejectV1::ReportSignature));
    }

    #[test]
    fn rejects_a_report_signed_by_another_key() {
        // The simulator report is well formed but its signer is not the Milan
        // VCEK.
        assert_eq!(
            verify_milan(SIMULATOR_REPORT),
            Err(SevSnpRejectV1::ReportSignature)
        );
    }

    #[test]
    fn rejects_a_chain_with_a_swapped_intermediate() {
        let certificates = SevSnpCertificatesV1 {
            ask_der: MILAN_ARK,
            ..MILAN
        };

        let result = verify_sev_snp_report_v1(MILAN_REPORT, certificates, &sha256(MILAN_ARK));

        assert_eq!(result, Err(SevSnpRejectV1::CertificateChain));
    }

    #[test]
    fn rejects_malformed_and_oversized_certificates() {
        let garbage = [0x30_u8, 0x03, 0x02, 0x01, 0x00];
        let malformed = SevSnpCertificatesV1 {
            vcek_der: &garbage,
            ..MILAN
        };
        let oversized_bytes = vec![0_u8; SEV_SNP_MAX_CERTIFICATE_LEN + 1];
        let oversized = SevSnpCertificatesV1 {
            ask_der: &oversized_bytes,
            ..MILAN
        };
        let empty = SevSnpCertificatesV1 {
            vcek_der: &[],
            ..MILAN
        };
        let pin = sha256(MILAN_ARK);

        assert_eq!(
            verify_sev_snp_report_v1(MILAN_REPORT, malformed, &pin),
            Err(SevSnpRejectV1::CertificateMalformed)
        );
        assert_eq!(
            verify_sev_snp_report_v1(MILAN_REPORT, oversized, &pin),
            Err(SevSnpRejectV1::CertificateLength)
        );
        assert_eq!(
            verify_sev_snp_report_v1(MILAN_REPORT, empty, &pin),
            Err(SevSnpRejectV1::CertificateLength)
        );
    }

    /// The dstack simulator signs its mock certificates with ECDSA P-384. The
    /// AMD key hierarchy uses RSA-PSS, and the verifier accepts nothing else.
    /// A network that pins the simulator root therefore cannot register.
    #[test]
    fn rejects_the_ecdsa_certificate_chain_of_the_dstack_simulator() {
        let certificates = SevSnpCertificatesV1 {
            ark_der: SIMULATOR_ARK,
            ask_der: certificate_table_entry(SIMULATOR_CERT_TABLE, ASK_GUID),
            vcek_der: certificate_table_entry(SIMULATOR_CERT_TABLE, VCEK_GUID),
        };

        let result =
            verify_sev_snp_report_v1(SIMULATOR_REPORT, certificates, &sha256(SIMULATOR_ARK));

        assert_eq!(result, Err(SevSnpRejectV1::CertificateChain));
    }

    fn sev_snp_policy(measurement: &[u8; 48], host_data: [u8; 32]) -> TeePolicyV1 {
        use outbe_primitives::tee_attestation_v1::{
            PlatformTcbStatusSetV1, QvlTcbStatusV1, TeeMeasurementRuleV1,
        };
        TeePolicyV1 {
            policy_version: 1,
            chain_id: [0x11; 32],
            genesis_hash: B256::repeat_byte(0x22),
            activation_height: 1,
            predecessor_policy_hash: B256::ZERO,
            attestation_mode: AttestationMode::SevSnpRequired,
            intel_root_der_hash: sha256(MILAN_ARK),
            quote_version: 3,
            tee_type: 0,
            attestation_key_type: 2,
            qe_vendor_id: [
                0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
                0x06, 0x07,
            ],
            certification_data_type: 5,
            tcb_info_schema_version: 3,
            qe_identity_schema_version: 2,
            minimum_tcb_evaluation_data_number: 1,
            accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
            accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
            minimum_lease: 3_600,
            maximum_lease: 14 * 86_400,
            collateral_margin: 3_600,
            resource_schedule_hash: B256::repeat_byte(0x44),
            measurement_rules: vec![TeeMeasurementRuleV1 {
                mrenclave: sev_snp_measurement_rule_value(measurement),
                mrsigner: B256::from(host_data),
                isv_prod_id: SEV_SNP_RULE_PRODUCT_ID,
                minimum_isv_svn: SEV_SNP_RULE_SVN,
                admit_from_height: 1,
                admit_until_height_exclusive: u64::MAX,
            }],
        }
    }

    fn milan_evidence(policy: &TeePolicyV1) -> SevSnpEvidenceV1 {
        use outbe_primitives::tee_attestation_v1::{
            AttestationOperationV1, NodeIdV1, RegistrationIntentV1,
        };
        let reth_p2p_public = k256::ecdsa::SigningKey::from_bytes((&[0x77; 32]).into())
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        SevSnpEvidenceV1 {
            intent: RegistrationIntentV1 {
                chain_id: policy.chain_id,
                genesis_hash: policy.genesis_hash,
                operation: AttestationOperationV1::RegisterEnclave,
                attestation_mode: AttestationMode::SevSnpRequired,
                policy_hash: policy.policy_hash().unwrap(),
                node_id: NodeIdV1 { reth_p2p_public },
                enclave_id: B256::repeat_byte(0x99),
                binding_id: B256::repeat_byte(0xaa),
                binding_version: 1,
                registration_version: 1,
                renewal_nonce: 0,
                transition_nonce: 0,
                requested_valid_until: 7_200,
                recipient_x25519: [0xbb; 32],
                attestation_ed25519: [0xcc; 32],
                noise_responder_x25519: [0xdd; 32],
                node_host_authorization_hash: B256::repeat_byte(0xee),
            },
            report: MILAN_REPORT.to_vec(),
            ark_der: MILAN_ARK.to_vec(),
            ask_der: MILAN_ASK.to_vec(),
            vcek_der: MILAN_VCEK.to_vec(),
            transition_key_ready_proof: None,
        }
    }

    fn milan_identity() -> SevSnpVerifiedReportV1 {
        verify_milan(MILAN_REPORT).unwrap()
    }

    fn milan_policy() -> TeePolicyV1 {
        let identity = milan_identity();
        // The host data of the AMD sample report is zero. A policy rule cannot
        // hold a zero value, so the tests that need a matching rule use a
        // constructed identity instead.
        sev_snp_policy(&identity.measurement, [0x5a; 32])
    }

    #[test]
    fn verdict_projects_the_identity_and_never_expires() {
        let verified = SevSnpVerifiedReportV1 {
            measurement: [0x33; 48],
            host_data: [0x5a; 32],
            report_data: [0x41; 64],
        };
        let policy = sev_snp_policy(&verified.measurement, verified.host_data);

        let verdict = verdict_for_verified_report(&verified, &[0x41; 64], &policy).unwrap();

        assert_eq!(verdict.mrenclave, policy.measurement_rules[0].mrenclave);
        assert_eq!(verdict.mrsigner, B256::repeat_byte(0x5a));
        assert_eq!((verdict.isv_prod_id, verdict.isv_svn), (1, 1));
        assert_eq!(
            verdict.platform_tcb_status,
            DcapPlatformTcbStatusV1::UpToDate
        );
        assert_eq!(verdict.collateral_valid_until, u64::MAX);
        assert!(verdict.advisory_ids.is_empty());
        verdict.encode_canonical().unwrap();
    }

    #[test]
    fn verdict_rejects_another_measurement_host_data_or_report_data() {
        let verified = SevSnpVerifiedReportV1 {
            measurement: [0x33; 48],
            host_data: [0x5a; 32],
            report_data: [0x41; 64],
        };
        let policy = sev_snp_policy(&verified.measurement, verified.host_data);

        let other_measurement = SevSnpVerifiedReportV1 {
            measurement: [0x34; 48],
            ..verified
        };
        let other_host_data = SevSnpVerifiedReportV1 {
            host_data: [0x5b; 32],
            ..verified
        };

        assert_eq!(
            verdict_for_verified_report(&other_measurement, &[0x41; 64], &policy),
            Err(DcapRejectCodeV1::MeasurementRejected)
        );
        assert_eq!(
            verdict_for_verified_report(&other_host_data, &[0x41; 64], &policy),
            Err(DcapRejectCodeV1::MeasurementRejected)
        );
        assert_eq!(
            verdict_for_verified_report(&verified, &[0x42; 64], &policy),
            Err(DcapRejectCodeV1::ReportDataMismatch)
        );
    }

    #[test]
    fn verdict_rejects_a_rule_with_another_product_or_a_higher_svn() {
        let verified = SevSnpVerifiedReportV1 {
            measurement: [0x33; 48],
            host_data: [0x5a; 32],
            report_data: [0x41; 64],
        };
        let mut other_product = sev_snp_policy(&verified.measurement, verified.host_data);
        other_product.measurement_rules[0].isv_prod_id = 2;
        let mut higher_svn = sev_snp_policy(&verified.measurement, verified.host_data);
        higher_svn.measurement_rules[0].minimum_isv_svn = 2;

        for policy in [other_product, higher_svn] {
            assert_eq!(
                verdict_for_verified_report(&verified, &[0x41; 64], &policy),
                Err(DcapRejectCodeV1::MeasurementRejected)
            );
        }
    }

    /// The sample report binds the report data of AMD, not of this intent. The
    /// evidence therefore passes every check before the report data comparison
    /// and fails there.
    #[test]
    fn evidence_with_a_valid_report_for_another_intent_fails_at_the_report_data() {
        let policy = milan_policy();

        let result = verify_sev_snp_evidence(&milan_evidence(&policy), &policy, 1_000);

        assert_eq!(result, Err(DcapRejectCodeV1::ReportDataMismatch));
    }

    #[test]
    fn evidence_rejects_a_policy_of_another_mode_chain_or_hash() {
        let policy = milan_policy();
        let evidence = milan_evidence(&policy);

        let mut other_mode = policy.clone();
        other_mode.attestation_mode = AttestationMode::DcapRequired;
        let mut other_chain = policy.clone();
        other_chain.chain_id = [0x12; 32];
        let mut other_hash = policy.clone();
        other_hash.minimum_lease += 1;

        for policy in [other_mode, other_chain, other_hash] {
            assert_eq!(
                verify_sev_snp_evidence(&evidence, &policy, 1_000),
                Err(DcapRejectCodeV1::PolicyBindingMismatch)
            );
        }
    }

    #[test]
    fn evidence_maps_report_failures_to_protocol_reject_codes() {
        let policy = milan_policy();

        let mut other_root = policy.clone();
        other_root.intel_root_der_hash = B256::repeat_byte(0x11);
        let other_root_evidence = milan_evidence(&other_root);
        assert_eq!(
            verify_sev_snp_evidence(&other_root_evidence, &other_root, 1_000),
            Err(DcapRejectCodeV1::IntelRootMismatch)
        );

        let mut forged = milan_evidence(&policy);
        forged.report[0x90] ^= 0x01;
        assert_eq!(
            verify_sev_snp_evidence(&forged, &policy, 1_000),
            Err(DcapRejectCodeV1::NativeVerificationFailed)
        );

        let mut short = milan_evidence(&policy);
        short.report.pop();
        assert_eq!(
            verify_sev_snp_evidence(&short, &policy, 1_000),
            Err(DcapRejectCodeV1::EvidenceNonCanonical)
        );
    }
}
