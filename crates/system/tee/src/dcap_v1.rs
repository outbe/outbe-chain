//! Consensus-facing DCAP verification for attestation protocol V1.
//!
//! Callers supply only canonical evidence, the active policy and consensus
//! time. Quote grammar, collateral adaptation, native QVL invocation and
//! policy mapping remain private implementation details.

mod pck_certificate;
mod quote_layout;
mod signed_collateral;

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, DcapEvidenceV1, PlatformTcbStatusSetV1, TeePolicyV1,
};

pub use crate::dcap_protocol::{
    DcapPckCaV1, DcapPlatformTcbStatusV1, DcapRejectCodeV1, DcapVerdictV1,
};
use crate::native_qvl::{
    verify_quote_native, NativeDcapCollateral, NativeQvlError, NativeQvlStatus,
    NativeQvlSupplemental, NativeQvlVerdict,
};
use crate::quote::ReportMeasurements;
use pck_certificate::{
    parse_pck_identity, pck_root_der_hash, validate_canonical_certificate_chain,
    validate_canonical_der_crl, validate_canonical_pck_certificate_chain, PckIdentity,
};
use quote_layout::{
    parse_quote_authentication_data, validate_quote_outer_length, validate_quote_profile,
};
use signed_collateral::{
    parse_signed_qe_identity, parse_signed_tcb_info, QeIdentityMetadata, TcbInfoMetadata,
};

/// Verify one canonical DCAP evidence value using only consensus inputs.
pub fn verify_dcap_evidence(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
) -> Result<DcapVerdictV1, DcapRejectCodeV1> {
    validate_evidence_context(evidence, policy, block_timestamp)?;
    let measurements = validate_quote_binding(evidence, policy)?;
    validate_canonical_collateral(evidence, policy)?;
    let signed = validate_signed_collateral(evidence, policy, block_timestamp)?;
    validate_measurement_rule(&measurements, policy)?;
    verify_native_quote_and_build_verdict(evidence, policy, block_timestamp, measurements, signed)
}

fn validate_evidence_context(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
) -> Result<(), DcapRejectCodeV1> {
    AttestationEvidenceV1::Dcap(evidence.clone())
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    policy
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    let policy_hash = policy
        .policy_hash()
        .map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    if evidence.intent.chain_id != policy.chain_id
        || evidence.intent.genesis_hash != policy.genesis_hash
        || evidence.intent.policy_hash != policy_hash
    {
        return Err(DcapRejectCodeV1::PolicyBindingMismatch);
    }
    if block_timestamp > i64::MAX as u64 {
        return Err(DcapRejectCodeV1::TimestampInvalid);
    }
    Ok(())
}

fn validate_quote_binding(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
) -> Result<ReportMeasurements, DcapRejectCodeV1> {
    validate_quote_outer_length(&evidence.quote)?;
    validate_quote_profile(&evidence.quote, policy)?;
    let quote_authentication = parse_quote_authentication_data(&evidence.quote)?;
    let submitted_pck_chain = evidence
        .components
        .first()
        .ok_or(DcapRejectCodeV1::EvidenceNonCanonical)?;
    if quote_authentication.certification_data_type != policy.certification_data_type
        || quote_authentication.certification_data != submitted_pck_chain.bytes
    {
        return Err(DcapRejectCodeV1::QuoteCertificationDataMismatch);
    }
    let measurements = crate::quote::parse_quote_measurements(&evidence.quote)
        .map_err(|_| DcapRejectCodeV1::QuoteMalformed)?;
    let expected_report_data = evidence
        .intent
        .report_data()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    if measurements.report_data != expected_report_data {
        return Err(DcapRejectCodeV1::ReportDataMismatch);
    }
    Ok(measurements)
}

fn validate_canonical_collateral(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
) -> Result<(), DcapRejectCodeV1> {
    validate_canonical_pck_certificate_chain(component(evidence, 0)?)?;
    validate_canonical_der_crl(component(evidence, 1)?)?;
    validate_canonical_certificate_chain(component(evidence, 2)?, 2)?;
    validate_canonical_der_crl(component(evidence, 3)?)?;
    validate_canonical_certificate_chain(component(evidence, 5)?, 2)?;
    validate_canonical_certificate_chain(component(evidence, 7)?, 2)?;
    if pck_root_der_hash(component(evidence, 0)?)? != policy.intel_root_der_hash {
        return Err(DcapRejectCodeV1::IntelRootMismatch);
    }
    Ok(())
}

struct ValidatedDcapClaims {
    tcb_info: TcbInfoMetadata,
    qe_identity: QeIdentityMetadata,
    pck_identity: PckIdentity,
    issue_floor: u64,
    expiration_ceiling: u64,
}

fn validate_signed_collateral(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
) -> Result<ValidatedDcapClaims, DcapRejectCodeV1> {
    let tcb_info = parse_signed_tcb_info(component(evidence, 4)?, policy)?;
    let qe_identity = parse_signed_qe_identity(component(evidence, 6)?, policy)?;
    let issue_floor = tcb_info.issue_date.max(qe_identity.issue_date);
    let expiration_ceiling = tcb_info.next_update.min(qe_identity.next_update);
    if block_timestamp < issue_floor {
        return Err(DcapRejectCodeV1::CollateralNotYetValid);
    }
    if block_timestamp >= expiration_ceiling {
        return Err(DcapRejectCodeV1::CollateralExpired);
    }
    if tcb_info.tcb_evaluation_data_number < policy.minimum_tcb_evaluation_data_number
        || qe_identity.tcb_evaluation_data_number < policy.minimum_tcb_evaluation_data_number
    {
        return Err(DcapRejectCodeV1::TcbEvaluationNumberTooLow);
    }
    let pck_identity = parse_pck_identity(component(evidence, 0)?)?;
    if pck_identity.fmspc != tcb_info.fmspc || pck_identity.pce_id != tcb_info.pce_id {
        return Err(DcapRejectCodeV1::PlatformIdentityMismatch);
    }
    Ok(ValidatedDcapClaims {
        tcb_info,
        qe_identity,
        pck_identity,
        issue_floor,
        expiration_ceiling,
    })
}

fn validate_measurement_rule(
    measurements: &ReportMeasurements,
    policy: &TeePolicyV1,
) -> Result<(), DcapRejectCodeV1> {
    let measurement_accepted = policy.measurement_rules.iter().any(|rule| {
        rule.mrenclave == B256::from(measurements.mrenclave)
            && rule.mrsigner == B256::from(measurements.mrsigner)
            && rule.isv_prod_id == measurements.isv_prod_id
            && measurements.isv_svn >= rule.minimum_isv_svn
    });
    if !measurement_accepted {
        return Err(DcapRejectCodeV1::MeasurementRejected);
    }
    Ok(())
}

fn verify_native_quote_and_build_verdict(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
    measurements: ReportMeasurements,
    signed: ValidatedDcapClaims,
) -> Result<DcapVerdictV1, DcapRejectCodeV1> {
    let ValidatedDcapClaims {
        tcb_info,
        qe_identity,
        pck_identity,
        issue_floor,
        expiration_ceiling,
    } = signed;
    let native_verdict = verify_native_quote(evidence, block_timestamp)?;
    let signed_pce_id = u16::from_be_bytes(tcb_info.pce_id);
    let collateral_window = reconcile_native_supplemental(
        &native_verdict.supplemental,
        SignedCollateralClaims {
            tee_type: policy.tee_type,
            pce_id: signed_pce_id,
            issue_floor,
            expiration_ceiling,
            platform_tcb_evaluation_data_number: tcb_info.tcb_evaluation_data_number,
            qe_tcb_evaluation_data_number: qe_identity.tcb_evaluation_data_number,
        },
    )?;
    if block_timestamp < collateral_window.issue_floor {
        return Err(DcapRejectCodeV1::CollateralNotYetValid);
    }
    if native_verdict.collateral_expired || block_timestamp >= collateral_window.expiration_ceiling
    {
        return Err(DcapRejectCodeV1::CollateralExpired);
    }
    validate_qe_status(native_verdict.supplemental.qe_status)?;
    let platform_tcb_status = map_platform_status(
        native_verdict.aggregate_status,
        policy.accepted_platform_tcb_statuses,
    )?;

    Ok(DcapVerdictV1 {
        mrenclave: B256::from(measurements.mrenclave),
        mrsigner: B256::from(measurements.mrsigner),
        isv_prod_id: measurements.isv_prod_id,
        isv_svn: measurements.isv_svn,
        pck_ca: pck_identity.ca,
        fmspc: tcb_info.fmspc,
        pce_id: signed_pce_id,
        platform_tcb_status,
        advisory_ids: native_verdict.supplemental.advisory_ids,
        tcb_evaluation_data_number: tcb_info.tcb_evaluation_data_number,
        qe_tcb_evaluation_data_number: qe_identity.tcb_evaluation_data_number,
        collateral_valid_until: collateral_window.expiration_ceiling,
    })
}

fn verify_native_quote(
    evidence: &DcapEvidenceV1,
    block_timestamp: u64,
) -> Result<NativeQvlVerdict, DcapRejectCodeV1> {
    let native_collateral = NativeDcapCollateral {
        pck_crl_issuer_chain: component(evidence, 2)?,
        root_ca_crl: component(evidence, 3)?,
        pck_crl: component(evidence, 1)?,
        tcb_info_issuer_chain: component(evidence, 5)?,
        tcb_info: component(evidence, 4)?,
        qe_identity_issuer_chain: component(evidence, 7)?,
        qe_identity: component(evidence, 6)?,
    };
    verify_quote_native(
        &evidence.quote,
        &native_collateral,
        i64::try_from(block_timestamp).map_err(|_| DcapRejectCodeV1::TimestampInvalid)?,
    )
    .map_err(map_native_error)
}

/// Signed Intel collateral time bounds needed by the host renewal scheduler.
///
/// This is deliberately narrower than [`verify_dcap_evidence`]. It validates
/// the canonical TCB-info and QE-identity envelopes under the active policy and
/// returns only their intersection. Quote acceptance remains consensus work in
/// the enclave QVL path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DcapCollateralValidityWindowV1 {
    pub issue_floor: u64,
    pub expiration_ceiling: u64,
}

pub fn dcap_collateral_validity_window_v1(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
) -> Result<DcapCollateralValidityWindowV1, DcapRejectCodeV1> {
    AttestationEvidenceV1::Dcap(evidence.clone())
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::EvidenceNonCanonical)?;
    policy
        .encode_canonical()
        .map_err(|_| DcapRejectCodeV1::PolicyNonCanonical)?;
    let tcb_info = parse_signed_tcb_info(component(evidence, 4)?, policy)?;
    let qe_identity = parse_signed_qe_identity(component(evidence, 6)?, policy)?;
    Ok(DcapCollateralValidityWindowV1 {
        issue_floor: tcb_info.issue_date.max(qe_identity.issue_date),
        expiration_ceiling: tcb_info.next_update.min(qe_identity.next_update),
    })
}

#[derive(Clone, Copy)]
struct SignedCollateralClaims {
    tee_type: u32,
    pce_id: u16,
    issue_floor: u64,
    expiration_ceiling: u64,
    platform_tcb_evaluation_data_number: u32,
    qe_tcb_evaluation_data_number: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CollateralWindow {
    issue_floor: u64,
    expiration_ceiling: u64,
}

fn reconcile_native_supplemental(
    supplemental: &NativeQvlSupplemental,
    signed: SignedCollateralClaims,
) -> Result<CollateralWindow, DcapRejectCodeV1> {
    let issue_floor = u64::try_from(supplemental.latest_issue_date)
        .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
    let expiration_ceiling = u64::try_from(supplemental.earliest_expiration_date)
        .map_err(|_| DcapRejectCodeV1::NativeOutputMalformed)?;
    let expected_tcb_evaluation_reference = signed
        .platform_tcb_evaluation_data_number
        .min(signed.qe_tcb_evaluation_data_number);
    let claims_match = [
        supplemental.tee_type == signed.tee_type,
        supplemental.pce_id == signed.pce_id,
        supplemental.tcb_evaluation_data_number == expected_tcb_evaluation_reference,
        supplemental.qe_tcb_evaluation_data_number == 0
            || supplemental.qe_tcb_evaluation_data_number == signed.qe_tcb_evaluation_data_number,
        issue_floor >= signed.issue_floor,
        expiration_ceiling <= signed.expiration_ceiling,
    ]
    .into_iter()
    .all(|matches| matches);
    if !claims_match {
        return Err(DcapRejectCodeV1::NativeOutputMalformed);
    }
    Ok(CollateralWindow {
        issue_floor,
        expiration_ceiling,
    })
}

const fn map_native_error(error: NativeQvlError) -> DcapRejectCodeV1 {
    match error {
        NativeQvlError::InvalidInput => DcapRejectCodeV1::CollateralNonCanonical,
        NativeQvlError::UnsupportedAbi => DcapRejectCodeV1::NativeVerifierUnavailable,
        NativeQvlError::VerificationFailed => DcapRejectCodeV1::NativeVerificationFailed,
        NativeQvlError::UnsupportedResult | NativeQvlError::MalformedSupplemental => {
            DcapRejectCodeV1::NativeOutputMalformed
        }
    }
}

const fn map_platform_status(
    status: NativeQvlStatus,
    accepted: PlatformTcbStatusSetV1,
) -> Result<DcapPlatformTcbStatusV1, DcapRejectCodeV1> {
    match (status, accepted) {
        (NativeQvlStatus::UpToDate, _) => Ok(DcapPlatformTcbStatusV1::UpToDate),
        (NativeQvlStatus::SWHardeningNeeded, PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded) => {
            Ok(DcapPlatformTcbStatusV1::SWHardeningNeeded)
        }
        (
            NativeQvlStatus::ConfigurationAndSWHardeningNeeded,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        ) => Ok(DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded),
        _ => Err(DcapRejectCodeV1::PlatformTcbRejected),
    }
}

const fn validate_qe_status(status: NativeQvlStatus) -> Result<(), DcapRejectCodeV1> {
    match status {
        NativeQvlStatus::UpToDate => Ok(()),
        _ => Err(DcapRejectCodeV1::QeTcbRejected),
    }
}

fn component(evidence: &DcapEvidenceV1, index: usize) -> Result<&[u8], DcapRejectCodeV1> {
    evidence
        .components
        .get(index)
        .map(|component| component.bytes.as_slice())
        .ok_or(DcapRejectCodeV1::EvidenceNonCanonical)
}

#[cfg(test)]
mod tests {
    use outbe_primitives::tee_attestation_v1::PlatformTcbStatusSetV1;

    use super::*;
    use crate::native_qvl::{NativeQvlStatus, NativeQvlSupplemental, SGX_QVL_STATUS_VECTORS};

    fn supplemental() -> NativeQvlSupplemental {
        NativeQvlSupplemental {
            major_version: 3,
            minor_version: 0,
            earliest_issue_date: 100,
            latest_issue_date: 200,
            earliest_expiration_date: 300,
            tcb_evaluation_data_number: 19,
            pce_id: 7,
            tee_type: 0,
            sgx_type: 0,
            dynamic_platform: 0,
            cached_keys: 0,
            smt_enabled: 0,
            advisory_ids: Vec::new(),
            qe_status: NativeQvlStatus::UpToDate,
            qe_tcb_evaluation_data_number: 0,
        }
    }

    fn signed_collateral_claims() -> SignedCollateralClaims {
        SignedCollateralClaims {
            tee_type: 0,
            pce_id: 7,
            issue_floor: 190,
            expiration_ceiling: 310,
            platform_tcb_evaluation_data_number: 20,
            qe_tcb_evaluation_data_number: 19,
        }
    }

    #[test]
    fn supplemental_reconciliation_uses_lower_evaluation_reference_and_all_collateral_time() {
        assert_eq!(
            reconcile_native_supplemental(&supplemental(), signed_collateral_claims()),
            Ok(CollateralWindow {
                issue_floor: 200,
                expiration_ceiling: 300,
            })
        );
    }

    #[test]
    fn supplemental_reconciliation_rejects_wrong_combined_evaluation_reference() {
        let mut supplemental = supplemental();
        supplemental.tcb_evaluation_data_number = 20;

        assert_eq!(
            reconcile_native_supplemental(&supplemental, signed_collateral_claims()),
            Err(DcapRejectCodeV1::NativeOutputMalformed)
        );
    }

    #[test]
    fn supplemental_reconciliation_rejects_wrong_nonzero_qe_evaluation_reference() {
        let mut supplemental = supplemental();
        supplemental.qe_tcb_evaluation_data_number = 18;

        assert_eq!(
            reconcile_native_supplemental(&supplemental, signed_collateral_claims()),
            Err(DcapRejectCodeV1::NativeOutputMalformed)
        );
    }

    #[test]
    fn supplemental_reconciliation_requires_all_collateral_window_to_be_narrower() {
        let claims = signed_collateral_claims();
        let mut issue_before_signed_documents = supplemental();
        issue_before_signed_documents.latest_issue_date = 189;
        let mut expiry_after_signed_documents = supplemental();
        expiry_after_signed_documents.earliest_expiration_date = 311;

        for supplemental in [issue_before_signed_documents, expiry_after_signed_documents] {
            assert_eq!(
                reconcile_native_supplemental(&supplemental, claims),
                Err(DcapRejectCodeV1::NativeOutputMalformed)
            );
        }
    }

    #[test]
    fn platform_status_matrix_is_exact() {
        let statuses = SGX_QVL_STATUS_VECTORS.map(|(_, status)| status);
        for accepted in [
            PlatformTcbStatusSetV1::UpToDateOnly,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        ] {
            for status in statuses {
                let expected = match (status, accepted) {
                    (NativeQvlStatus::UpToDate, _) => Ok(DcapPlatformTcbStatusV1::UpToDate),
                    (
                        NativeQvlStatus::SWHardeningNeeded,
                        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
                    ) => Ok(DcapPlatformTcbStatusV1::SWHardeningNeeded),
                    (
                        NativeQvlStatus::ConfigurationAndSWHardeningNeeded,
                        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
                    ) => Ok(DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded),
                    _ => Err(DcapRejectCodeV1::PlatformTcbRejected),
                };
                assert_eq!(map_platform_status(status, accepted), expected);
            }
        }
    }

    #[test]
    fn qe_status_matrix_accepts_only_up_to_date() {
        let statuses = SGX_QVL_STATUS_VECTORS.map(|(_, status)| status);
        for status in statuses {
            let expected = if status == NativeQvlStatus::UpToDate {
                Ok(())
            } else {
                Err(DcapRejectCodeV1::QeTcbRejected)
            };
            assert_eq!(validate_qe_status(status), expected);
        }
    }

    #[test]
    fn native_errors_map_to_stable_reject_codes_exhaustively() {
        assert_eq!(
            [
                NativeQvlError::InvalidInput,
                NativeQvlError::UnsupportedAbi,
                NativeQvlError::VerificationFailed,
                NativeQvlError::UnsupportedResult,
                NativeQvlError::MalformedSupplemental,
            ]
            .map(map_native_error),
            [
                DcapRejectCodeV1::CollateralNonCanonical,
                DcapRejectCodeV1::NativeVerifierUnavailable,
                DcapRejectCodeV1::NativeVerificationFailed,
                DcapRejectCodeV1::NativeOutputMalformed,
                DcapRejectCodeV1::NativeOutputMalformed,
            ]
        );
    }

    #[test]
    fn official_intel_platform_ca_vector_has_stable_public_identity() {
        let mut pck_chain =
            include_bytes!("../tests/fixtures/intel-platform-ca-parser/platform-pck-leaf.pem")
                .to_vec();
        pck_chain.push(0);

        let identity = parse_pck_identity(&pck_chain).unwrap();
        assert_eq!(identity.ca, DcapPckCaV1::Platform);
        assert_eq!(identity.fmspc, [0x10, 0x47, 0x5c, 0x0d, 0x00, 0x00]);
        assert_eq!(identity.pce_id, [0x00, 0x00]);
    }

    #[test]
    fn stable_verdict_encoding_is_byte_exact() {
        let verdict = DcapVerdictV1 {
            mrenclave: B256::repeat_byte(0x11),
            mrsigner: B256::repeat_byte(0x22),
            isv_prod_id: 0x3344,
            isv_svn: 0x5566,
            pck_ca: DcapPckCaV1::Platform,
            fmspc: [1, 2, 3, 4, 5, 6],
            pce_id: 0x7788,
            platform_tcb_status: DcapPlatformTcbStatusV1::SWHardeningNeeded,
            advisory_ids: vec!["INTEL-SA-00001".to_owned(), "INTEL-SA-00002".to_owned()],
            tcb_evaluation_data_number: 0x99aa_bbcc,
            qe_tcb_evaluation_data_number: 0xddee_ff00,
            collateral_valid_until: 0x0102_0304_0506_0708,
        };
        let mut expected = vec![1];
        expected.extend_from_slice(&[0x11; 32]);
        expected.extend_from_slice(&[0x22; 32]);
        expected.extend_from_slice(&0x3344_u16.to_be_bytes());
        expected.extend_from_slice(&0x5566_u16.to_be_bytes());
        expected.push(DcapPckCaV1::Platform as u8);
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        expected.extend_from_slice(&0x7788_u16.to_be_bytes());
        expected.push(DcapPlatformTcbStatusV1::SWHardeningNeeded as u8);
        expected.extend_from_slice(&0x99aa_bbcc_u32.to_be_bytes());
        expected.extend_from_slice(&0xddee_ff00_u32.to_be_bytes());
        expected.extend_from_slice(&0x0102_0304_0506_0708_u64.to_be_bytes());
        expected.extend_from_slice(&2_u16.to_be_bytes());
        for advisory in ["INTEL-SA-00001", "INTEL-SA-00002"] {
            expected.extend_from_slice(&(advisory.len() as u16).to_be_bytes());
            expected.extend_from_slice(advisory.as_bytes());
        }

        assert_eq!(verdict.encode_canonical().unwrap(), expected);
    }

    #[test]
    fn stable_reject_codes_are_byte_exact() {
        assert_eq!(
            [
                DcapRejectCodeV1::EvidenceNonCanonical.code(),
                DcapRejectCodeV1::PolicyNonCanonical.code(),
                DcapRejectCodeV1::PolicyBindingMismatch.code(),
                DcapRejectCodeV1::TimestampInvalid.code(),
                DcapRejectCodeV1::QuoteMalformed.code(),
                DcapRejectCodeV1::QuoteProfileMismatch.code(),
                DcapRejectCodeV1::QuoteCertificationDataMismatch.code(),
                DcapRejectCodeV1::ReportDataMismatch.code(),
                DcapRejectCodeV1::CollateralNonCanonical.code(),
                DcapRejectCodeV1::IntelRootMismatch.code(),
                DcapRejectCodeV1::PlatformIdentityMismatch.code(),
                DcapRejectCodeV1::CollateralNotYetValid.code(),
                DcapRejectCodeV1::CollateralExpired.code(),
                DcapRejectCodeV1::NativeVerifierUnavailable.code(),
                DcapRejectCodeV1::NativeVerificationFailed.code(),
                DcapRejectCodeV1::NativeOutputMalformed.code(),
                DcapRejectCodeV1::PlatformTcbRejected.code(),
                DcapRejectCodeV1::QeTcbRejected.code(),
                DcapRejectCodeV1::TcbEvaluationNumberTooLow.code(),
                DcapRejectCodeV1::MeasurementRejected.code(),
            ],
            [
                0x0101, 0x0102, 0x0103, 0x0104, 0x0201, 0x0202, 0x0203, 0x0204, 0x0301, 0x0302,
                0x0303, 0x0304, 0x0305, 0x0401, 0x0402, 0x0403, 0x0501, 0x0502, 0x0503, 0x0601,
            ]
        );
    }
}
