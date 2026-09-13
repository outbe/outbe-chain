use super::file_artifact;
use super::file_digest;

use super::passed_gate;
use super::passed_gate_many;
use super::read_canonical_json;
use super::require_bundle_measurement_binding;
use super::require_bundle_network;
use super::require_evidence_result;
use super::require_fresh_dcap_hardware_evidence;
use super::require_measured_network_descriptor;
use super::require_nonempty_regular_file;
use super::require_seeded_genesis_release_evidence;

use super::verified_cosign_attestation;
use super::verify_bundle_archive;
use super::verify_cosign_image_signature;
use super::verify_processor_dcap_archive;
use super::SgxReleaseNetwork;
use super::GITHUB_ACTIONS_OIDC_ISSUER;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;

use outbe_evm::tee_attestation_activation::DcapChainSpecBindingV1;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use std::collections::BTreeMap;

use std::fs;

use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GramineIdentity {
    pub builder_image: String,
    pub source_commit: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SourceIdentity {
    pub release_tag: String,
    pub source_commit: String,
    pub source_date_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Measurements {
    pub debug: bool,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub mrenclave: String,
    pub mrsigner: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Sha256Digest {
    pub algorithm: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BundleFile {
    pub digest: Sha256Digest,
    pub mode: String,
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ManifestSource {
    pub commit: String,
    pub source_date_epoch: i64,
    pub tag: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BundleManifest {
    pub authorization_scope: String,
    pub bundle_version: u32,
    pub chain_id: u64,
    pub files: Vec<BundleFile>,
    pub gramine: GramineIdentity,
    pub install_root: String,
    pub measurements: Measurements,
    pub network: String,
    pub network_name: String,
    pub platform: String,
    pub schema_version: String,
    pub sealed_state_schema: u32,
    pub sigstruct_date: String,
    pub source: ManifestSource,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct OciDescriptor {
    pub digest: Sha256Digest,
    pub media_type: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct OciBuildEvidence {
    pub bundle_manifest_digest: Sha256Digest,
    pub image: OciDescriptor,
    pub image_reference: String,
    pub measurements: Measurements,
    pub platform: String,
    pub provenance_attestation: bool,
    pub sbom_attestation: bool,
    pub schema_version: String,
    pub source: ManifestSource,
}

#[derive(Clone, Debug)]
pub struct VerifiedReleaseInputs {
    pub network: SgxReleaseNetwork,
    pub bundle: PathBuf,
    pub bundle_archive: PathBuf,
    pub cosign_image_verification: PathBuf,
    pub cosign_provenance_verification: PathBuf,
    pub cosign_sbom_verification: PathBuf,
    pub elf_evidence: PathBuf,
    pub elf_manifest: PathBuf,
    pub hardware_evidence: PathBuf,
    pub processor_dcap_archive: PathBuf,
    pub processor_dcap_evidence: PathBuf,
    pub oci_evidence: PathBuf,
    pub sbom: PathBuf,
    pub sgx_evidence: PathBuf,
    pub seeded_genesis: PathBuf,
    pub network_binding_evidence: PathBuf,
    pub genesis: PathBuf,
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).wrap_err("serialize canonical JSON value")?;
    let value = sort_json(value);
    let mut encoded = serde_json::to_vec(&value).wrap_err("encode canonical JSON")?;
    encoded.push(b'\n');
    Ok(encoded)
}

/// Build a structurally validated candidate for tests and diagnostics.
///
/// This function never emits the terminal `verified` lifecycle. Only
/// [`super::finalize_release_manifest`] can do that after it invokes Cosign itself.
pub fn build_release_manifest_candidate(inputs: &VerifiedReleaseInputs) -> Result<Value> {
    build_release_manifest_from_evidence(inputs, "build-candidate")
}

pub(super) fn build_release_manifest_from_evidence(
    inputs: &VerifiedReleaseInputs,
    lifecycle: &str,
) -> Result<Value> {
    let network = inputs.network;
    let mut release: Value = read_canonical_json(&inputs.elf_manifest)?;
    let bundle_manifest_path = inputs.bundle.join(network.bundle_manifest_path());
    let bundle: BundleManifest = read_canonical_json(&bundle_manifest_path)?;
    require_bundle_network(&bundle, network)?;
    let oci: OciBuildEvidence = read_canonical_json(&inputs.oci_evidence)?;
    validate_final_release_identity(&release, &bundle, &oci)?;
    require_nonempty_regular_file(&inputs.genesis, "release genesis ChainSpec")?;
    let chain_binding = DcapChainSpecBindingV1::from_genesis_path(&inputs.genesis)
        .map_err(|error| eyre!("release ChainSpec binding is invalid: {error}"))?;
    if chain_binding.chain_id != network.chain_id() {
        bail!("release genesis belongs to a foreign network");
    }
    require_measured_network_descriptor(&inputs.bundle, &inputs.genesis, &chain_binding)?;
    require_bundle_measurement_binding(&chain_binding, &bundle)?;
    require_seeded_genesis_release_evidence(inputs, &bundle, &chain_binding)?;

    if !oci.provenance_attestation || !oci.sbom_attestation {
        bail!("OCI image must carry BuildKit provenance and SBOM attestations");
    }
    if oci.bundle_manifest_digest != file_digest(&bundle_manifest_path)? {
        bail!("OCI evidence does not bind the signed SGX bundle manifest");
    }
    verify_cosign_image_signature(&inputs.cosign_image_verification, &oci.image.digest.value)?;
    require_evidence_result(&inputs.elf_evidence, &["passed"])?;
    require_evidence_result(&inputs.sgx_evidence, &["identical"])?;
    let hardware: Value = require_evidence_result(&inputs.hardware_evidence, &["passed"])?;
    if hardware
        .pointer("/environment/backend")
        .and_then(Value::as_str)
        != Some("gramine-sgx")
        || hardware
            .pointer("/environment/hardware_sgx")
            .and_then(Value::as_bool)
            != Some(true)
        || hardware.get("measurements") != Some(&serde_json::to_value(&bundle.measurements)?)
        || hardware
            .pointer("/image/digest/value")
            .and_then(Value::as_str)
            != Some(oci.image.digest.value.as_str())
    {
        bail!("hardware SGX evidence does not bind the release image and measurements");
    }
    let processor_dcap = require_fresh_dcap_hardware_evidence(
        &inputs.processor_dcap_evidence,
        "processor",
        &bundle,
        &oci,
        &chain_binding,
        &inputs.genesis,
        network,
    )?;
    verify_processor_dcap_archive(
        &inputs.processor_dcap_archive,
        &inputs.processor_dcap_evidence,
        &processor_dcap,
        network.dcap_artifact_set(),
        bundle.source.source_date_epoch,
    )?;
    require_nonempty_regular_file(&inputs.bundle_archive, "signed SGX bundle archive")?;
    verify_bundle_archive(
        &inputs.bundle,
        &inputs.bundle_archive,
        bundle.source.source_date_epoch,
    )?;
    require_nonempty_regular_file(&inputs.sbom, "SPDX SBOM")?;
    let sbom: Value =
        serde_json::from_slice(&fs::read(&inputs.sbom)?).wrap_err("parse SPDX SBOM")?;
    if sbom.get("spdxVersion").and_then(Value::as_str) != Some("SPDX-2.3") {
        bail!("release SBOM must use SPDX-2.3");
    }
    let attested_sbom = verified_cosign_attestation(
        &inputs.cosign_sbom_verification,
        &oci.image.digest.value,
        "https://spdx.dev/Document",
    )?;
    if attested_sbom.get("predicate") != Some(&sbom) {
        bail!("attested SBOM does not match the exact release SBOM");
    }
    let attested_provenance = verified_cosign_attestation(
        &inputs.cosign_provenance_verification,
        &oci.image.digest.value,
        "https://slsa.dev/provenance/v0.2",
    )?;
    let predicate = attested_provenance
        .get("predicate")
        .ok_or_else(|| eyre!("verified provenance attestation lacks a predicate"))?;
    if predicate.get("buildType").and_then(Value::as_str)
        != Some("https://mobyproject.org/buildkit@v1")
        || predicate
            .get("materials")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        bail!("verified provenance is not a material-bearing BuildKit statement");
    }

    let release_object = release
        .as_object_mut()
        .ok_or_else(|| eyre!("ELF release manifest must be a JSON object"))?;
    release_object
        .get_mut("release")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("ELF release manifest lacks release metadata"))?
        .insert("lifecycle".to_owned(), Value::String(lifecycle.to_owned()));
    let provenance = release_object
        .get_mut("build")
        .and_then(Value::as_object_mut)
        .and_then(|build| build.get_mut("provenance"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("ELF release manifest lacks build provenance"))?;
    provenance.insert(
        "mode".to_owned(),
        Value::String("github-actions".to_owned()),
    );
    provenance.insert(
        "workflow".to_owned(),
        Value::String(network.workflow_path().to_owned()),
    );
    provenance.insert(
        "certificate_identity".to_owned(),
        Value::String(network.certificate_identity().to_owned()),
    );
    provenance.insert(
        "certificate_oidc_issuer".to_owned(),
        Value::String(GITHUB_ACTIONS_OIDC_ISSUER.to_owned()),
    );
    provenance.insert(
        "certificate_workflow_sha".to_owned(),
        Value::String(bundle.source.commit.clone()),
    );
    release_object.insert(
        "network".to_owned(),
        serde_json::json!({
            "chain_id": network.chain_id(),
            "chain_name": network.chain_name(),
            "genesis_hash": format!("{:#x}", chain_binding.genesis_hash),
            "genesis_file": {
                "path": network.genesis_artifact_name(),
                "digest": file_digest(&inputs.genesis)?,
                "size": fs::metadata(&inputs.genesis)?.len()
            }
        }),
    );

    let artifacts = release_object
        .get_mut("artifacts")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| eyre!("ELF release manifest lacks artifacts"))?;
    let tee = signed_tee_metadata(&bundle);
    artifacts.push(file_artifact(
        &inputs.bundle_archive,
        "outbe-tee-enclave-sgx-bundle",
        "archive",
        "application/x-tar",
        tee.clone(),
    )?);
    artifacts.push(serde_json::json!({
        "classification": "production",
        "digest": oci.image.digest,
        "features": [],
        "install_profiles": ["full-node", "validator"],
        "kind": "oci-manifest",
        "media_type": oci.image.media_type,
        "name": format!("{}-oci", network.oci_name()),
        "network_compatibility": "network-manifest-required",
        "package": "outbe-tee-enclave",
        "path": format!("oci/{}@sha256:{}", network.oci_name(), oci.image.digest.value),
        "platform": release_platform(),
        "role": "tee-enclave",
        "size": oci.image.size,
        "tee": tee.clone()
    }));
    artifacts.push(file_artifact(
        &inputs.sbom,
        "outbe-tee-enclave-sbom",
        "sbom",
        "application/spdx+json",
        tee,
    )?);

    let gates = vec![
        passed_gate(
            "independent-byte-for-byte-elf-rebuild",
            &inputs.elf_evidence,
        )?,
        passed_gate(
            "release-manifest-schema-and-canonicalization",
            &inputs.elf_manifest,
        )?,
        passed_gate("independent-unsigned-sgx-bundle", &inputs.sgx_evidence)?,
        passed_gate("signed-sgx-sigstruct-verification", &bundle_manifest_path)?,
        passed_gate_many(
            "seeded-genesis-to-signed-enclave-policy",
            &[&inputs.seeded_genesis, &inputs.network_binding_evidence],
        )?,
        passed_gate_many(
            "immutable-oci-sbom-and-provenance",
            &[
                &inputs.oci_evidence,
                &inputs.cosign_image_verification,
                &inputs.cosign_sbom_verification,
                &inputs.cosign_provenance_verification,
            ],
        )?,
        passed_gate("hardware-sgx-release-smoke", &inputs.hardware_evidence)?,
        passed_gate_many(
            "fresh-accepted-processor-dcap",
            &[
                &inputs.processor_dcap_evidence,
                &inputs.processor_dcap_archive,
            ],
        )?,
    ];
    release_object.insert("verification_gates".to_owned(), Value::Array(gates));
    Ok(release)
}

fn validate_final_release_identity(
    release: &Value,
    bundle: &BundleManifest,
    oci: &OciBuildEvidence,
) -> Result<()> {
    let commit = release
        .pointer("/release/source/commit")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF release manifest lacks source commit"))?;
    let tag = release
        .pointer("/release/tag")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF release manifest lacks release tag"))?;
    let epoch = release
        .pointer("/build/source_date_epoch")
        .and_then(Value::as_i64)
        .ok_or_else(|| eyre!("ELF release manifest lacks SOURCE_DATE_EPOCH"))?;
    if commit != bundle.source.commit
        || tag != bundle.source.tag
        || epoch != bundle.source.source_date_epoch
        || bundle.source != oci.source
        || bundle.measurements != oci.measurements
        || oci.platform != "linux/amd64"
    {
        bail!("ELF, SGX bundle and OCI evidence do not share one release identity");
    }
    Ok(())
}

fn signed_tee_metadata(bundle: &BundleManifest) -> Value {
    serde_json::json!({
        "authorization_scope": bundle.authorization_scope,
        "isv_prod_id": bundle.measurements.isv_prod_id,
        "isv_svn": bundle.measurements.isv_svn,
        "mock": false,
        "mrenclave": bundle.measurements.mrenclave,
        "mrsigner": bundle.measurements.mrsigner,
        "sealed_state_schema": bundle.sealed_state_schema,
        "stage": "signed"
    })
}

pub(super) fn release_platform() -> Value {
    serde_json::json!({
        "architecture": "x86_64",
        "os": "linux",
        "target": "x86_64-unknown-linux-gnu"
    })
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        Value::Object(values) => {
            let sorted = values
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar,
    }
}
