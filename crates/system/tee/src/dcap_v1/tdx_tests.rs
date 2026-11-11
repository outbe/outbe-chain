//! TDX verification against a real Intel TDX quote and the pinned native QVL.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, AttestationOperationV1, DcapCollateralComponentV1, DcapCollateralKind,
    NodeIdV1, RegistrationIntentV1, TeeMeasurementRuleV1, INTEL_QE_VENDOR_ID, TDX_QUOTE_VERSION_V1,
    TDX_TEE_TYPE_V1,
};
use serde::Deserialize;

use super::*;

const QUOTE_FILE: &[u8] =
    include_bytes!("../../tests/fixtures/intel-tdx-quote-v4/tdx-quote-v4.bin");
/// Length of the quote without the zero bytes that follow the signature data.
const QUOTE_BYTES: usize = 4936;
const COLLATERAL_WRAPPER: &str =
    include_str!("../../tests/fixtures/intel-tdx-quote-v4/tdx-collateral-wrapper.json");
/// A consensus time inside the validity window of the fixture collateral.
const FIXTURE_TIME: u64 = 1_751_000_000;
/// `nextUpdate` of the fixture TCB info: 2025-07-19T10:16:03Z.
const FIXTURE_TCB_INFO_NEXT_UPDATE: u64 = 1_752_920_163;
const INTEL_SGX_ROOT_CA_DER_SHA256: &str =
    "44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3";
const PEM_CERTIFICATE_BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
/// Offset of `TD_ATTRIBUTES` in a quote: the header and the field offset.
const QUOTE_TD_ATTRIBUTES: usize = 48 + 120;
/// Offset of `MRTD` in a quote.
const QUOTE_MRTD: usize = 48 + 136;

#[derive(Deserialize)]
struct FixtureCollateral {
    pck_crl_issuer_chain: String,
    root_ca_crl: String,
    pck_crl: String,
    tcb_info_issuer_chain: String,
    tcb_info: String,
    tcb_info_signature: String,
    qe_identity_issuer_chain: String,
    qe_identity: String,
    qe_identity_signature: String,
}

fn quote() -> Vec<u8> {
    assert!(QUOTE_FILE[QUOTE_BYTES..].iter().all(|byte| *byte == 0));
    QUOTE_FILE[..QUOTE_BYTES].to_vec()
}

fn embedded_pck_chain(quote: &[u8]) -> Vec<u8> {
    let start = quote
        .windows(PEM_CERTIFICATE_BEGIN.len())
        .position(|window| window == PEM_CERTIFICATE_BEGIN)
        .unwrap();
    quote[start..].to_vec()
}

fn signed_document(field: &str, body: &str, signature: &str) -> Vec<u8> {
    format!(r#"{{"{field}":{body},"signature":"{signature}"}}"#).into_bytes()
}

fn policy_for(quote: &[u8]) -> TeePolicyV1 {
    let measurements = parse_tdx_quote_measurements(quote).unwrap();
    TeePolicyV1 {
        policy_version: 1,
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::TdxRequired,
        intel_root_der_hash: B256::from_slice(&hex::decode(INTEL_SGX_ROOT_CA_DER_SHA256).unwrap()),
        quote_version: TDX_QUOTE_VERSION_V1,
        tee_type: TDX_TEE_TYPE_V1,
        attestation_key_type: 2,
        qe_vendor_id: INTEL_QE_VENDOR_ID,
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 7 * 86_400,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x44),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: measurements.image_rule_value(),
            mrsigner: measurements.mrconfigid_rule_value(),
            isv_prod_id: TDX_RULE_PRODUCT_ID_V1,
            minimum_isv_svn: TDX_RULE_SVN_V1,
            admit_from_height: 1,
            admit_until_height_exclusive: 100,
        }],
    }
}

fn evidence_for(quote: Vec<u8>, policy: &TeePolicyV1) -> DcapEvidenceV1 {
    let collateral: FixtureCollateral = serde_json::from_str(COLLATERAL_WRAPPER).unwrap();
    let reth_p2p_public = k256::ecdsa::SigningKey::from_bytes((&[0x77; 32]).into())
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::TdxRequired,
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
    };
    let components = [
        (
            DcapCollateralKind::PckCertificateChain,
            embedded_pck_chain(&quote),
        ),
        (
            DcapCollateralKind::PckCrl,
            hex::decode(&collateral.pck_crl).unwrap(),
        ),
        (
            DcapCollateralKind::PckCrlIssuerChain,
            collateral.pck_crl_issuer_chain.into_bytes(),
        ),
        (
            DcapCollateralKind::RootCaCrl,
            hex::decode(&collateral.root_ca_crl).unwrap(),
        ),
        (
            DcapCollateralKind::TcbInfo,
            signed_document(
                "tcbInfo",
                &collateral.tcb_info,
                &collateral.tcb_info_signature,
            ),
        ),
        (
            DcapCollateralKind::TcbInfoIssuerChain,
            collateral.tcb_info_issuer_chain.into_bytes(),
        ),
        (
            DcapCollateralKind::QeIdentity,
            signed_document(
                "enclaveIdentity",
                &collateral.qe_identity,
                &collateral.qe_identity_signature,
            ),
        ),
        (
            DcapCollateralKind::QeIdentityIssuerChain,
            collateral.qe_identity_issuer_chain.into_bytes(),
        ),
    ];
    DcapEvidenceV1 {
        intent,
        quote,
        components: components
            .into_iter()
            .map(|(kind, bytes)| DcapCollateralComponentV1 { kind, bytes })
            .collect(),
        transition_key_ready_proof: None,
    }
}

fn fixture() -> (TeePolicyV1, DcapEvidenceV1) {
    let quote = quote();
    let policy = policy_for(&quote);
    let evidence = evidence_for(quote, &policy);
    (policy, evidence)
}

/// The steps of [`verify_tdx_evidence`] without the report data comparison.
/// The fixture quote does not bind an Outbe intent.
fn verify_without_report_data_binding(
    evidence: &DcapEvidenceV1,
    policy: &TeePolicyV1,
    block_timestamp: u64,
) -> Result<DcapVerdictV1, DcapRejectCodeV1> {
    validate_evidence_context(
        AttestationEvidenceV1::Tdx(evidence.clone()),
        policy,
        block_timestamp,
    )?;
    assert_eq!(
        validate_tdx_quote_binding(evidence, policy),
        Err(DcapRejectCodeV1::ReportDataMismatch)
    );
    let measurements = parse_tdx_quote_measurements(&evidence.quote)?;
    validate_canonical_collateral(evidence, policy)?;
    let signed = validate_signed_collateral(evidence, policy, block_timestamp)?;
    let identity = validate_tdx_measurement_rule(&measurements, policy)?;
    verify_native_quote_and_build_verdict(
        QuoteTee::Tdx,
        evidence,
        policy,
        block_timestamp,
        identity,
        signed,
    )
}

#[test]
fn real_tdx_quote_is_accepted_by_the_pinned_native_qvl() {
    let (policy, evidence) = fixture();
    let measurements = parse_tdx_quote_measurements(&evidence.quote).unwrap();

    let verdict = verify_without_report_data_binding(&evidence, &policy, FIXTURE_TIME).unwrap();

    assert_eq!(verdict.mrenclave, measurements.image_rule_value());
    assert_eq!(verdict.mrsigner, measurements.mrconfigid_rule_value());
    assert_eq!(verdict.isv_prod_id, TDX_RULE_PRODUCT_ID_V1);
    assert_eq!(verdict.isv_svn, TDX_RULE_SVN_V1);
    assert_eq!(
        verdict.platform_tcb_status,
        DcapPlatformTcbStatusV1::UpToDate
    );
    assert_eq!(verdict.tcb_evaluation_data_number, 17);
    assert!(verdict.collateral_valid_until <= FIXTURE_TCB_INFO_NEXT_UPDATE);
    assert!(verdict.collateral_valid_until > FIXTURE_TIME);
}

#[test]
fn evidence_assembled_by_the_node_is_accepted() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("collateral.json"), COLLATERAL_WRAPPER).unwrap();
    // The node receives the quote with the zero bytes after its signature data.
    let assembled =
        crate::tdx_host::acquire_tdx_collateral_from(QUOTE_FILE, directory.path()).unwrap();
    let (policy, mut evidence) = fixture();
    assert_eq!(assembled.quote, evidence.quote);
    assert_eq!(assembled.components, evidence.components);
    evidence.quote = assembled.quote;
    evidence.components = assembled.components;

    assert!(verify_without_report_data_binding(&evidence, &policy, FIXTURE_TIME).is_ok());
}

#[test]
fn public_verifier_stops_at_the_report_data_of_a_foreign_quote() {
    let (policy, evidence) = fixture();

    assert_eq!(
        verify_tdx_evidence(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::ReportDataMismatch)
    );
}

#[test]
fn collateral_expiration_is_checked_against_consensus_time() {
    let (policy, evidence) = fixture();
    let verdict = verify_without_report_data_binding(&evidence, &policy, FIXTURE_TIME).unwrap();

    assert_eq!(
        verify_without_report_data_binding(&evidence, &policy, verdict.collateral_valid_until),
        Err(DcapRejectCodeV1::CollateralExpired)
    );
    assert!(verify_without_report_data_binding(
        &evidence,
        &policy,
        verdict.collateral_valid_until - 1
    )
    .is_ok());
}

#[test]
fn changed_measurement_fails_the_quote_signature() {
    let mut quote = quote();
    quote[QUOTE_MRTD] ^= 0x01;
    // The policy admits the changed measurement. Only the signature rejects.
    let policy = policy_for(&quote);
    let evidence = evidence_for(quote, &policy);

    assert_eq!(
        verify_without_report_data_binding(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::NativeVerificationFailed)
    );
}

#[test]
fn measurement_outside_the_policy_is_rejected() {
    let (mut policy, _) = fixture();
    policy.measurement_rules[0].mrsigner = B256::repeat_byte(0x55);
    let evidence = evidence_for(quote(), &policy);

    assert_eq!(
        verify_without_report_data_binding(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::MeasurementRejected)
    );
}

#[test]
fn debug_td_is_rejected() {
    let mut quote = quote();
    quote[QUOTE_TD_ATTRIBUTES] |= 0x01;
    let policy = policy_for(&quote);
    let evidence = evidence_for(quote, &policy);

    assert_eq!(
        verify_without_report_data_binding(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::DebugEnclaveRejected)
    );
}

#[test]
fn quote_with_bytes_after_the_signature_data_is_malformed() {
    let policy = policy_for(&quote());
    let evidence = evidence_for(QUOTE_FILE.to_vec(), &policy);

    assert_eq!(
        verify_tdx_evidence(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::QuoteMalformed)
    );
}

#[test]
fn sgx_quote_is_not_a_tdx_quote() {
    let (policy, mut evidence) = fixture();
    evidence.quote =
        include_bytes!("../../tests/fixtures/intel-dcap-1.26/sgx-processor-quote-v3.bin").to_vec();

    assert!(matches!(
        verify_tdx_evidence(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::QuoteMalformed | DcapRejectCodeV1::QuoteProfileMismatch)
    ));
}

#[test]
fn tdx_evidence_needs_a_tdx_policy() {
    let (mut policy, evidence) = fixture();
    policy.attestation_mode = AttestationMode::DcapRequired;

    assert_eq!(
        verify_tdx_evidence(&evidence, &policy, FIXTURE_TIME),
        Err(DcapRejectCodeV1::PolicyBindingMismatch)
    );
}
