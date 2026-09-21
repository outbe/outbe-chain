use super::absolute_path;
use super::build_bundle_manifest;
use super::build_project_toolchain_image;
use super::build_release_manifest_from_evidence;
use super::canonical_json;
use super::compare_unsigned_trees;
use super::container_adapter;
use super::create_empty_output;
use super::docker_command;
use super::file_digest;
use super::is_lower_hex;
use super::normalize_tree_mtime;
use super::parse_oci_descriptor;
use super::read_canonical_json;
use super::read_elf_identity;
use super::read_measured_network_descriptor;
use super::require_bundle_measurement_binding;
use super::require_bundle_network;
use super::require_clean_source;
use super::require_measured_network_descriptor;
use super::require_release_checkout;
use super::run_output;
use super::run_status;
use super::sigstruct_date;
use super::validate_signing_key;
use super::validate_source_identity;
use super::verify_bundle_archive;
use super::verify_checksums;
use super::verify_signed_bundle;
use super::write_canonical;
use super::write_checksums;
use super::write_deterministic_bundle_archive;
use super::write_new_file;
use super::BundleManifest;
use super::BundleSpec;
use super::OciBuildEvidence;
use super::SgxReleaseNetwork;
use super::SourceIdentity;
use super::VerifiedReleaseInputs;
use super::GITHUB_ACTIONS_OIDC_ISSUER;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;

use outbe_evm::tee_attestation_activation::DcapChainSpecBindingV1;
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;

use outbe_primitives::tee_attestation_v1::AttestationMode;
use outbe_primitives::tee_attestation_v1::NetworkBindingV1;
use outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1;
use outbe_primitives::tee_genesis_v1::initial_tee_policy_v1;
use outbe_primitives::tee_genesis_v1::tee_attestation_v1_genesis_field;
use outbe_primitives::tee_genesis_v1::InitialTeeProfileV1;
use outbe_primitives::tee_genesis_v1::ProductionSgxMeasurementV1;

use serde_json::Value;

use std::fs;

use std::path::Path;

use std::process::Command;

pub fn prepare(
    repo_root: &Path,
    network: SgxReleaseNetwork,
    genesis: &Path,
    elf_output: &Path,
    output: &Path,
) -> Result<()> {
    let spec = BundleSpec::read(&repo_root.join(network.bundle_spec_path()))?;
    require_release_checkout(repo_root, network)?;
    verify_checksums(elf_output, "SHA256SUMS")?;
    let identity = read_elf_identity(elf_output)?;
    require_clean_source(repo_root, &identity.source_commit)?;
    let toolchain_image = build_project_toolchain_image(repo_root, &spec, &identity.source_commit)?;
    let output = create_empty_output(repo_root, output)?;
    let genesis = fs::canonicalize(genesis)
        .wrap_err_with(|| format!("resolve release genesis ChainSpec: {}", genesis.display()))?;
    let seeded = DcapSeededChainSpecBindingV1::from_genesis_path(&genesis)
        .map_err(|error| eyre!("release seeded ChainSpec binding is invalid: {error}"))?;
    if seeded.chain_id != network.chain_id() {
        bail!("release genesis belongs to a foreign SGX network");
    }
    let trusted_network_descriptor = TrustedNetworkDescriptorV1 {
        network_binding: NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(seeded.chain_id).to_be_bytes(),
            genesis_hash: seeded.genesis_hash,
            attestation_mode: AttestationMode::DcapRequired,
        },
        genesis_consensus_keys: seeded.genesis_consensus_keys,
    }
    .encode_canonical()
    .map_err(|error| eyre!("encode trusted network descriptor: {error}"))?;
    let descriptor_path = output.join("metadata/network-descriptor-v1.bin");
    fs::create_dir_all(
        descriptor_path
            .parent()
            .expect("network descriptor path has a parent"),
    )?;
    fs::write(&descriptor_path, trusted_network_descriptor)
        .wrap_err("write trusted network descriptor")?;
    let elf_output = fs::canonicalize(elf_output)
        .wrap_err_with(|| format!("resolve ELF output: {}", elf_output.display()))?;

    let mut command = docker_command(&spec, repo_root)?;
    command
        .args(["-e", &format!("SGX_MAX_THREADS={}", spec.sgx.max_threads)])
        .args(["-e", &format!("SGX_ISV_PROD_ID={}", spec.sgx.isv_prod_id)])
        .args(["-e", &format!("SGX_ISV_SVN={}", spec.sgx.isv_svn)])
        .args(["-v", &format!("{}:/elf:ro", elf_output.display())])
        .args(["-v", &format!("{}:/out", output.display())])
        .arg(&toolchain_image)
        .args([container_adapter(), "prepare"]);
    run_status(&mut command, "prepare unsigned SGX bundle")?;

    write_canonical(&output.join("metadata/source-identity.json"), &identity)?;
    write_checksums(&output, "SHA256SUMS.unsigned")?;
    normalize_tree_mtime(&output, identity.source_date_epoch)?;
    verify_checksums(&output, "SHA256SUMS.unsigned")?;
    Ok(())
}

pub fn compare(first: &Path, second: &Path, output: &Path) -> Result<()> {
    let first = fs::canonicalize(first)
        .wrap_err_with(|| format!("resolve first unsigned bundle: {}", first.display()))?;
    let second = fs::canonicalize(second)
        .wrap_err_with(|| format!("resolve second unsigned bundle: {}", second.display()))?;
    verify_checksums(&first, "SHA256SUMS.unsigned")?;
    verify_checksums(&second, "SHA256SUMS.unsigned")?;
    let output = absolute_path(output)?;
    if output.starts_with(&first) || output.starts_with(&second) {
        bail!("comparison evidence must be outside both input trees");
    }
    if output.exists() {
        bail!("comparison evidence already exists: {}", output.display());
    }
    let evidence = compare_unsigned_trees(&first, &second)?;
    write_canonical(&output, &evidence)
}

pub fn sign(
    repo_root: &Path,
    network: SgxReleaseNetwork,
    unsigned: &Path,
    key_file: &Path,
    output: &Path,
) -> Result<()> {
    let spec = BundleSpec::read(&repo_root.join(network.bundle_spec_path()))?;
    require_release_checkout(repo_root, network)?;
    validate_signing_key(key_file)?;
    let unsigned = fs::canonicalize(unsigned)
        .wrap_err_with(|| format!("resolve unsigned SGX bundle: {}", unsigned.display()))?;
    let key_file = fs::canonicalize(key_file)
        .wrap_err_with(|| format!("resolve SGX signing key: {}", key_file.display()))?;
    verify_checksums(&unsigned, "SHA256SUMS.unsigned")?;
    let identity: SourceIdentity =
        read_canonical_json(&unsigned.join("metadata/source-identity.json"))?;
    validate_source_identity(&identity)?;
    require_clean_source(repo_root, &identity.source_commit)?;
    let toolchain_image = build_project_toolchain_image(repo_root, &spec, &identity.source_commit)?;
    let output = create_empty_output(repo_root, output)?;
    let date = sigstruct_date(identity.source_date_epoch)?;

    let mut command = docker_command(&spec, repo_root)?;
    command
        .args(["-e", &format!("SIGSTRUCT_DATE={date}")])
        .args(["-v", &format!("{}:/unsigned:ro", unsigned.display())])
        .args([
            "-v",
            &format!("{}:/run/secrets/sgx-signing-key.pem:ro", key_file.display()),
        ])
        .args(["-v", &format!("{}:/out", output.display())])
        .arg(&toolchain_image)
        .args([container_adapter(), "sign"]);
    run_status(&mut command, "sign SGX bundle")?;

    let sigstruct_view = fs::read_to_string(output.join("metadata/sigstruct.txt"))
        .wrap_err("read signed bundle SIGSTRUCT evidence")?;
    let manifest = build_bundle_manifest(&output, &spec, &identity, &sigstruct_view)?;
    write_canonical(&output.join(network.bundle_manifest_path()), &manifest)?;
    verify_signed_bundle(&output, &manifest, &spec, &sigstruct_view)?;
    write_checksums(&output, "SHA256SUMS")?;
    normalize_tree_mtime(&output, identity.source_date_epoch)?;
    verify_checksums(&output, "SHA256SUMS")?;
    Ok(())
}

/// Creates the final network genesis from one approved seeded genesis and the
/// exact measurements of an already signed bundle. The only JSON mutation is
/// insertion of `config.teeAttestationV1`; the measured descriptor remains
/// rooted in the unchanged chain identity and epoch-0 committee.
pub fn finalize_genesis(
    repo_root: &Path,
    network: SgxReleaseNetwork,
    seeded_genesis: &Path,
    bundle: &Path,
    output: &Path,
    evidence_output: &Path,
) -> Result<()> {
    if output == evidence_output || output.exists() || evidence_output.exists() {
        bail!("final genesis and evidence outputs must be distinct new paths");
    }
    let spec = BundleSpec::read(&repo_root.join(network.bundle_spec_path()))?;
    require_release_checkout(repo_root, network)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    verify_checksums(&bundle, "SHA256SUMS")?;
    let manifest_path = bundle.join(network.bundle_manifest_path());
    let manifest: BundleManifest = read_canonical_json(&manifest_path)?;
    require_bundle_network(&manifest, network)?;
    require_clean_source(repo_root, &manifest.source.commit)?;

    let seeded = DcapSeededChainSpecBindingV1::from_genesis_path(seeded_genesis)
        .map_err(|error| eyre!("seeded release ChainSpec binding is invalid: {error}"))?;
    if seeded.chain_id != network.chain_id() {
        bail!("seeded release genesis belongs to a foreign network");
    }
    let descriptor = read_measured_network_descriptor(&bundle)?;
    let expected_descriptor = TrustedNetworkDescriptorV1 {
        network_binding: NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(seeded.chain_id).to_be_bytes(),
            genesis_hash: seeded.genesis_hash,
            attestation_mode: AttestationMode::DcapRequired,
        },
        genesis_consensus_keys: seeded.genesis_consensus_keys.clone(),
    };
    if descriptor != expected_descriptor {
        bail!("signed bundle descriptor does not match the approved seeded genesis");
    }

    let measurement = |value: &str, label: &str| -> Result<alloy_primitives::B256> {
        if !is_lower_hex(value, 64) {
            bail!("signed bundle {label} is not 32 lowercase hexadecimal bytes");
        }
        let bytes = hex::decode(value).wrap_err_with(|| format!("decode signed bundle {label}"))?;
        Ok(alloy_primitives::B256::from_slice(&bytes))
    };
    let profile = InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
        mrenclave: measurement(&manifest.measurements.mrenclave, "MRENCLAVE")?,
        mrsigner: measurement(&manifest.measurements.mrsigner, "MRSIGNER")?,
        isv_prod_id: manifest.measurements.isv_prod_id,
        minimum_isv_svn: manifest.measurements.isv_svn,
        minimum_tcb_evaluation_data_number: spec.sgx.minimum_tcb_evaluation_data_number,
    });
    let policy = initial_tee_policy_v1(profile, seeded.chain_id, seeded.genesis_hash)
        .map_err(eyre::Report::msg)?;
    let policy_field = tee_attestation_v1_genesis_field(&policy).map_err(eyre::Report::msg)?;

    let seeded_bytes = fs::read(seeded_genesis)
        .wrap_err_with(|| format!("read approved seeded genesis: {}", seeded_genesis.display()))?;
    let mut final_value: Value =
        serde_json::from_slice(&seeded_bytes).wrap_err("parse approved seeded genesis JSON")?;
    let config = final_value
        .get_mut("config")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("seeded genesis config must be a JSON object"))?;
    if config.contains_key("teeAttestationV1") {
        bail!("seeded genesis already contains teeAttestationV1");
    }
    config.insert("teeAttestationV1".to_owned(), policy_field);
    let final_bytes = canonical_json(&final_value)?;
    write_new_file(output, &final_bytes, "final genesis")?;

    let validate_result = (|| -> Result<DcapChainSpecBindingV1> {
        let final_binding = DcapChainSpecBindingV1::from_genesis_path(output)
            .map_err(|error| eyre!("generated final ChainSpec binding is invalid: {error}"))?;
        if final_binding.chain_id != seeded.chain_id
            || final_binding.genesis_hash != seeded.genesis_hash
        {
            bail!("final genesis changed the seeded chain identity");
        }
        let final_seeded = DcapSeededChainSpecBindingV1::from_genesis_path(output)
            .map_err(|error| eyre!("generated final seeded binding is invalid: {error}"))?;
        if final_seeded != seeded {
            bail!("final genesis changed the seeded epoch-0 committee or chain identity");
        }
        require_measured_network_descriptor(&bundle, output, &final_binding)?;
        require_bundle_measurement_binding(&final_binding, &manifest)?;
        Ok(final_binding)
    })();
    let final_binding = match validate_result {
        Ok(binding) => binding,
        Err(error) => {
            let _ = fs::remove_file(output);
            return Err(error);
        }
    };

    let evidence = serde_json::json!({
        "schema": "outbe-sgx-final-genesis-evidence-v1",
        "network": network.authorization_scope(),
        "chain_id": final_binding.chain_id,
        "genesis_hash": format!("{:#x}", final_binding.genesis_hash),
        "seeded_genesis": file_digest(seeded_genesis)?,
        "final_genesis": file_digest(output)?,
        "bundle_manifest": file_digest(&manifest_path)?,
        "measured_descriptor": file_digest(&bundle.join("metadata/network-descriptor-v1.bin"))?,
        "measurements": manifest.measurements,
        "minimum_tcb_evaluation_data_number": spec.sgx.minimum_tcb_evaluation_data_number,
        "mutation": "insert-config-teeAttestationV1-only",
        "result": "passed"
    });
    if let Err(error) = write_new_file(
        evidence_output,
        &canonical_json(&evidence)?,
        "final genesis evidence",
    ) {
        let _ = fs::remove_file(output);
        return Err(error);
    }
    Ok(())
}

pub fn verify(repo_root: &Path, network: SgxReleaseNetwork, bundle: &Path) -> Result<()> {
    let spec = BundleSpec::read(&repo_root.join(network.bundle_spec_path()))?;
    require_release_checkout(repo_root, network)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    verify_checksums(&bundle, "SHA256SUMS")?;
    let manifest_path = bundle.join(network.bundle_manifest_path());
    let manifest: BundleManifest = read_canonical_json(&manifest_path)?;
    require_clean_source(repo_root, &manifest.source.commit)?;
    let toolchain_image = build_project_toolchain_image(repo_root, &spec, &manifest.source.commit)?;

    let mut command = docker_command(&spec, repo_root)?;
    command
        .args(["-v", &format!("{}:/bundle:ro", bundle.display())])
        .arg(&toolchain_image)
        .args([container_adapter(), "view"]);
    let sigstruct_view = run_output(&mut command, "read signed SGX SIGSTRUCT")?;
    verify_signed_bundle(&bundle, &manifest, &spec, &sigstruct_view)
}

pub fn verify_with_genesis(
    repo_root: &Path,
    network: SgxReleaseNetwork,
    bundle: &Path,
    genesis: &Path,
) -> Result<()> {
    verify(repo_root, network, bundle)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    let manifest: BundleManifest =
        read_canonical_json(&bundle.join(network.bundle_manifest_path()))?;
    let chain_binding = DcapChainSpecBindingV1::from_genesis_path(genesis)
        .map_err(|error| eyre!("final release ChainSpec binding is invalid: {error}"))?;
    if chain_binding.chain_id != network.chain_id() {
        bail!("final release genesis belongs to a foreign network");
    }
    require_measured_network_descriptor(&bundle, genesis, &chain_binding)?;
    require_bundle_measurement_binding(&chain_binding, &manifest)
}

pub fn archive(
    repo_root: &Path,
    network: SgxReleaseNetwork,
    bundle: &Path,
    output: &Path,
) -> Result<()> {
    verify(repo_root, network, bundle)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    let manifest: BundleManifest =
        read_canonical_json(&bundle.join(network.bundle_manifest_path()))?;
    let output = absolute_path(output)?;
    if output.starts_with(repo_root) || output.starts_with(&bundle) {
        bail!("signed SGX archive must be outside the checkout and bundle");
    }
    write_deterministic_bundle_archive(&bundle, &output, manifest.source.source_date_epoch)?;
    verify_bundle_archive(&bundle, &output, manifest.source.source_date_epoch)
}

pub fn build_image(
    repo_root: &Path,
    network: SgxReleaseNetwork,
    bundle: &Path,
    image_reference: &str,
    output: &Path,
    push: bool,
) -> Result<()> {
    if image_reference.is_empty()
        || !image_reference.is_ascii()
        || image_reference.chars().any(char::is_whitespace)
    {
        bail!("OCI image reference must be non-empty ASCII without whitespace");
    }
    let output = absolute_path(output)?;
    if output.exists() {
        bail!("OCI build evidence already exists: {}", output.display());
    }
    verify(repo_root, network, bundle)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    if output.starts_with(&bundle) {
        bail!("OCI build evidence must be outside the signed bundle");
    }
    let manifest_path = bundle.join(network.bundle_manifest_path());
    let manifest: BundleManifest = read_canonical_json(&manifest_path)?;
    let metadata_file = tempfile::NamedTempFile::new().wrap_err("create BuildKit metadata file")?;
    let dockerfile = repo_root.join("bin/outbe-tee-enclave/gramine/Dockerfile");
    let mut command = Command::new("docker");
    command
        .args(["buildx", "build", "--platform", "linux/amd64", "--file"])
        .arg(&dockerfile)
        .args(["--tag", image_reference, "--metadata-file"])
        .arg(metadata_file.path());
    if push {
        command.args([
            "--push",
            "--provenance=mode=max,version=v0.2",
            "--sbom=true",
        ]);
    } else {
        command.args(["--load", "--provenance=false", "--sbom=false"]);
    }
    command.arg(&bundle);
    run_status(&mut command, "build immutable SGX OCI image")?;
    let buildkit_metadata =
        fs::read_to_string(metadata_file.path()).wrap_err("read BuildKit OCI metadata")?;
    let descriptor = parse_oci_descriptor(&buildkit_metadata)?;
    let evidence = OciBuildEvidence {
        bundle_manifest_digest: file_digest(&manifest_path)?,
        image: descriptor,
        image_reference: image_reference.to_owned(),
        measurements: manifest.measurements,
        platform: "linux/amd64".to_owned(),
        provenance_attestation: push,
        sbom_attestation: push,
        schema_version: "1.0.0".to_owned(),
        source: manifest.source,
    };
    write_canonical(&output, &evidence)
}

pub fn finalize_release_manifest(
    repo_root: &Path,
    inputs: &VerifiedReleaseInputs,
    output: &Path,
) -> Result<()> {
    verify(repo_root, inputs.network, &inputs.bundle)?;
    let output = absolute_path(output)?;
    if output.exists() {
        bail!(
            "verified ReleaseManifest already exists: {}",
            output.display()
        );
    }
    refresh_cosign_evidence(inputs)?;
    let manifest = build_release_manifest_from_evidence(inputs, "verified")?;
    write_canonical(&output, &manifest)
}

fn refresh_cosign_evidence(inputs: &VerifiedReleaseInputs) -> Result<()> {
    let oci: OciBuildEvidence = read_canonical_json(&inputs.oci_evidence)?;
    let bundle: BundleManifest =
        read_canonical_json(&inputs.bundle.join(inputs.network.bundle_manifest_path()))?;
    let exact_image = exact_image_reference(&oci)?;
    let workflow_sha = bundle.source.commit.as_str();

    let mut image = Command::new("cosign");
    image
        .args([
            "verify",
            "--certificate-identity",
            inputs.network.certificate_identity(),
            "--certificate-oidc-issuer",
            GITHUB_ACTIONS_OIDC_ISSUER,
            "--certificate-github-workflow-sha",
            workflow_sha,
        ])
        .arg(&exact_image);
    let image_output = run_output(&mut image, "cryptographically verify exact OCI image")?;
    write_canonical(
        &inputs.cosign_image_verification,
        &normalize_cosign_json_output(&image_output, "Cosign image verification")?,
    )?;

    refresh_cosign_attestation(
        inputs.network,
        &exact_image,
        workflow_sha,
        "spdxjson",
        &inputs.cosign_sbom_verification,
    )?;
    refresh_cosign_attestation(
        inputs.network,
        &exact_image,
        workflow_sha,
        "slsaprovenance02",
        &inputs.cosign_provenance_verification,
    )
}

fn refresh_cosign_attestation(
    network: SgxReleaseNetwork,
    exact_image: &str,
    workflow_sha: &str,
    predicate_type: &str,
    output: &Path,
) -> Result<()> {
    let mut command = Command::new("cosign");
    command
        .args([
            "verify-attestation",
            "--type",
            predicate_type,
            "--certificate-identity",
            network.certificate_identity(),
            "--certificate-oidc-issuer",
            GITHUB_ACTIONS_OIDC_ISSUER,
            "--certificate-github-workflow-sha",
            workflow_sha,
        ])
        .arg(exact_image);
    let value = run_output(
        &mut command,
        &format!("cryptographically verify {predicate_type} OCI attestation"),
    )?;
    write_canonical(
        output,
        &normalize_cosign_json_output(&value, "Cosign attestation")?,
    )
}

fn exact_image_reference(oci: &OciBuildEvidence) -> Result<String> {
    if oci.image_reference.contains('@') {
        bail!("OCI build evidence image reference must be a tag before digest promotion");
    }
    let slash = oci.image_reference.rfind('/').unwrap_or(0);
    let colon = oci
        .image_reference
        .rfind(':')
        .filter(|position| *position > slash)
        .ok_or_else(|| eyre!("OCI build evidence image reference lacks a release tag"))?;
    Ok(format!(
        "{}@sha256:{}",
        &oci.image_reference[..colon],
        oci.image.digest.value
    ))
}

pub fn normalize_cosign_json_output(output: &str, label: &str) -> Result<Value> {
    let mut flattened = Vec::new();
    for value in serde_json::Deserializer::from_str(output).into_iter::<Value>() {
        match value.wrap_err_with(|| format!("parse {label} JSON output"))? {
            Value::Array(values) => flattened.extend(values),
            value => flattened.push(value),
        }
    }
    if flattened.is_empty() {
        bail!("{label} emitted no JSON evidence");
    }
    Ok(Value::Array(flattened))
}
