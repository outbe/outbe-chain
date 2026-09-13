use super::canonical_json;
use super::file_digest;
use super::is_lower_hex;
use super::BundleFile;
use super::BundleManifest;
use super::BundleSpec;
use super::ManifestSource;
use super::Measurements;
use super::OciDescriptor;
use super::Sha256Digest;
use super::SourceIdentity;
use super::EXCLUDED_BUNDLE_FILES;
use super::REQUIRED_BUNDLE_FILES;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;

use outbe_evm::tee_attestation_activation::DcapChainSpecBindingV1;
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;

use outbe_primitives::tee_attestation_v1::AttestationMode;
use outbe_primitives::tee_attestation_v1::NetworkBindingV1;
use outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;

use std::os::unix::fs::PermissionsExt;

use std::path::Path;

use time::OffsetDateTime;
use walkdir::WalkDir;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ComparisonEvidence {
    pub entry_count: usize,
    pub result: String,
    pub schema_version: String,
    pub tree_digest: Sha256Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct TreeEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) digest: Option<Sha256Digest>,
    pub(super) mode: String,
    pub(super) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) size: Option<u64>,
    pub(super) kind: String,
}

pub fn parse_sigstruct_view(output: &str) -> Result<Measurements> {
    let values = output
        .lines()
        .filter_map(|raw| raw.trim().split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();

    let field = |name: &str| {
        values
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| eyre!("SIGSTRUCT output missing field: {name}"))
    };
    let mrsigner = field("mr_signer")?.to_ascii_lowercase();
    let mrenclave = field("mr_enclave")?.to_ascii_lowercase();
    if !is_lower_hex(&mrsigner, 64) {
        bail!("SIGSTRUCT MRSIGNER must be 32 lowercase hexadecimal bytes");
    }
    if !is_lower_hex(&mrenclave, 64) {
        bail!("SIGSTRUCT MRENCLAVE must be 32 lowercase hexadecimal bytes");
    }
    let debug = match field("debug_enclave")?.to_ascii_lowercase().as_str() {
        "true" => true,
        "false" => false,
        _ => return Err(eyre!("SIGSTRUCT debug_enclave must be True or False")),
    };

    Ok(Measurements {
        debug,
        isv_prod_id: field("isv_prod_id")?
            .parse()
            .wrap_err("parse SIGSTRUCT isv_prod_id")?,
        isv_svn: field("isv_svn")?
            .parse()
            .wrap_err("parse SIGSTRUCT isv_svn")?,
        mrenclave,
        mrsigner,
    })
}

pub fn parse_oci_descriptor(metadata: &str) -> Result<OciDescriptor> {
    let value: Value = serde_json::from_str(metadata).wrap_err("parse BuildKit metadata")?;
    let descriptor = value
        .get("containerimage.descriptor")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("BuildKit metadata lacks OCI descriptor"))?;
    let digest = descriptor
        .get("digest")
        .and_then(Value::as_str)
        .or_else(|| value.get("containerimage.digest").and_then(Value::as_str))
        .ok_or_else(|| eyre!("BuildKit metadata lacks OCI descriptor digest"))?;
    let Some(digest) = digest.strip_prefix("sha256:") else {
        bail!("OCI descriptor digest must use sha256");
    };
    if !is_lower_hex(digest, 64) {
        bail!("OCI descriptor digest must contain 32 lowercase hexadecimal bytes");
    }
    let media_type = descriptor
        .get("mediaType")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("BuildKit metadata lacks OCI descriptor media type"))?;
    if media_type.is_empty() || !media_type.is_ascii() {
        bail!("OCI descriptor media type must be non-empty ASCII");
    }
    let size = descriptor
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("BuildKit metadata lacks OCI descriptor size"))?;
    Ok(OciDescriptor {
        digest: Sha256Digest {
            algorithm: "sha256".to_owned(),
            value: digest.to_owned(),
        },
        media_type: media_type.to_owned(),
        size,
    })
}

pub fn compare_unsigned_trees(first: &Path, second: &Path) -> Result<ComparisonEvidence> {
    let first_entries = tree_entries(first)?;
    let second_entries = tree_entries(second)?;
    if first_entries != second_entries {
        bail!("unsigned SGX bundle mismatch");
    }
    let digest = Sha256::digest(canonical_json(&first_entries)?);
    Ok(ComparisonEvidence {
        entry_count: first_entries.len(),
        result: "identical".to_owned(),
        schema_version: "1.0.0".to_owned(),
        tree_digest: Sha256Digest {
            algorithm: "sha256".to_owned(),
            value: hex::encode(digest),
        },
    })
}

pub fn build_bundle_manifest(
    bundle_root: &Path,
    bundle_spec: &BundleSpec,
    source: &SourceIdentity,
    sigstruct_view: &str,
) -> Result<BundleManifest> {
    bundle_spec.validate()?;
    if !is_lower_hex(&source.source_commit, 40) {
        bail!("source commit must be a lowercase 40-character Git SHA");
    }
    if source.release_tag.is_empty() || !source.release_tag.is_ascii() {
        bail!("release tag must be non-empty ASCII");
    }
    let measurements = parse_sigstruct_view(sigstruct_view)?;
    validate_measurements(bundle_spec, &measurements)?;

    Ok(BundleManifest {
        authorization_scope: bundle_spec.authorization_scope.clone(),
        bundle_version: bundle_spec.bundle_version,
        chain_id: bundle_spec.chain_id,
        files: bundle_files(bundle_root)?,
        gramine: bundle_spec.gramine.clone(),
        install_root: bundle_spec.install_root.clone(),
        measurements,
        network: bundle_spec.network.clone(),
        network_name: bundle_spec.network_name.clone(),
        platform: bundle_spec.platform.clone(),
        schema_version: "1.0.0".to_owned(),
        sealed_state_schema: bundle_spec.sealed_state_schema,
        sigstruct_date: sigstruct_date(source.source_date_epoch)?,
        source: ManifestSource {
            commit: source.source_commit.clone(),
            source_date_epoch: source.source_date_epoch,
            tag: source.release_tag.clone(),
        },
    })
}

pub fn verify_signed_bundle(
    bundle_root: &Path,
    manifest: &BundleManifest,
    bundle_spec: &BundleSpec,
    sigstruct_view: &str,
) -> Result<()> {
    bundle_spec.validate()?;
    if manifest.schema_version != "1.0.0"
        || manifest.authorization_scope != bundle_spec.authorization_scope
        || manifest.bundle_version != bundle_spec.bundle_version
        || manifest.chain_id != bundle_spec.chain_id
        || manifest.gramine != bundle_spec.gramine
        || manifest.install_root != bundle_spec.install_root
        || manifest.network != bundle_spec.network
        || manifest.network_name != bundle_spec.network_name
        || manifest.platform != bundle_spec.platform
        || manifest.sealed_state_schema != bundle_spec.sealed_state_schema
    {
        bail!("bundle metadata does not match the SGX network contract");
    }
    if manifest.files != bundle_files(bundle_root)? {
        bail!("bundle file matrix mismatch");
    }
    let descriptor = read_measured_network_descriptor(bundle_root)?;
    if descriptor.network_binding.chain_id
        != alloy_primitives::U256::from(bundle_spec.chain_id).to_be_bytes()
    {
        bail!("measured network descriptor belongs to a foreign chain");
    }
    let measurements = parse_sigstruct_view(sigstruct_view)?;
    validate_measurements(bundle_spec, &measurements)?;
    if manifest.measurements != measurements {
        bail!("SIGSTRUCT measurements do not match bundle metadata");
    }
    if manifest.sigstruct_date != sigstruct_date(manifest.source.source_date_epoch)? {
        bail!("SIGSTRUCT date does not match SOURCE_DATE_EPOCH");
    }
    Ok(())
}

pub(super) fn read_measured_network_descriptor(
    bundle_root: &Path,
) -> Result<TrustedNetworkDescriptorV1> {
    let metadata_path = bundle_root.join("metadata/network-descriptor-v1.bin");
    let measured_path = bundle_root.join("rootfs/opt/outbe/sgx/network-descriptor-v1.bin");
    let metadata = fs::read(&metadata_path).wrap_err_with(|| {
        format!(
            "read trusted network descriptor: {}",
            metadata_path.display()
        )
    })?;
    let measured = fs::read(&measured_path).wrap_err_with(|| {
        format!(
            "read measured network descriptor: {}",
            measured_path.display()
        )
    })?;
    if metadata != measured {
        bail!("trusted network descriptor differs from the file measured into MRENCLAVE");
    }
    TrustedNetworkDescriptorV1::decode_canonical(&measured)
        .map_err(|error| eyre!("trusted network descriptor is invalid: {error}"))
}

pub(super) fn require_measured_network_descriptor(
    bundle_root: &Path,
    genesis: &Path,
    binding: &DcapChainSpecBindingV1,
) -> Result<()> {
    let actual = read_measured_network_descriptor(bundle_root)?;
    let seeded = DcapSeededChainSpecBindingV1::from_genesis_path(genesis)
        .map_err(|error| eyre!("release seeded ChainSpec binding is invalid: {error}"))?;
    let expected = TrustedNetworkDescriptorV1 {
        network_binding: NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(binding.chain_id).to_be_bytes(),
            genesis_hash: binding.genesis_hash,
            attestation_mode: AttestationMode::DcapRequired,
        },
        genesis_consensus_keys: seeded.genesis_consensus_keys,
    };
    if actual != expected {
        bail!("measured network descriptor does not match the release genesis ChainSpec");
    }
    Ok(())
}

fn validate_measurements(spec: &BundleSpec, measurements: &Measurements) -> Result<()> {
    if measurements.debug != spec.sgx.debug
        || measurements.isv_prod_id != spec.sgx.isv_prod_id
        || measurements.isv_svn != spec.sgx.isv_svn
    {
        bail!("SIGSTRUCT identity does not match the SGX bundle contract");
    }
    Ok(())
}

pub(super) fn sigstruct_date(source_date_epoch: i64) -> Result<String> {
    if source_date_epoch < 0 {
        bail!("SOURCE_DATE_EPOCH must be non-negative");
    }
    let date = OffsetDateTime::from_unix_timestamp(source_date_epoch)
        .wrap_err("SOURCE_DATE_EPOCH is outside the supported range")?
        .date();
    Ok(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    ))
}

pub(super) fn tree_entries(root: &Path) -> Result<Vec<TreeEntry>> {
    if !root.is_dir() {
        bail!("bundle tree is not a directory: {}", root.display());
    }
    let mut entries = Vec::new();
    for item in WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let item = item.wrap_err_with(|| format!("walk bundle tree: {}", root.display()))?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)
            .wrap_err("derive bundle tree relative path")?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("read bundle tree metadata: {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("bundle tree contains symlink: {relative}");
        }
        let mode = format!("{:04o}", metadata.permissions().mode() & 0o7777);
        if metadata.is_dir() {
            entries.push(TreeEntry {
                digest: None,
                mode,
                path: relative,
                size: None,
                kind: "directory".to_owned(),
            });
        } else if metadata.is_file() {
            entries.push(TreeEntry {
                digest: Some(file_digest(path)?),
                mode,
                path: relative,
                size: Some(metadata.len()),
                kind: "file".to_owned(),
            });
        } else {
            bail!("bundle tree contains unsupported entry: {relative}");
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn bundle_files(root: &Path) -> Result<Vec<BundleFile>> {
    if !root.is_dir() {
        bail!("SGX bundle is not a directory: {}", root.display());
    }
    let mut files = Vec::new();
    let mut found = BTreeSet::new();
    for item in WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let item = item.wrap_err_with(|| format!("walk SGX bundle: {}", root.display()))?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)
            .wrap_err("derive SGX bundle relative path")?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("read SGX bundle metadata: {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("bundle contains symlink: {relative}");
        }
        if !metadata.is_file() || EXCLUDED_BUNDLE_FILES.contains(&relative.as_str()) {
            continue;
        }
        let lowered = relative.to_ascii_lowercase();
        if lowered.ends_with(".pem") || lowered.ends_with(".key") || lowered.contains("private-key")
        {
            bail!("bundle contains forbidden private-key material: {relative}");
        }
        found.insert(relative.clone());
        files.push(BundleFile {
            digest: file_digest(path)?,
            mode: format!("{:04o}", metadata.permissions().mode() & 0o7777),
            path: relative,
            size: metadata.len(),
        });
    }
    let missing = REQUIRED_BUNDLE_FILES
        .iter()
        .filter(|path| !found.contains(**path))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!("SGX bundle missing required files: {}", missing.join(", "));
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}
