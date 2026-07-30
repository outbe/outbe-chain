#![cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, AttestationOperationV1, DcapCollateralComponentV1, DcapCollateralKind,
    DcapEvidenceV1, EnclaveProfile, NodeIdV1, PlatformTcbStatusSetV1, QvlTcbStatusV1,
    RegistrationIntentV1, TeeMeasurementRuleV1, TeePolicyV1,
};
use outbe_tee::dcap_v1::{verify_dcap_evidence, DcapRejectCodeV1};
use serde::Deserialize;

const QUOTE: &[u8] = include_bytes!("fixtures/intel-dcap-1.26/sgx-processor-quote-v3.bin");
const COLLATERAL_WRAPPER: &str =
    include_str!("fixtures/intel-dcap-1.26/sgx-processor-collateral-wrapper.json");
const PEM_CERTIFICATE_BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";

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

fn signed_document(field: &str, body: &str, signature: &str) -> Vec<u8> {
    format!(r#"{{"{field}":{body},"signature":"{signature}"}}"#).into_bytes()
}

fn embedded_pck_chain() -> Vec<u8> {
    let start = QUOTE
        .windows(PEM_CERTIFICATE_BEGIN.len())
        .position(|window| window == PEM_CERTIFICATE_BEGIN)
        .unwrap();
    QUOTE[start..].to_vec()
}

fn policy() -> TeePolicyV1 {
    let intel_root_der_hash = B256::from_slice(
        &hex::decode("44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3").unwrap(),
    );
    TeePolicyV1 {
        policy_version: 1,
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash,
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
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x44),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            enclave_profile: EnclaveProfile::Validator,
            mrenclave: B256::repeat_byte(0x55),
            mrsigner: B256::repeat_byte(0x66),
            isv_prod_id: 0,
            minimum_isv_svn: 0,
            admit_from_height: 1,
            admit_until_height_exclusive: 100,
        }],
    }
}

fn evidence(policy: &TeePolicyV1) -> DcapEvidenceV1 {
    let collateral: FixtureCollateral = serde_json::from_str(COLLATERAL_WRAPPER).unwrap();
    let intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: policy.policy_hash().unwrap(),
        enclave_profile: EnclaveProfile::Validator,
        node_id: NodeIdV1::Validator {
            address: [0x77; 20],
            bls_minpk_public: [0x88; 48],
        },
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
            embedded_pck_chain(),
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
        quote: QUOTE.to_vec(),
        components: components
            .into_iter()
            .map(|(kind, bytes)| DcapCollateralComponentV1 { kind, bytes })
            .collect(),
    }
}

fn evidence_with_synthetic_intent_binding(policy: &TeePolicyV1) -> DcapEvidenceV1 {
    let mut evidence = evidence(policy);
    evidence.quote[368..432].copy_from_slice(&evidence.intent.report_data().unwrap());
    evidence
}

#[test]
fn quote_with_trailing_byte_is_rejected_by_the_consensus_interface() {
    let policy = policy();
    let mut evidence = evidence(&policy);
    evidence.quote.push(0xa5);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::QuoteMalformed)
    );
}

#[test]
fn quote_with_trailing_byte_inside_declared_authentication_data_is_rejected() {
    let policy = policy();
    let mut evidence = evidence(&policy);
    let declared = u32::from_le_bytes(evidence.quote[432..436].try_into().unwrap());
    evidence.quote[432..436].copy_from_slice(&(declared + 1).to_le_bytes());
    evidence.quote.push(0xa5);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::QuoteMalformed)
    );
}

#[test]
fn quote_header_must_match_the_active_sgx_v3_policy() {
    let policy = policy();
    let mut evidence = evidence(&policy);
    evidence.quote[0..2].copy_from_slice(&4_u16.to_le_bytes());

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::QuoteProfileMismatch)
    );
}

#[test]
fn embedded_type_five_pck_chain_must_equal_the_evidence_component() {
    let policy = policy();
    let mut evidence = evidence(&policy);
    evidence.components[0].bytes[0] ^= 1;

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::QuoteCertificationDataMismatch)
    );
}

#[test]
fn quote_report_data_must_equal_the_canonical_registration_intent_binding() {
    let policy = policy();
    let evidence = evidence(&policy);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::ReportDataMismatch)
    );
}

#[test]
fn semantically_equivalent_noncanonical_pem_collateral_is_rejected() {
    let policy = policy();
    let mut evidence = evidence_with_synthetic_intent_binding(&policy);
    evidence.components[2].bytes = String::from_utf8(evidence.components[2].bytes.clone())
        .unwrap()
        .replace('\n', "\r\n")
        .into_bytes();

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::CollateralNonCanonical)
    );
}

#[test]
fn signed_json_wrapper_with_equivalent_whitespace_is_rejected() {
    let policy = policy();
    let mut evidence = evidence_with_synthetic_intent_binding(&policy);
    evidence.components[4].bytes.insert(1, b' ');

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_000_000),
        Err(DcapRejectCodeV1::CollateralNonCanonical)
    );
}

#[test]
fn intent_rebound_quote_is_rejected_by_native_qvl() {
    let policy = policy();
    let evidence = evidence_with_synthetic_intent_binding(&policy);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_100_000),
        Err(DcapRejectCodeV1::NativeVerificationFailed)
    );
}

#[test]
fn consensus_time_before_either_signed_document_issue_time_is_rejected() {
    let policy = policy();
    let evidence = evidence_with_synthetic_intent_binding(&policy);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_750_330_570),
        Err(DcapRejectCodeV1::CollateralNotYetValid)
    );
}

#[test]
fn earliest_signed_document_expiration_is_exclusive() {
    let policy = policy();
    let evidence = evidence_with_synthetic_intent_binding(&policy);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_752_919_278),
        Err(DcapRejectCodeV1::CollateralExpired)
    );
}

#[test]
fn pck_chain_must_terminate_at_the_policy_pinned_intel_root() {
    let mut policy = policy();
    policy.intel_root_der_hash = B256::repeat_byte(0x33);
    let evidence = evidence_with_synthetic_intent_binding(&policy);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_100_000),
        Err(DcapRejectCodeV1::IntelRootMismatch)
    );
}

#[test]
fn both_signed_document_evaluation_numbers_must_meet_policy() {
    let mut policy = policy();
    policy.minimum_tcb_evaluation_data_number = 18;
    let evidence = evidence_with_synthetic_intent_binding(&policy);

    assert_eq!(
        verify_dcap_evidence(&evidence, &policy, 1_751_100_000),
        Err(DcapRejectCodeV1::TcbEvaluationNumberTooLow)
    );
}
