//! Host-only preparation and assembly of real Intel DCAP consensus fixtures.
//!
//! This binary is available only with `dcap-fixture-tool`. The build never links
//! it into the consensus library or the production enclave process.

use std::{env, path::PathBuf};

use alloy_primitives::{hex, B256};
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, AttestationOperationV1, NodeIdV1, PlatformTcbStatusSetV1, QvlTcbStatusV1,
    RegistrationIntentV1, TeeMeasurementRuleV1, TeePolicyV1, INTEL_QE_VENDOR_ID,
};
use outbe_primitives::time::SECONDS_PER_DAY;

use outbe_tee::errors::DcapFixtureError;

#[path = "outbe-dcap-fixture/assemble.rs"]
mod assemble;
#[path = "outbe-dcap-fixture/cli.rs"]
mod cli;
use cli::{argument, ensure_empty_directory, parse_u64, write_new};

const INTEL_ROOT_DER_SHA256: &str =
    "44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3";

fn main() {
    if let Err(error) = run(&env::args().collect::<Vec<_>>()) {
        eprintln!("outbe-dcap-fixture: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: &[String]) -> Result<(), DcapFixtureError> {
    match arguments.get(1).map(String::as_str) {
        Some("prepare") => prepare(arguments),
        Some("assemble") => assemble::run(arguments),
        _ => Err(DcapFixtureError::Usage),
    }
}

fn prepare(arguments: &[String]) -> Result<(), DcapFixtureError> {
    let mrenclave = parse_hex32(&argument(arguments, "--mrenclave")?)?;
    let mrsigner = parse_hex32(&argument(arguments, "--mrsigner")?)?;
    let isv_prod_id = parse_u16(&argument(arguments, "--isv-prod-id")?, "isv-prod-id")?;
    let isv_svn = parse_u16(&argument(arguments, "--isv-svn")?, "isv-svn")?;
    let timestamp = parse_u64(&argument(arguments, "--timestamp")?, "timestamp")?;
    let output_dir = PathBuf::from(argument(arguments, "--output-dir")?);
    ensure_empty_directory(&output_dir)?;

    let policy = policy(mrenclave, mrsigner, isv_prod_id, isv_svn)?;
    let intent = intent(&policy, timestamp)?;
    let policy_bytes = policy
        .encode_canonical()
        .map_err(DcapFixtureError::encode_tee_policy)?;
    let intent_bytes = intent
        .encode_canonical()
        .map_err(DcapFixtureError::encode_registration_intent)?;
    let report_data = intent
        .report_data()
        .map_err(DcapFixtureError::derive_report_data)?;

    write_new(&output_dir.join("policy.bin"), &policy_bytes)?;
    write_new(&output_dir.join("intent.bin"), &intent_bytes)?;
    write_new(&output_dir.join("report-data.bin"), &report_data)?;
    let metadata = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "capture_timestamp": timestamp,
        "mrenclave": hex::encode(mrenclave),
        "mrsigner": hex::encode(mrsigner),
        "isv_prod_id": isv_prod_id,
        "isv_svn": isv_svn,
        "report_data": hex::encode(report_data),
    }))
    .map_err(DcapFixtureError::encode_capture_metadata)?;
    let mut metadata_with_newline = metadata;
    metadata_with_newline.push(b'\n');
    write_new(
        &output_dir.join("capture-input-v1.json"),
        &metadata_with_newline,
    )
}

fn policy(
    mrenclave: [u8; 32],
    mrsigner: [u8; 32],
    isv_prod_id: u16,
    isv_svn: u16,
) -> Result<TeePolicyV1, DcapFixtureError> {
    Ok(TeePolicyV1 {
        policy_version: 1,
        chain_id: [0x11; 32],
        genesis_hash: B256::repeat_byte(0x22),
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::from(parse_hex32(INTEL_ROOT_DER_SHA256)?),
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: INTEL_QE_VENDOR_ID,
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 7 * SECONDS_PER_DAY,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x44),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: B256::from(mrenclave),
            mrsigner: B256::from(mrsigner),
            isv_prod_id,
            minimum_isv_svn: isv_svn,
            admit_from_height: 1,
            admit_until_height_exclusive: u64::MAX,
        }],
    })
}

fn intent(policy: &TeePolicyV1, timestamp: u64) -> Result<RegistrationIntentV1, DcapFixtureError> {
    let reth_p2p_public = k256::ecdsa::SigningKey::from_bytes((&[0x77; 32]).into())
        .map_err(DcapFixtureError::construct_fixture_node_host_key)?
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(DcapFixtureError::encode_fixture_node_host_key)?;
    Ok(RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: policy
            .policy_hash()
            .map_err(DcapFixtureError::hash_tee_policy)?,
        node_id: NodeIdV1 { reth_p2p_public },
        enclave_id: B256::repeat_byte(0x99),
        binding_id: B256::repeat_byte(0xaa),
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: timestamp
            .checked_add(SECONDS_PER_DAY)
            .ok_or(DcapFixtureError::RequestedValidUntilOverflowsU64)?,
        recipient_x25519: [0xbb; 32],
        attestation_ed25519: [0xcc; 32],
        noise_responder_x25519: [0xdd; 32],
        node_host_authorization_hash: B256::repeat_byte(0xee),
    })
}

fn parse_u16(value: &str, label: &'static str) -> Result<u16, DcapFixtureError> {
    value.parse().map_err(DcapFixtureError::invalid_u16(label))
}

fn parse_hex32(value: &str) -> Result<[u8; 32], DcapFixtureError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DcapFixtureError::ExpectedHex32);
    }
    let mut decoded = [0u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = (hex_nibble(value.as_bytes()[index * 2])? << 4)
            | hex_nibble(value.as_bytes()[index * 2 + 1])?;
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> Result<u8, DcapFixtureError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(DcapFixtureError::InvalidHexDigit),
    }
}
