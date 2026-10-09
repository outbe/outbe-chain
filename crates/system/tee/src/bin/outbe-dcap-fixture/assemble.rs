use super::cli::{argument, ensure_empty_directory, parse_u64, write_new};
use alloy_primitives::hex;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, DcapCollateralComponentV1, DcapEvidenceV1, RegistrationIntentV1,
    TeePolicyV1,
};
use outbe_tee::release_dcap_artifacts::DCAP_COLLATERAL_COMPONENT_FILES;
use outbe_tee::{
    dcap_protocol::DcapVerdictV1,
    dcap_v1::{verify_dcap_evidence, DcapRejectCodeV1},
    native_qvl::{verify_quote_native, NativeDcapCollateral},
    quote::parse_quote_measurements,
};
use sha2::{Digest, Sha256};

pub fn run(arguments: &[String]) -> Result<(), String> {
    let capture = load_capture(arguments)?;
    let collateral = load_and_preflight_collateral(&capture)?;
    let verified = verify_evidence(&capture, &collateral)?;
    write_fixture(&capture, &collateral, &verified)
}

struct CaptureInputs {
    collateral_dir: PathBuf,
    output_dir: PathBuf,
    timestamp: u64,
    policy_bytes: Vec<u8>,
    intent_bytes: Vec<u8>,
    quote: Vec<u8>,
    policy: TeePolicyV1,
    intent: RegistrationIntentV1,
    expected_report_data: [u8; 64],
}

fn load_capture(arguments: &[String]) -> Result<CaptureInputs, String> {
    let policy_path = PathBuf::from(argument(arguments, "--policy")?);
    let intent_path = PathBuf::from(argument(arguments, "--intent")?);
    let quote_path = PathBuf::from(argument(arguments, "--quote")?);
    let collateral_dir = PathBuf::from(argument(arguments, "--collateral-dir")?);
    let timestamp = parse_u64(&argument(arguments, "--timestamp")?, "timestamp")?;
    let output_dir = PathBuf::from(argument(arguments, "--output-dir")?);

    let policy_bytes = read(&policy_path)?;
    let intent_bytes = read(&intent_path)?;
    let quote = read(&quote_path)?;
    let policy = TeePolicyV1::decode_canonical(&policy_bytes)
        .map_err(|error| format!("decode canonical TeePolicyV1: {error}"))?;
    let intent = RegistrationIntentV1::decode_canonical(&intent_bytes)
        .map_err(|error| format!("decode canonical RegistrationIntentV1: {error}"))?;
    let measurements = parse_quote_measurements(&quote)
        .map_err(|error| format!("parse captured SGX quote: {error}"))?;
    let expected_report_data = intent
        .report_data()
        .map_err(|error| format!("derive RegistrationIntentV1 report_data: {error}"))?;
    if measurements.report_data != expected_report_data {
        return Err("quote REPORT_DATA does not match the canonical intent".to_owned());
    }
    Ok(CaptureInputs {
        collateral_dir,
        output_dir,
        timestamp,
        policy_bytes,
        intent_bytes,
        quote,
        policy,
        intent,
        expected_report_data,
    })
}

struct CapturedCollateral {
    components: Vec<DcapCollateralComponentV1>,
    files: BTreeMap<&'static str, Vec<u8>>,
    provenance: Vec<u8>,
}

fn load_and_preflight_collateral(capture: &CaptureInputs) -> Result<CapturedCollateral, String> {
    let mut files = BTreeMap::new();
    let components = DCAP_COLLATERAL_COMPONENT_FILES
        .into_iter()
        .map(|(kind, name)| {
            let bytes = read(&capture.collateral_dir.join(name))?;
            files.insert(name, bytes.clone());
            Ok(DcapCollateralComponentV1 { kind, bytes })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let provenance = read(&capture.collateral_dir.join("capture-provenance.json"))?;
    serde_json::from_slice::<serde_json::Value>(&provenance)
        .map_err(|error| format!("decode capture-provenance.json: {error}"))?;
    let component = |name: &str| {
        files
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("missing captured component {name}"))
    };
    let native_collateral = NativeDcapCollateral {
        pck_crl_issuer_chain: component("pck-crl-issuer-chain.pem")?,
        root_ca_crl: component("root-ca.crl.der")?,
        pck_crl: component("pck.crl.der")?,
        tcb_info_issuer_chain: component("tcb-info-issuer-chain.pem")?,
        tcb_info: component("tcb-info.json")?,
        qe_identity_issuer_chain: component("qe-identity-issuer-chain.pem")?,
        qe_identity: component("qe-identity.json")?,
    };
    let native_preflight = verify_quote_native(
        &capture.quote,
        &native_collateral,
        i64::try_from(capture.timestamp).map_err(|_| "timestamp exceeds i64".to_owned())?,
    )
    .map_err(|error| format!("native QVL preflight failed: {error:?}"))?;
    eprintln!("native QVL preflight: {native_preflight:?}");
    eprintln!(
        "native QVL supplemental: issue={}..{} expiration={} combined_eval_ref={} qe_eval_ref={}",
        native_preflight.supplemental.earliest_issue_date,
        native_preflight.supplemental.latest_issue_date,
        native_preflight.supplemental.earliest_expiration_date,
        native_preflight.supplemental.tcb_evaluation_data_number,
        native_preflight.supplemental.qe_tcb_evaluation_data_number,
    );
    Ok(CapturedCollateral {
        components,
        files,
        provenance,
    })
}

struct VerifiedEvidence {
    evidence_bytes: Vec<u8>,
    verdict: DcapVerdictV1,
    verdict_bytes: Vec<u8>,
    tampered_evidence_bytes: Vec<u8>,
    tampered_reject: DcapRejectCodeV1,
    reject_bytes: [u8; 2],
}

fn verify_evidence(
    capture: &CaptureInputs,
    collateral: &CapturedCollateral,
) -> Result<VerifiedEvidence, String> {
    let evidence = DcapEvidenceV1 {
        intent: capture.intent.clone(),
        quote: capture.quote.clone(),
        components: collateral.components.clone(),
        transition_key_ready_proof: None,
    };
    let evidence_bytes = AttestationEvidenceV1::Dcap(evidence.clone())
        .encode_canonical()
        .map_err(|error| format!("encode valid AttestationEvidenceV1: {error}"))?;
    let verdict =
        verify_dcap_evidence(&evidence, &capture.policy, capture.timestamp).map_err(|code| {
            format!(
                "valid evidence rejected with stable code 0x{:04x}",
                code.code()
            )
        })?;
    let verdict_bytes = verdict
        .encode_canonical()
        .map_err(|code| format!("encode stable verdict 0x{:04x}", code.code()))?;

    let mut tampered = evidence;
    let signature_byte = tampered
        .quote
        .get_mut(436)
        .ok_or_else(|| "captured quote has no ECDSA signature byte".to_owned())?;
    *signature_byte ^= 1;
    let tampered_evidence_bytes = AttestationEvidenceV1::Dcap(tampered.clone())
        .encode_canonical()
        .map_err(|error| format!("encode tampered AttestationEvidenceV1: {error}"))?;
    let tampered_reject = verify_dcap_evidence(&tampered, &capture.policy, capture.timestamp)
        .expect_err("tampered quote unexpectedly passed public verifier");
    if tampered_reject != DcapRejectCodeV1::NativeVerificationFailed {
        return Err(format!(
            "tampered quote returned 0x{:04x}, expected NativeVerificationFailed",
            tampered_reject.code()
        ));
    }
    let reject_bytes = tampered_reject.code().to_be_bytes();
    Ok(VerifiedEvidence {
        evidence_bytes,
        verdict,
        verdict_bytes,
        tampered_evidence_bytes,
        tampered_reject,
        reject_bytes,
    })
}

fn write_fixture(
    capture: &CaptureInputs,
    collateral: &CapturedCollateral,
    verified: &VerifiedEvidence,
) -> Result<(), String> {
    ensure_empty_directory(&capture.output_dir)?;
    let mut artifacts = BTreeMap::new();
    write_artifact(
        &capture.output_dir,
        "policy-v1.bin",
        &capture.policy_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &capture.output_dir,
        "intent-v1.bin",
        &capture.intent_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &capture.output_dir,
        "quote-v3.bin",
        &capture.quote,
        &mut artifacts,
    )?;
    for (name, bytes) in &collateral.files {
        write_artifact(&capture.output_dir, name, bytes, &mut artifacts)?;
    }
    write_artifact(
        &capture.output_dir,
        "capture-provenance.json",
        &collateral.provenance,
        &mut artifacts,
    )?;
    write_artifact(
        &capture.output_dir,
        "evidence-valid-v1.bin",
        &verified.evidence_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &capture.output_dir,
        "verdict-valid-v1.bin",
        &verified.verdict_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &capture.output_dir,
        "evidence-tampered-quote-v1.bin",
        &verified.tampered_evidence_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &capture.output_dir,
        "reject-tampered-quote-v1.bin",
        &verified.reject_bytes,
        &mut artifacts,
    )?;

    write_fixture_manifest(capture, verified, artifacts)
}

fn write_fixture_manifest(
    capture: &CaptureInputs,
    verified: &VerifiedEvidence,
    artifacts: BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "capture_timestamp": capture.timestamp,
        "intent_report_data": hex::encode(capture.expected_report_data),
        "platform_tcb_status": verified.verdict.platform_tcb_status as u8,
        "pck_ca": verified.verdict.pck_ca as u8,
        "fmspc": hex::encode(verified.verdict.fmspc),
        "pce_id": verified.verdict.pce_id,
        "tcb_evaluation_data_number": verified.verdict.tcb_evaluation_data_number,
        "qe_tcb_evaluation_data_number": verified.verdict.qe_tcb_evaluation_data_number,
        "collateral_valid_until": verified.verdict.collateral_valid_until,
        "advisory_ids": &verified.verdict.advisory_ids,
        "negative_reject_code": verified.tampered_reject.code(),
        "artifacts": artifacts,
    }))
    .map_err(|error| format!("encode fixture manifest: {error}"))?;
    let mut manifest_with_newline = manifest;
    manifest_with_newline.push(b'\n');
    write_new(
        &capture.output_dir.join("fixture-manifest-v1.json"),
        &manifest_with_newline,
    )
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn write_artifact(
    output_dir: &Path,
    name: &str,
    value: &[u8],
    artifacts: &mut BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    write_new(&output_dir.join(name), value)?;
    artifacts.insert(
        name.to_owned(),
        serde_json::json!({
            "size": value.len(),
            "sha256": hex::encode(Sha256::digest(value)),
        }),
    );
    Ok(())
}
