use super::canonical_json;
use super::file_digest;
use super::is_lower_hex;
use super::read_canonical_json;
use super::require_nonempty_regular_file;
use super::BundleManifest;
use super::OciBuildEvidence;
use super::SgxReleaseNetwork;
use super::VerifiedReleaseInputs;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;

use outbe_evm::tee_attestation_activation::DcapChainSpecBindingV1;
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;

use outbe_primitives::tee_genesis_v1::tee_attestation_v1_genesis_field;

use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;
use std::fs;

use std::path::Path;

#[path = "evidence_archive.rs"]
mod archive;
pub(super) use archive::verify_processor_dcap_archive;

#[path = "evidence_collateral.rs"]
mod collateral;

pub(super) fn require_seeded_genesis_release_evidence(
    inputs: &VerifiedReleaseInputs,
    bundle: &BundleManifest,
    chain_binding: &DcapChainSpecBindingV1,
) -> Result<()> {
    require_nonempty_regular_file(&inputs.seeded_genesis, "approved seeded release genesis")?;
    let seeded = DcapSeededChainSpecBindingV1::from_genesis_path(&inputs.seeded_genesis)
        .map_err(|error| eyre!("seeded release ChainSpec binding is invalid: {error}"))?;
    if seeded.chain_id != inputs.network.chain_id() {
        bail!("seeded release genesis belongs to a foreign network");
    }
    let final_seeded = DcapSeededChainSpecBindingV1::from_genesis_path(&inputs.genesis)
        .map_err(|error| eyre!("final seeded release ChainSpec binding is invalid: {error}"))?;
    if final_seeded != seeded
        || seeded.chain_id != chain_binding.chain_id
        || seeded.genesis_hash != chain_binding.genesis_hash
    {
        bail!("final genesis changed the approved seeded chain identity or epoch-0 committee");
    }

    let seeded_bytes = fs::read(&inputs.seeded_genesis).wrap_err_with(|| {
        format!(
            "read approved seeded release genesis: {}",
            inputs.seeded_genesis.display()
        )
    })?;
    let mut expected_final: Value =
        serde_json::from_slice(&seeded_bytes).wrap_err("parse approved seeded genesis JSON")?;
    let config = expected_final
        .get_mut("config")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("seeded genesis config must be a JSON object"))?;
    if config.contains_key("teeAttestationV1") {
        bail!("seeded genesis already contains teeAttestationV1");
    }
    config.insert(
        "teeAttestationV1".to_owned(),
        tee_attestation_v1_genesis_field(&chain_binding.policy).map_err(eyre::Report::msg)?,
    );
    let expected_final = canonical_json(&expected_final)?;
    let actual_final = fs::read(&inputs.genesis)
        .wrap_err_with(|| format!("read final release genesis: {}", inputs.genesis.display()))?;
    if actual_final != expected_final {
        bail!("final genesis is not the exact allowed seeded-genesis policy insertion");
    }

    let evidence = require_evidence_result(&inputs.network_binding_evidence, &["passed"])?;
    let expected_evidence = serde_json::json!({
        "schema": "outbe-sgx-final-genesis-evidence-v1",
        "network": inputs.network.authorization_scope(),
        "chain_id": chain_binding.chain_id,
        "genesis_hash": format!("{:#x}", chain_binding.genesis_hash),
        "seeded_genesis": file_digest(&inputs.seeded_genesis)?,
        "final_genesis": file_digest(&inputs.genesis)?,
        "bundle_manifest": file_digest(&inputs.bundle.join(inputs.network.bundle_manifest_path()))?,
        "measured_descriptor": file_digest(&inputs.bundle.join("metadata/network-descriptor-v1.bin"))?,
        "measurements": bundle.measurements,
        "minimum_tcb_evaluation_data_number": chain_binding.policy.minimum_tcb_evaluation_data_number,
        "mutation": "insert-config-teeAttestationV1-only",
        "result": "passed"
    });
    if evidence != expected_evidence {
        bail!("network-binding evidence does not exactly bind the seed, final genesis and signed bundle");
    }
    Ok(())
}

pub(super) fn require_bundle_network(
    bundle: &BundleManifest,
    network: SgxReleaseNetwork,
) -> Result<()> {
    if bundle.authorization_scope != network.authorization_scope()
        || bundle.chain_id != network.chain_id()
        || bundle.network != network.authorization_scope()
        || bundle.network_name != network.chain_name()
    {
        bail!("signed SGX bundle belongs to a foreign release network");
    }
    Ok(())
}

pub(super) fn require_fresh_dcap_hardware_evidence(
    path: &Path,
    expected_pck_ca: &str,
    bundle: &BundleManifest,
    oci: &OciBuildEvidence,
    chain: (&DcapChainSpecBindingV1, &Path, SgxReleaseNetwork),
) -> Result<Value> {
    let (chain_binding, genesis, network) = chain;
    let evidence = require_evidence_result(path, &["passed"])?;
    let string_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("{expected_pck_ca} DCAP evidence lacks {pointer}"))
    };
    let u64_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("{expected_pck_ca} DCAP evidence lacks {pointer}"))
    };
    require_hardware_release_binding(&evidence, expected_pck_ca, bundle, oci)?;
    require_chain_spec_evidence(&evidence, chain_binding, genesis, network)
        .wrap_err("release ChainSpec binding does not match retained DCAP evidence")?;
    // Guest-visible socket topology is retained as provenance only. The
    // enclave-verified Intel PCK issuer above is the PCK CA authority.
    let _ = u64_at("/environment/physical_package_count")?;
    if evidence
        .pointer("/freshness/binding_id_nonzero")
        .and_then(Value::as_bool)
        != Some(true)
    {
        bail!("{expected_pck_ca} DCAP evidence did not use a non-zero one-use binding");
    }
    let run_started = u64_at("/freshness/run_started_at")?;
    let quote_generated = u64_at("/freshness/quote_generated_at")?;
    let collateral_started = u64_at("/freshness/collateral_started_at")?;
    let collateral_completed = u64_at("/freshness/collateral_completed_at")?;
    let consensus_timestamp = u64_at("/freshness/consensus_timestamp")?;
    let verified = u64_at("/freshness/verified_at")?;
    let timestamps = [
        run_started,
        quote_generated,
        collateral_started,
        collateral_completed,
        consensus_timestamp,
        verified,
    ];
    if run_started == 0 || !timestamps.windows(2).all(|pair| pair[0] <= pair[1]) {
        bail!("{expected_pck_ca} DCAP freshness order is invalid");
    }
    let platform_status = string_at("/attestation/platform_tcb_status")?;
    if !matches!(
        platform_status,
        "up-to-date" | "sw-hardening-needed" | "configuration-and-sw-hardening-needed"
    ) || u64_at("/attestation/collateral_valid_until")? <= consensus_timestamp
    {
        bail!("{expected_pck_ca} DCAP accepted verdict provenance is invalid");
    }
    collateral::require_collateral(&evidence, expected_pck_ca, consensus_timestamp)?;
    Ok(evidence)
}

fn require_hardware_release_binding(
    evidence: &Value,
    expected_pck_ca: &str,
    bundle: &BundleManifest,
    oci: &OciBuildEvidence,
) -> Result<()> {
    let string_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("{expected_pck_ca} DCAP evidence lacks {pointer}"))
    };
    let environment_mismatch = string_at("/environment/architecture")? != "x86_64"
        || string_at("/environment/backend")? != "gramine-sgx"
        || evidence
            .pointer("/environment/hardware_sgx")
            .and_then(Value::as_bool)
            != Some(true)
        || evidence
            .pointer("/environment/dcap")
            .and_then(Value::as_bool)
            != Some(true);
    if environment_mismatch
        || string_at("/attestation/pck_ca")? != expected_pck_ca
        || string_at("/attestation/public_verifier")? != "enclave-resident-begin-chunk-finish-v1"
    {
        bail!(
            "{expected_pck_ca} DCAP evidence does not bind the exact release and public verifier"
        );
    }
    if evidence.get("measurements") != Some(&serde_json::to_value(&bundle.measurements)?)
        || string_at("/image/digest/algorithm")? != "sha256"
        || string_at("/image/digest/value")? != oci.image.digest.value
        || string_at("/source_commit")? != bundle.source.commit
    {
        bail!(
            "{expected_pck_ca} DCAP evidence does not bind the exact release and public verifier"
        );
    }
    Ok(())
}

pub(super) fn require_bundle_measurement_binding(
    binding: &DcapChainSpecBindingV1,
    bundle: &BundleManifest,
) -> Result<()> {
    if bundle.measurements.debug {
        bail!("release ChainSpec binding cannot authorize a debug enclave");
    }
    let measurement = |value: &str, label: &str| -> Result<alloy_primitives::B256> {
        if !is_lower_hex(value, 64) {
            bail!("signed bundle {label} is not 32 lowercase hexadecimal bytes");
        }
        let bytes = hex::decode(value).wrap_err_with(|| format!("decode signed bundle {label}"))?;
        Ok(alloy_primitives::B256::from_slice(&bytes))
    };
    binding
        .ensure_exact_release_measurements(
            measurement(&bundle.measurements.mrenclave, "MRENCLAVE")?,
            measurement(&bundle.measurements.mrsigner, "MRSIGNER")?,
            bundle.measurements.isv_prod_id,
            bundle.measurements.isv_svn,
        )
        .map_err(|error| eyre!("release ChainSpec binding is invalid: {error}"))
}

fn require_chain_spec_evidence(
    evidence: &Value,
    binding: &DcapChainSpecBindingV1,
    genesis: &Path,
    network: SgxReleaseNetwork,
) -> Result<()> {
    let string_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("DCAP evidence lacks {pointer}"))
    };
    let u64_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("DCAP evidence lacks {pointer}"))
    };
    let policy_sha256 = hex::encode(Sha256::digest(&binding.policy_bytes));
    let schedule_sha256 = hex::encode(Sha256::digest(&binding.policy_schedule_bytes));
    if u64_at("/policy/chain_id")? != binding.chain_id
        || string_at("/policy/genesis_hash")? != hex::encode(binding.genesis_hash)
        || u64_at("/policy/activation_height")? != binding.activation_height
        || u64_at("/policy/policy_version")? != binding.policy_version
    {
        bail!("DCAP evidence policy does not equal the block-1 release policy");
    }
    if string_at("/policy/policy_hash")? != hex::encode(binding.policy_hash)
        || string_at("/policy/sha256")? != policy_sha256
        || string_at("/policy/policy_schedule_hash")? != hex::encode(binding.policy_schedule_hash)
        || string_at("/policy/policy_schedule_sha256")? != schedule_sha256
    {
        bail!("DCAP evidence policy does not equal the block-1 release policy");
    }

    let genesis_digest = file_digest(genesis)?;
    let genesis_size = fs::metadata(genesis)?.len();
    require_retained_binding(
        evidence,
        network.genesis_artifact_name(),
        genesis_size,
        &genesis_digest.value,
    )?;
    require_retained_binding(
        evidence,
        "policy-v1.bin",
        binding.policy_bytes.len() as u64,
        &policy_sha256,
    )?;
    require_retained_binding(
        evidence,
        "policy-schedule-v1.bin",
        binding.policy_schedule_bytes.len() as u64,
        &schedule_sha256,
    )
}

fn require_retained_binding(
    evidence: &Value,
    name: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<()> {
    let pointer = format!("/artifacts/{name}");
    let artifact = evidence
        .pointer(&pointer)
        .ok_or_else(|| eyre!("DCAP evidence lacks retained {name}"))?;
    if artifact.get("size").and_then(Value::as_u64) != Some(expected_size)
        || artifact.get("sha256").and_then(Value::as_str) != Some(expected_sha256)
    {
        bail!("retained {name} does not match its exact input bytes");
    }
    Ok(())
}

pub(super) fn require_evidence_result(path: &Path, allowed: &[&str]) -> Result<Value> {
    require_nonempty_regular_file(path, "release evidence")?;
    let value: Value = read_canonical_json(path)?;
    let result = value
        .get("result")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("release evidence lacks result: {}", path.display()))?;
    if !allowed.contains(&result) {
        bail!("release evidence is not successful: {}", path.display());
    }
    Ok(value)
}

pub(super) fn passed_gate(name: &str, evidence: &Path) -> Result<Value> {
    passed_gate_many(name, &[evidence])
}

pub(super) fn passed_gate_many(name: &str, evidence: &[&Path]) -> Result<Value> {
    let evidence = evidence
        .iter()
        .map(|path| {
            require_nonempty_regular_file(path, "release evidence")?;
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| eyre!("release evidence needs a UTF-8 file name"))?;
            let media_type = if path.extension().and_then(|value| value.to_str()) == Some("tar") {
                "application/x-tar"
            } else {
                "application/json"
            };
            Ok(serde_json::json!({
                "digest": file_digest(path)?,
                "media_type": media_type,
                "uri": format!("release://evidence/{file_name}")
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({
        "evidence": evidence,
        "name": name,
        "status": "passed"
    }))
}
