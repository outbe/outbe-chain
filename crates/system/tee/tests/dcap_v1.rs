#![cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, AttestationOperationV1, DcapCollateralComponentV1, DcapCollateralKind,
    DcapEvidenceV1, EnclaveProfile, NodeIdV1, PlatformTcbStatusSetV1, QvlTcbStatusV1,
    RegistrationIntentV1, TeeMeasurementRuleV1, TeePolicyV1,
};
use outbe_tee::dcap_v1::{verify_dcap_evidence, DcapRejectCodeV1};

const QUOTE: &[u8] = include_bytes!("fixtures/intel-dcap-1.26/sgx-processor-quote-v3.bin");
const PEM_CERTIFICATE_BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";

fn embedded_pck_chain() -> Vec<u8> {
    let start = QUOTE
        .windows(PEM_CERTIFICATE_BEGIN.len())
        .position(|window| window == PEM_CERTIFICATE_BEGIN)
        .unwrap();
    QUOTE[start..].to_vec()
}

fn policy() -> TeePolicyV1 {
    TeePolicyV1 {
        policy_version: 1,
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x33),
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
    let kinds = [
        DcapCollateralKind::PckCertificateChain,
        DcapCollateralKind::PckCrl,
        DcapCollateralKind::PckCrlIssuerChain,
        DcapCollateralKind::RootCaCrl,
        DcapCollateralKind::TcbInfo,
        DcapCollateralKind::TcbInfoIssuerChain,
        DcapCollateralKind::QeIdentity,
        DcapCollateralKind::QeIdentityIssuerChain,
    ];
    DcapEvidenceV1 {
        intent,
        quote: QUOTE.to_vec(),
        components: kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| DcapCollateralComponentV1 {
                kind,
                bytes: if index == 0 {
                    embedded_pck_chain()
                } else {
                    vec![1]
                },
            })
            .collect(),
    }
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
