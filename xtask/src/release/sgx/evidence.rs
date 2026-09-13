use super::canonical_json;
use super::file_digest;
use super::is_lower_hex;
use super::read_canonical_json;
use super::require_nonempty_regular_file;
use super::validate_archive_member_path;
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

use outbe_tee::release_dcap_artifacts::ReleaseDcapArtifactSetV1;

use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::fs::File;

use std::io::Read;

use std::path::Path;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

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
    chain_binding: &DcapChainSpecBindingV1,
    genesis: &Path,
    network: SgxReleaseNetwork,
) -> Result<Value> {
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
    if string_at("/environment/architecture")? != "x86_64"
        || string_at("/environment/backend")? != "gramine-sgx"
        || evidence
            .pointer("/environment/hardware_sgx")
            .and_then(Value::as_bool)
            != Some(true)
        || evidence
            .pointer("/environment/dcap")
            .and_then(Value::as_bool)
            != Some(true)
        || string_at("/attestation/pck_ca")? != expected_pck_ca
        || string_at("/attestation/public_verifier")? != "enclave-resident-begin-chunk-finish-v1"
        || evidence.get("measurements") != Some(&serde_json::to_value(&bundle.measurements)?)
        || string_at("/image/digest/algorithm")? != "sha256"
        || string_at("/image/digest/value")? != oci.image.digest.value
        || string_at("/source_commit")? != bundle.source.commit
    {
        bail!(
            "{expected_pck_ca} DCAP evidence does not bind the exact release and public verifier"
        );
    }
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
    if run_started == 0
        || !(run_started <= quote_generated
            && quote_generated <= collateral_started
            && collateral_started <= collateral_completed
            && collateral_completed <= consensus_timestamp
            && consensus_timestamp <= verified)
    {
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
    let consensus_timestamp_i64 = i64::try_from(consensus_timestamp)
        .map_err(|_| eyre!("{expected_pck_ca} consensus timestamp exceeds i64"))?;
    if u64_at("/collateral/component_count")? != 8
        || string_at("/collateral/pck_crl/kind")? != expected_pck_ca
        || string_at("/collateral/root_crl/kind")? != "root"
    {
        bail!("{expected_pck_ca} DCAP evidence lacks the exact collateral matrix");
    }
    for (label, pointer) in [
        ("PCK CRL", "/collateral/pck_crl"),
        ("root CRL", "/collateral/root_crl"),
    ] {
        let issuer = string_at(&format!("{pointer}/issuer"))?;
        let this_update = string_at(&format!("{pointer}/this_update"))?;
        let next_update = string_at(&format!("{pointer}/next_update"))?;
        let size = u64_at(&format!("{pointer}/size"))?;
        let sha256 = string_at(&format!("{pointer}/sha256"))?;
        let retained_path = string_at(&format!("{pointer}/path"))?;
        let expected_path = if pointer.ends_with("pck_crl") {
            "collateral/pck.crl.der"
        } else {
            "collateral/root-ca.crl.der"
        };
        if size == 0
            || !is_lower_hex(sha256, 64)
            || issuer.is_empty()
            || !issuer.is_ascii()
            || !is_utc_second(this_update)
            || !is_utc_second(next_update)
            || this_update >= next_update
        {
            bail!("{expected_pck_ca} {label} provenance is invalid");
        }
        let expected_issuer_marker = if pointer.ends_with("pck_crl") {
            if expected_pck_ca == "processor" {
                "Intel SGX PCK Processor CA"
            } else {
                "Intel SGX PCK Platform CA"
            }
        } else {
            "Intel SGX Root CA"
        };
        if !issuer.contains(expected_issuer_marker) {
            bail!("{expected_pck_ca} {label} provenance has the wrong issuer");
        }
        let this_update_time = OffsetDateTime::parse(this_update, &Rfc3339)
            .wrap_err_with(|| format!("parse {expected_pck_ca} {label} this_update"))?;
        let next_update_time = OffsetDateTime::parse(next_update, &Rfc3339)
            .wrap_err_with(|| format!("parse {expected_pck_ca} {label} next_update"))?;
        if this_update_time.unix_timestamp() > consensus_timestamp_i64
            || consensus_timestamp_i64 >= next_update_time.unix_timestamp()
        {
            bail!("{expected_pck_ca} {label} was not current at the consensus timestamp");
        }
        let artifact = evidence
            .get("artifacts")
            .and_then(Value::as_object)
            .and_then(|artifacts| artifacts.get(expected_path));
        if retained_path != expected_path
            || artifact
                .and_then(|record| record.get("size"))
                .and_then(Value::as_u64)
                != Some(size)
            || artifact
                .and_then(|record| record.get("sha256"))
                .and_then(Value::as_str)
                != Some(sha256)
        {
            bail!("{expected_pck_ca} retained {label} does not match its provenance record");
        }
    }
    Ok(evidence)
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
        || string_at("/policy/policy_hash")? != hex::encode(binding.policy_hash)
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

pub(super) fn verify_processor_dcap_archive(
    archive_path: &Path,
    summary_path: &Path,
    evidence: &Value,
    artifact_set: ReleaseDcapArtifactSetV1,
    source_date_epoch: i64,
) -> Result<()> {
    if source_date_epoch < 0 {
        bail!("Processor DCAP archive SOURCE_DATE_EPOCH must be non-negative");
    }
    require_nonempty_regular_file(archive_path, "Processor DCAP evidence archive")?;

    let records = evidence
        .get("artifacts")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("Processor DCAP evidence lacks retained artifact records"))?;
    let required = artifact_set.paths();
    let declared = records.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if declared != required {
        bail!("Processor DCAP evidence does not declare the exact canonical artifact set");
    }
    let mut expected = BTreeMap::new();
    for (path, record) in records {
        validate_archive_member_path(path)?;
        let size = record
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("Processor DCAP artifact {path} lacks size"))?;
        let sha256 = record
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("Processor DCAP artifact {path} lacks sha256"))?;
        if !is_lower_hex(sha256, 64) {
            bail!("Processor DCAP artifact {path} has invalid sha256");
        }
        if expected
            .insert(path.clone(), (size, sha256.to_owned()))
            .is_some()
        {
            bail!("Processor DCAP evidence contains duplicate artifact {path}");
        }
    }
    let summary_size = fs::metadata(summary_path)?.len();
    let summary_digest = file_digest(summary_path)?;
    if expected
        .insert(
            "hardware-dcap-evidence.json".to_owned(),
            (summary_size, summary_digest.value),
        )
        .is_some()
    {
        bail!("Processor DCAP artifact records contain the reserved summary path");
    }

    let input = File::open(archive_path)
        .wrap_err_with(|| format!("open Processor DCAP archive: {}", archive_path.display()))?;
    let mut archive = tar::Archive::new(input);
    let mut observed = BTreeSet::new();
    let mut previous = None::<String>;
    for item in archive
        .entries()
        .wrap_err("read Processor DCAP evidence archive")?
    {
        let mut item = item.wrap_err("read Processor DCAP evidence archive entry")?;
        let raw_path = item
            .path()
            .wrap_err("read Processor DCAP evidence archive path")?
            .to_string_lossy()
            .into_owned();
        let path = if raw_path == "." {
            if !item.header().entry_type().is_dir() {
                bail!("Processor DCAP archive root entry is not a directory");
            }
            continue;
        } else {
            raw_path
                .strip_prefix("./")
                .unwrap_or(&raw_path)
                .trim_end_matches('/')
                .to_owned()
        };
        validate_archive_member_path(&path)?;
        if previous.as_ref().is_some_and(|value| value >= &path) {
            bail!("Processor DCAP archive members are not in canonical order");
        }
        previous = Some(path.clone());
        if !observed.insert(path.clone()) {
            bail!("Processor DCAP archive contains duplicate path: {path}");
        }
        let header = item.header();
        if header.uid()? != 0 || header.gid()? != 0 || header.mtime()? != source_date_epoch as u64 {
            bail!("Processor DCAP archive has non-deterministic ownership/time: {path}");
        }
        if header.entry_type().is_dir() {
            let prefix = format!("{path}/");
            if !expected.keys().any(|member| member.starts_with(&prefix)) {
                bail!("Processor DCAP archive contains unrecorded directory: {path}");
            }
            continue;
        }
        if !header.entry_type().is_file() {
            bail!("Processor DCAP archive contains non-file entry: {path}");
        }
        let (expected_size, expected_sha256) = expected
            .remove(&path)
            .ok_or_else(|| eyre!("Processor DCAP archive contains unrecorded file: {path}"))?;
        if header.size()? != expected_size {
            bail!("Processor DCAP archive member size mismatch: {path}");
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut size = 0_u64;
        loop {
            let count = item.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            size += count as u64;
        }
        if size != expected_size || hex::encode(hasher.finalize()) != expected_sha256 {
            bail!("Processor DCAP archive member digest mismatch: {path}");
        }
    }
    if !expected.is_empty() {
        bail!(
            "Processor DCAP archive lacks retained files: {}",
            expected.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn is_utc_second(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
        && OffsetDateTime::parse(value, &Rfc3339).is_ok()
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
