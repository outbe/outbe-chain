use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path},
};

use alloy_primitives::U256;
use eyre::{ensure, Context, Result};
use outbe_genesis_cli::{
    attestation::run_tee_stage,
    keys::discover_validators,
    launch_bundle::{self, SIGNED_ENCLAVE_FILES},
};
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, TeePolicyScheduleV1, TrustedNetworkDescriptorV1,
};
use outbe_software_bundle::{sha256_file, verify_and_extract};
use serde::Deserialize;
use serde_json::{json, Value};
use walkdir::WalkDir;

use super::{
    build::{
        build_joining_artifacts, copy_tree, founder_artifacts, genesis_identity, legacy_config,
        BuildArtifacts, BuildState, NetworkManifest, SignedSgxIdentity,
    },
    config::{NetworkConfig, TeePolicy},
};

const MINIMUM_TCB_EVALUATION_DATA_NUMBER: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SgxManifest {
    authorization_scope: String,
    bundle_version: u32,
    chain_id: u64,
    files: Vec<SgxFile>,
    gramine: serde_json::Value,
    install_root: String,
    measurements: Measurements,
    network: String,
    network_name: String,
    platform: String,
    schema_version: String,
    sealed_state_schema: u32,
    sigstruct_date: String,
    source: Source,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SgxFile {
    digest: Digest,
    mode: String,
    path: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Digest {
    algorithm: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Measurements {
    debug: bool,
    isv_prod_id: u16,
    isv_svn: u16,
    mrenclave: String,
    mrsigner: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    commit: String,
    source_date_epoch: i64,
    tag: String,
}

pub fn run(build: &Path, signed_sgx: &Path, output: &Path) -> Result<()> {
    ensure!(
        std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64",
        "network finalize executes the bundled Linux binaries and must run on Linux x86_64"
    );
    ensure!(
        build.is_dir(),
        "network build does not exist: {}",
        build.display()
    );
    ensure!(
        !output.exists(),
        "final network output already exists: {}",
        output.display()
    );

    let config = NetworkConfig::read(&build.join("network.yaml"))?;
    ensure!(
        config.tee.policy == TeePolicy::DcapRequired,
        "network finalize is only valid for tee.policy=dcap-required"
    );
    let mut network = read_network_manifest(build)?;
    ensure!(
        network.state == BuildState::SigningRequired,
        "network build is not waiting for SGX signing"
    );
    verify_build_identity(build, &config, &network)?;

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let software_root = tempfile::Builder::new()
        .prefix("outbe-software-")
        .tempdir_in(parent)?;
    let software_bundle = build.join("software-bundle.tar.zst");
    let software = verify_and_extract(&software_bundle, software_root.path())?;
    ensure!(
        software.commit == network.software.commit,
        "software bundle commit changed after network build"
    );

    let sgx = verify_signed_sgx(signed_sgx, &config, &network.software.commit)?;
    let staging = tempfile::Builder::new()
        .prefix(".outbe-network-final-")
        .tempdir_in(parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    copy_tree(build, staging.path())?;

    let keys = staging.path().join("secrets/founder-keys");
    let mut legacy = legacy_config(&config, software_root.path(), &keys)?;
    let tee = legacy
        .get_mut("tee")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated config has no tee section"))?;
    tee.insert("isv_prod_id".into(), json!(sgx.measurements.isv_prod_id));
    tee.insert("minimum_isv_svn".into(), json!(sgx.measurements.isv_svn));
    tee.insert(
        "minimum_tcb_evaluation_data_number".into(),
        json!(MINIMUM_TCB_EVALUATION_DATA_NUMBER),
    );
    tee.insert("mrenclave".into(), json!(sgx.measurements.mrenclave));
    tee.insert("mrsigner".into(), json!(sgx.measurements.mrsigner));
    legacy
        .as_object_mut()
        .expect("legacy config is an object")
        .insert("signed_enclave_dir".into(), json!(signed_sgx));

    let keygen = software_root.path().join("bin/outbe-keygen");
    let validators = discover_validators(
        &legacy,
        &keys,
        keygen
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
    )
    .map_err(eyre::Report::msg)?;
    let seeded = staging.path().join("genesis.seeded.json");
    let final_genesis = staging.path().join("genesis.json");
    run_tee_stage(
        software_root
            .path()
            .join("bin/outbe-chain")
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
        &seeded,
        &final_genesis,
        &legacy,
    )
    .map_err(eyre::Report::msg)?;
    let descriptor =
        verify_descriptor_binding(signed_sgx, &final_genesis, &validators, config.chain_id)?;
    launch_bundle::render(&legacy, &validators, &final_genesis, &keys)
        .map_err(eyre::Report::msg)?;

    network.state = BuildState::Complete;
    network.genesis.final_path = Some("genesis.json".into());
    network.genesis.final_sha256 = Some(sha256_file(&final_genesis)?);
    let (genesis_block_hash, _) = genesis_identity(&final_genesis)?;
    network.genesis.genesis_block_hash = Some(genesis_block_hash);
    network.signed_sgx = Some(SignedSgxIdentity {
        bundle_manifest_sha256: sha256_file(&signed_sgx.join(format!(
            "metadata/{}-sgx-bundle.json",
            config.network.as_str()
        )))?,
        descriptor_sha256: sha256_file(&signed_sgx.join("metadata/network-descriptor-v1.bin"))?,
        network_binding: hex::encode(
            descriptor
                .network_binding
                .encode_canonical()
                .map_err(|error| eyre::eyre!("encode final network binding: {error}"))?,
        ),
        mrenclave: sgx.measurements.mrenclave,
        mrsigner: sgx.measurements.mrsigner,
        isv_prod_id: sgx.measurements.isv_prod_id,
        isv_svn: sgx.measurements.isv_svn,
    });
    network.founders = founder_artifacts(staging.path(), &config, &validators, false)?;
    network.joining = Some(build_joining_artifacts(&BuildArtifacts {
        root: staging.path(),
        software_root: software_root.path(),
        software: &software,
        config: &config,
        validators: &validators,
    })?);
    fs::write(
        staging.path().join("network-manifest.json"),
        format!("{}\n", serde_json::to_string_pretty(&network)?),
    )?;
    fs::rename(staging.keep(), output)?;
    println!("network artifacts: {}", output.display());
    println!("state: complete; ready for `outbe-up network prepare`");
    Ok(())
}

fn read_network_manifest(root: &Path) -> Result<NetworkManifest> {
    let path = root.join("network-manifest.json");
    let manifest = serde_json::from_slice(&fs::read(&path)?)
        .wrap_err_with(|| format!("invalid network manifest: {}", path.display()))?;
    Ok(manifest)
}

fn verify_build_identity(
    root: &Path,
    config: &NetworkConfig,
    manifest: &NetworkManifest,
) -> Result<()> {
    ensure!(
        manifest.version == 1,
        "unsupported network manifest version"
    );
    ensure!(
        manifest.network == config.network.as_str()
            && manifest.chain_id == config.chain_id
            && manifest.name == config.name,
        "network config does not match its build manifest"
    );
    ensure!(
        sha256_file(&root.join("network.yaml"))? == manifest.config_sha256,
        "network config changed after build"
    );
    ensure!(
        sha256_file(&root.join("software-bundle.tar.zst"))? == manifest.software.archive_sha256,
        "software bundle changed after network build"
    );
    ensure!(
        sha256_file(&root.join(&manifest.genesis.seeded_path))? == manifest.genesis.seeded_sha256,
        "seeded genesis changed after network build"
    );
    Ok(())
}

fn verify_signed_sgx(
    root: &Path,
    config: &NetworkConfig,
    software_commit: &str,
) -> Result<SgxManifest> {
    ensure!(root.is_dir(), "signed SGX bundle is not a directory");
    reject_unsafe_tree(root)?;
    let manifest_path = root.join(format!(
        "metadata/{}-sgx-bundle.json",
        config.network.as_str()
    ));
    let manifest: SgxManifest = serde_json::from_slice(&fs::read(&manifest_path)?)
        .wrap_err_with(|| format!("invalid signed SGX manifest: {}", manifest_path.display()))?;
    ensure!(
        manifest.schema_version == "1.0.0"
            && manifest.bundle_version == 1
            && manifest.network == config.network.as_str()
            && manifest.authorization_scope == config.network.as_str()
            && manifest.chain_id == config.chain_id
            && manifest.platform == "linux/amd64"
            && manifest.install_root == "/opt/outbe/sgx"
            && manifest.sealed_state_schema > 0
            && !manifest.network_name.is_empty()
            && !manifest.sigstruct_date.is_empty()
            && manifest.gramine.is_object(),
        "signed SGX bundle identity does not match the network"
    );
    ensure!(
        manifest.source.commit == software_commit,
        "signed SGX bundle was built from a different source commit"
    );
    ensure!(
        manifest.source.source_date_epoch >= 0 && !manifest.source.tag.is_empty(),
        "signed SGX source identity is incomplete"
    );
    ensure!(
        !manifest.measurements.debug
            && manifest.measurements.isv_prod_id > 0
            && manifest.measurements.isv_svn > 0
            && lower_hex(&manifest.measurements.mrenclave, 64)
            && lower_hex(&manifest.measurements.mrsigner, 64),
        "signed SGX measurements are invalid or debug-enabled"
    );

    let mut declared = BTreeSet::new();
    for file in &manifest.files {
        let relative = safe_relative(&file.path)?;
        ensure!(
            declared.insert(relative.clone()),
            "duplicate SGX bundle file"
        );
        ensure!(
            file.digest.algorithm == "sha256",
            "unsupported SGX file digest"
        );
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("missing signed SGX file: {}", file.path))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "signed SGX path is not a regular file: {}",
            file.path
        );
        ensure!(metadata.len() == file.size, "signed SGX file size mismatch");
        ensure!(
            format!("{:04o}", metadata.permissions().mode() & 0o7777) == file.mode,
            "signed SGX file mode mismatch: {}",
            file.path
        );
        ensure!(
            sha256_file(&path)? == file.digest.value,
            "signed SGX file checksum mismatch: {}",
            file.path
        );
    }
    let manifest_relative = format!("metadata/{}-sgx-bundle.json", config.network.as_str());
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == manifest_relative
            || matches!(relative.as_str(), "SHA256SUMS" | "SHA256SUMS.unsigned")
        {
            continue;
        }
        ensure!(
            declared.contains(Path::new(&relative)),
            "signed SGX bundle has an undeclared file: {relative}"
        );
    }
    let install = root.join("rootfs/opt/outbe/sgx");
    for name in SIGNED_ENCLAVE_FILES {
        ensure!(
            install.join(name).is_file(),
            "signed SGX bundle is missing {name}"
        );
    }
    read_descriptor(root)?;
    Ok(manifest)
}

fn reject_unsafe_tree(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "signed SGX bundle contains a symlink"
        );
        if metadata.is_file() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            ensure!(
                !name.ends_with(".pem") && !name.ends_with(".key"),
                "signed SGX bundle contains private-key material"
            );
        }
    }
    Ok(())
}

fn verify_descriptor_binding(
    signed_sgx: &Path,
    genesis: &Path,
    validators: &[Value],
    chain_id: u64,
) -> Result<TrustedNetworkDescriptorV1> {
    let descriptor = read_descriptor(signed_sgx)?;
    let genesis: Value = serde_json::from_slice(&fs::read(genesis)?)?;
    let encoded = genesis
        .pointer("/config/teeAttestationV1/policySchedule")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("final genesis has no TEE policy schedule"))?;
    let schedule = TeePolicyScheduleV1::decode_canonical(&hex::decode(
        encoded.strip_prefix("0x").unwrap_or(encoded),
    )?)
    .map_err(|error| eyre::eyre!("invalid final TEE policy schedule: {error}"))?;
    ensure!(
        schedule.chain_id == U256::from(chain_id).to_be_bytes::<32>()
            && descriptor.network_binding.chain_id == schedule.chain_id
            && descriptor.network_binding.genesis_hash == schedule.genesis_hash
            && descriptor.network_binding.attestation_mode == AttestationMode::DcapRequired,
        "signed SGX network descriptor does not match final genesis"
    );
    let mut keys = validators
        .iter()
        .map(|validator| {
            let text = super::build::required_validator_field(validator, "public_key")?;
            let bytes = hex::decode(text.strip_prefix("0x").unwrap_or(text))?;
            <[u8; 48]>::try_from(bytes.as_slice())
                .map_err(|_| eyre::eyre!("founder consensus public key is not 48 bytes"))
        })
        .collect::<Result<Vec<_>>>()?;
    keys.sort_unstable();
    ensure!(
        descriptor.genesis_consensus_keys == keys,
        "signed SGX network descriptor has a different founding committee"
    );
    Ok(descriptor)
}

fn read_descriptor(root: &Path) -> Result<TrustedNetworkDescriptorV1> {
    let metadata = fs::read(root.join("metadata/network-descriptor-v1.bin"))?;
    let measured = fs::read(root.join("rootfs/opt/outbe/sgx/network-descriptor-v1.bin"))?;
    ensure!(
        metadata == measured,
        "signed and measured network descriptors differ"
    );
    TrustedNetworkDescriptorV1::decode_canonical(&metadata)
        .map_err(|error| eyre::eyre!("invalid trusted network descriptor: {error}"))
}

fn safe_relative(value: &str) -> Result<std::path::PathBuf> {
    let path = Path::new(value);
    ensure!(!path.as_os_str().is_empty(), "empty SGX bundle path");
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_))),
        "unsafe SGX bundle path: {value}"
    );
    Ok(path.to_owned())
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_signed_bundle_paths() {
        assert!(safe_relative("../secret").is_err());
        assert!(safe_relative("/absolute").is_err());
        assert_eq!(
            safe_relative("rootfs/opt/outbe/sgx/enclave.sig").unwrap(),
            Path::new("rootfs/opt/outbe/sgx/enclave.sig")
        );
    }

    #[test]
    fn measurement_hex_is_canonical() {
        assert!(lower_hex(&"ab".repeat(32), 64));
        assert!(!lower_hex(&"AB".repeat(32), 64));
        assert!(!lower_hex("abcd", 64));
    }
}
