//! Testnet SGX release bundle preparation, signing and verification.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Read},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use eyre::{bail, eyre, Result, WrapErr};
use filetime::FileTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use walkdir::WalkDir;

const REQUIRED_BUNDLE_FILES: [&str; 6] = [
    "rootfs/opt/outbe/sgx/bin/outbe-tee-enclave",
    "rootfs/opt/outbe/sgx/gramine/libpal.so",
    "rootfs/opt/outbe/sgx/gramine/loader",
    "rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest",
    "rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest.sgx",
    "rootfs/opt/outbe/sgx/outbe-tee-enclave.sig",
];

const EXCLUDED_BUNDLE_FILES: [&str; 3] = [
    "metadata/testnet-sgx-bundle.json",
    "SHA256SUMS",
    "SHA256SUMS.unsigned",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BundleSpec {
    pub authorization_scope: String,
    pub bundle_version: u32,
    pub gramine: GramineIdentity,
    pub inputs: Vec<String>,
    pub install_root: String,
    pub platform: String,
    pub sealed_state_schema: u32,
    pub sgx: SgxPolicy,
    pub spec_version: u32,
}

impl BundleSpec {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("read SGX bundle spec metadata: {}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!("missing or unsafe SGX bundle spec: {}", path.display());
        }
        let bytes =
            fs::read(path).wrap_err_with(|| format!("read SGX bundle spec: {}", path.display()))?;
        let spec: Self = serde_json::from_slice(&bytes)
            .wrap_err_with(|| format!("parse SGX bundle spec: {}", path.display()))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<()> {
        if self.spec_version != 1 || self.bundle_version != 1 {
            bail!("unsupported testnet SGX bundle contract");
        }
        if self.authorization_scope != "testnet" {
            bail!("SGX bundle must be authorized only for testnet");
        }
        let Some((image, digest)) = self.gramine.builder_image.split_once("@sha256:") else {
            bail!("Gramine builder image must be pinned by sha256 digest");
        };
        if image.is_empty() || image.chars().any(char::is_whitespace) || !is_lower_hex(digest, 64) {
            bail!("Gramine builder image must be pinned by sha256 digest");
        }
        if !is_lower_hex(&self.gramine.source_commit, 40) {
            bail!("Gramine source commit must be a lowercase 40-character Git SHA");
        }
        if self.platform != "linux/amd64" {
            bail!("testnet SGX bundle supports only linux/amd64");
        }
        if self.install_root != "/opt/outbe/sgx" {
            bail!("testnet SGX install root must remain /opt/outbe/sgx");
        }
        if self.sgx.debug {
            bail!("release SGX bundle must use a non-debug enclave");
        }
        if self.sgx.remote_attestation != "none" {
            bail!("testnet release currently supports local SGX evidence only");
        }
        if self.sgx.sigstruct_date_source != "source-date-epoch-utc" {
            bail!("SIGSTRUCT date must derive from SOURCE_DATE_EPOCH in UTC");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GramineIdentity {
    pub builder_image: String,
    pub source_commit: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SgxPolicy {
    pub debug: bool,
    pub edmm_enable: bool,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub max_threads: u32,
    pub remote_attestation: String,
    pub sigstruct_date_source: String,
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
    pub files: Vec<BundleFile>,
    pub gramine: GramineIdentity,
    pub install_root: String,
    pub measurements: Measurements,
    pub platform: String,
    pub schema_version: String,
    pub sealed_state_schema: u32,
    pub sigstruct_date: String,
    pub source: ManifestSource,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ComparisonEvidence {
    pub entry_count: usize,
    pub result: String,
    pub schema_version: String,
    pub tree_digest: Sha256Digest,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct TreeEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<Sha256Digest>,
    mode: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    kind: String,
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).wrap_err("serialize canonical JSON value")?;
    let value = sort_json(value);
    let mut encoded = serde_json::to_vec(&value).wrap_err("encode canonical JSON")?;
    encoded.push(b'\n');
    Ok(encoded)
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
        _ => bail!("SIGSTRUCT debug_enclave must be True or False"),
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
        files: bundle_files(bundle_root)?,
        gramine: bundle_spec.gramine.clone(),
        install_root: bundle_spec.install_root.clone(),
        measurements,
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
        || manifest.authorization_scope != "testnet"
        || manifest.bundle_version != bundle_spec.bundle_version
        || manifest.gramine != bundle_spec.gramine
        || manifest.install_root != bundle_spec.install_root
        || manifest.platform != bundle_spec.platform
        || manifest.sealed_state_schema != bundle_spec.sealed_state_schema
    {
        bail!("bundle metadata does not match the testnet SGX contract");
    }
    if manifest.files != bundle_files(bundle_root)? {
        bail!("bundle file matrix mismatch");
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

fn validate_measurements(spec: &BundleSpec, measurements: &Measurements) -> Result<()> {
    if measurements.debug != spec.sgx.debug
        || measurements.isv_prod_id != spec.sgx.isv_prod_id
        || measurements.isv_svn != spec.sgx.isv_svn
    {
        bail!("SIGSTRUCT identity does not match the SGX bundle contract");
    }
    Ok(())
}

fn sigstruct_date(source_date_epoch: i64) -> Result<String> {
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

fn tree_entries(root: &Path) -> Result<Vec<TreeEntry>> {
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

fn file_digest(path: &Path) -> Result<Sha256Digest> {
    let file =
        File::open(path).wrap_err_with(|| format!("open for hashing: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .wrap_err_with(|| format!("hash file: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest {
        algorithm: "sha256".to_owned(),
        value: hex::encode(hasher.finalize()),
    })
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn prepare(repo_root: &Path, elf_output: &Path, output: &Path) -> Result<()> {
    let spec = BundleSpec::read(&repo_root.join("release/testnet-sgx-bundle-v1.json"))?;
    require_release_checkout(repo_root)?;
    verify_checksums(elf_output, "SHA256SUMS")?;
    let identity = read_elf_identity(elf_output)?;
    require_clean_source(repo_root, &identity.source_commit)?;
    let output = create_empty_output(repo_root, output)?;
    let elf_output = fs::canonicalize(elf_output)
        .wrap_err_with(|| format!("resolve ELF output: {}", elf_output.display()))?;

    let mut command = docker_command(&spec, repo_root)?;
    command
        .args(["-e", &format!("SGX_MAX_THREADS={}", spec.sgx.max_threads)])
        .args(["-e", &format!("SGX_ISV_PROD_ID={}", spec.sgx.isv_prod_id)])
        .args(["-e", &format!("SGX_ISV_SVN={}", spec.sgx.isv_svn)])
        .args(["-v", &format!("{}:/elf:ro", elf_output.display())])
        .args(["-v", &format!("{}:/out", output.display())])
        .arg(&spec.gramine.builder_image)
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

pub fn sign(repo_root: &Path, unsigned: &Path, key_file: &Path, output: &Path) -> Result<()> {
    let spec = BundleSpec::read(&repo_root.join("release/testnet-sgx-bundle-v1.json"))?;
    require_release_checkout(repo_root)?;
    validate_signing_key(key_file)?;
    let unsigned = fs::canonicalize(unsigned)
        .wrap_err_with(|| format!("resolve unsigned SGX bundle: {}", unsigned.display()))?;
    let key_file = fs::canonicalize(key_file)
        .wrap_err_with(|| format!("resolve testnet SGX signing key: {}", key_file.display()))?;
    verify_checksums(&unsigned, "SHA256SUMS.unsigned")?;
    let identity: SourceIdentity =
        read_canonical_json(&unsigned.join("metadata/source-identity.json"))?;
    validate_source_identity(&identity)?;
    require_clean_source(repo_root, &identity.source_commit)?;
    let output = create_empty_output(repo_root, output)?;
    let date = sigstruct_date(identity.source_date_epoch)?;

    let mut command = docker_command(&spec, repo_root)?;
    command
        .args(["-e", &format!("SIGSTRUCT_DATE={date}")])
        .args(["-v", &format!("{}:/unsigned:ro", unsigned.display())])
        .args([
            "-v",
            &format!("{}:/run/secrets/testnet-sgx-key.pem:ro", key_file.display()),
        ])
        .args(["-v", &format!("{}:/out", output.display())])
        .arg(&spec.gramine.builder_image)
        .args([container_adapter(), "sign"]);
    run_status(&mut command, "sign testnet SGX bundle")?;

    let sigstruct_view = fs::read_to_string(output.join("metadata/sigstruct.txt"))
        .wrap_err("read signed bundle SIGSTRUCT evidence")?;
    let manifest = build_bundle_manifest(&output, &spec, &identity, &sigstruct_view)?;
    write_canonical(&output.join("metadata/testnet-sgx-bundle.json"), &manifest)?;
    verify_signed_bundle(&output, &manifest, &spec, &sigstruct_view)?;
    write_checksums(&output, "SHA256SUMS")?;
    normalize_tree_mtime(&output, identity.source_date_epoch)?;
    verify_checksums(&output, "SHA256SUMS")?;
    Ok(())
}

pub fn verify(repo_root: &Path, bundle: &Path) -> Result<()> {
    let spec = BundleSpec::read(&repo_root.join("release/testnet-sgx-bundle-v1.json"))?;
    require_release_checkout(repo_root)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    verify_checksums(&bundle, "SHA256SUMS")?;
    let manifest_path = bundle.join("metadata/testnet-sgx-bundle.json");
    let manifest: BundleManifest = read_canonical_json(&manifest_path)?;

    let mut command = docker_command(&spec, repo_root)?;
    command
        .args(["-v", &format!("{}:/bundle:ro", bundle.display())])
        .arg(&spec.gramine.builder_image)
        .args([container_adapter(), "view"]);
    let sigstruct_view = run_output(&mut command, "read signed SGX SIGSTRUCT")?;
    verify_signed_bundle(&bundle, &manifest, &spec, &sigstruct_view)
}

pub fn build_image(
    repo_root: &Path,
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
    verify(repo_root, bundle)?;
    let bundle = fs::canonicalize(bundle)
        .wrap_err_with(|| format!("resolve signed SGX bundle: {}", bundle.display()))?;
    if output.starts_with(&bundle) {
        bail!("OCI build evidence must be outside the signed bundle");
    }
    let manifest_path = bundle.join("metadata/testnet-sgx-bundle.json");
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
        command.args(["--push", "--provenance=mode=max", "--sbom=true"]);
    } else {
        command.args(["--load", "--provenance=false", "--sbom=false"]);
    }
    command.arg(&bundle);
    run_status(&mut command, "build immutable testnet SGX OCI image")?;
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

pub fn repository_root() -> Result<PathBuf> {
    let mut command = Command::new("git");
    command.args(["rev-parse", "--show-toplevel"]);
    let value = run_output(&mut command, "resolve repository root")?;
    fs::canonicalize(value.trim()).wrap_err("canonicalize repository root")
}

fn require_release_checkout(repo_root: &Path) -> Result<()> {
    for relative in [
        "release/testnet-sgx-bundle-v1.json",
        "scripts/release/build-testnet-sgx-bundle-in-container.sh",
        "xtask/Cargo.toml",
    ] {
        if !repo_root.join(relative).is_file() {
            bail!("repository is missing SGX release input: {relative}");
        }
    }
    Ok(())
}

fn read_elf_identity(elf_output: &Path) -> Result<SourceIdentity> {
    let manifest: Value = read_canonical_json(&elf_output.join("release-manifest.json"))?;
    let source = manifest
        .pointer("/release/source")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("ELF manifest lacks release source identity"))?;
    if source.get("tree_state").and_then(Value::as_str) != Some("clean")
        || source.get("clean_tree_policy").and_then(Value::as_str) != Some("required")
    {
        bail!("ELF manifest does not bind a required clean tree");
    }
    let source_commit = source
        .get("commit")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF manifest lacks source commit"))?
        .to_owned();
    let source_date_epoch = manifest
        .pointer("/build/source_date_epoch")
        .and_then(Value::as_i64)
        .ok_or_else(|| eyre!("ELF manifest lacks SOURCE_DATE_EPOCH"))?;
    let release_tag = manifest
        .pointer("/release/tag")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF manifest lacks release tag"))?
        .to_owned();
    let enclave = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .and_then(|artifacts| {
            artifacts.iter().find(|artifact| {
                artifact.get("name").and_then(Value::as_str) == Some("outbe-tee-enclave")
            })
        })
        .ok_or_else(|| eyre!("ELF manifest lacks the production enclave subject"))?;
    if enclave.get("tee") != Some(&serde_json::json!({"mock": false, "stage": "unsigned-bare-elf"}))
    {
        bail!("ELF manifest lacks the production enclave subject");
    }
    let enclave_path = elf_output.join("bin/outbe-tee-enclave");
    let metadata = fs::symlink_metadata(&enclave_path)
        .wrap_err("read enclave ELF from reproducible output")?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("reproducible output contains an unsafe enclave ELF");
    }
    let expected_digest = enclave
        .pointer("/digest/value")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF manifest lacks enclave digest"))?;
    let expected_size = enclave
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("ELF manifest lacks enclave size"))?;
    if file_digest(&enclave_path)?.value != expected_digest || metadata.len() != expected_size {
        bail!("enclave ELF does not match its release manifest");
    }
    let identity = SourceIdentity {
        release_tag,
        source_commit,
        source_date_epoch,
    };
    validate_source_identity(&identity)?;
    Ok(identity)
}

fn validate_source_identity(identity: &SourceIdentity) -> Result<()> {
    if !is_lower_hex(&identity.source_commit, 40) {
        bail!("source commit must be a lowercase 40-character Git SHA");
    }
    if identity.source_date_epoch < 0 {
        bail!("SOURCE_DATE_EPOCH must be non-negative");
    }
    if identity.release_tag.is_empty() || !identity.release_tag.is_ascii() {
        bail!("release tag must be non-empty ASCII");
    }
    Ok(())
}

fn require_clean_source(repo_root: &Path, expected_commit: &str) -> Result<()> {
    let mut status = Command::new("git");
    status
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"]);
    if !run_output(&mut status, "inspect source tree state")?.is_empty() {
        bail!("testnet SGX release operations require a clean source tree");
    }
    let mut head = Command::new("git");
    head.arg("-C").arg(repo_root).args(["rev-parse", "HEAD"]);
    let head = run_output(&mut head, "resolve source commit")?;
    if head.trim() != expected_commit {
        bail!(
            "SGX source identity {expected_commit} does not match checkout {}",
            head.trim()
        );
    }
    Ok(())
}

fn validate_signing_key(key_file: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(key_file)
        .wrap_err_with(|| format!("read signing key metadata: {}", key_file.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "missing or unsafe testnet SGX signing key: {}",
            key_file.display()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "unsafe testnet SGX signing key permissions: {mode:03o}; expected no group/other access"
        );
    }
    if metadata.len() == 0 {
        bail!("testnet SGX signing key is empty");
    }
    Ok(())
}

fn create_empty_output(repo_root: &Path, output: &Path) -> Result<PathBuf> {
    let output = absolute_path(output)?;
    if output.starts_with(repo_root) {
        bail!("output directory must be outside the source checkout");
    }
    if output.exists() {
        if !output.is_dir() {
            bail!("output path is not a directory: {}", output.display());
        }
        if fs::read_dir(&output)
            .wrap_err("read output directory")?
            .next()
            .is_some()
        {
            bail!("output directory must be empty: {}", output.display());
        }
    } else {
        fs::create_dir_all(&output)
            .wrap_err_with(|| format!("create output directory: {}", output.display()))?;
    }
    fs::canonicalize(&output).wrap_err("canonicalize output directory")
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .wrap_err("resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes filesystem root: {}", path.display());
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn verify_checksums(root: &Path, name: &str) -> Result<()> {
    let checksum_path = root.join(name);
    let content = fs::read_to_string(&checksum_path)
        .wrap_err_with(|| format!("read checksums: {}", checksum_path.display()))?;
    if content.is_empty() {
        bail!("checksum file is empty: {}", checksum_path.display());
    }
    for (index, line) in content.lines().enumerate() {
        let Some((digest, relative)) = line.split_once("  ") else {
            bail!(
                "invalid checksum row {} in {}",
                index + 1,
                checksum_path.display()
            );
        };
        if !is_lower_hex(digest, 64) {
            bail!("invalid checksum digest at row {}", index + 1);
        }
        let relative = safe_relative_path(relative)?;
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("checksum input is missing: {}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!(
                "checksum input is not a safe regular file: {}",
                path.display()
            );
        }
        if file_digest(&path)?.value != digest {
            bail!("checksum mismatch: {}", path.display());
        }
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe relative artifact path: {value}");
    }
    Ok(path)
}

fn write_checksums(root: &Path, name: &str) -> Result<()> {
    let mut rows = Vec::new();
    for item in WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let item = item.wrap_err("walk output for checksums")?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)
            .wrap_err("derive checksum path")?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(path).wrap_err("read checksum input metadata")?;
        if metadata.file_type().is_symlink() {
            bail!("output contains symlink: {relative}");
        }
        if metadata.is_file() && relative != name {
            rows.push((relative, file_digest(path)?.value));
        }
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let content = rows
        .into_iter()
        .map(|(path, digest)| format!("{digest}  {path}\n"))
        .collect::<String>();
    fs::write(root.join(name), content).wrap_err("write output checksums")
}

fn write_canonical<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("create metadata directory: {}", parent.display()))?;
    }
    fs::write(path, canonical_json(value)?)
        .wrap_err_with(|| format!("write canonical metadata: {}", path.display()))
}

fn read_canonical_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("read JSON metadata: {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("missing or unsafe JSON input: {}", path.display());
    }
    let bytes = fs::read(path).wrap_err_with(|| format!("read JSON input: {}", path.display()))?;
    let value: T = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("parse JSON input: {}", path.display()))?;
    if bytes != canonical_json(&value)? {
        bail!(
            "JSON input is not canonical outbe-canonical-json-v1: {}",
            path.display()
        );
    }
    Ok(value)
}

fn normalize_tree_mtime(root: &Path, source_date_epoch: i64) -> Result<()> {
    if source_date_epoch < 0 {
        bail!("SOURCE_DATE_EPOCH must be non-negative");
    }
    let timestamp = FileTime::from_unix_time(source_date_epoch, 0);
    let mut entries = WalkDir::new(root)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .wrap_err("walk output for timestamp normalization")?;
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.depth()));
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path()).wrap_err("read timestamp target")?;
        if metadata.file_type().is_symlink() {
            bail!(
                "cannot normalize symlink timestamp: {}",
                entry.path().display()
            );
        }
        filetime::set_file_times(entry.path(), timestamp, timestamp)
            .wrap_err_with(|| format!("normalize timestamp: {}", entry.path().display()))?;
    }
    Ok(())
}

fn docker_command(spec: &BundleSpec, repo_root: &Path) -> Result<Command> {
    let uid = current_id("-u")?;
    let gid = current_id("-g")?;
    let mut command = Command::new("docker");
    command
        .args(["run", "--rm", "--platform", &spec.platform])
        .args(["--user", &format!("{uid}:{gid}")])
        .args(["--entrypoint", "bash"])
        .args(["-v", &format!("{}:/source:ro", repo_root.display())]);
    Ok(command)
}

fn current_id(flag: &str) -> Result<String> {
    let mut command = Command::new("id");
    command.arg(flag);
    Ok(run_output(&mut command, "resolve current Unix identity")?
        .trim()
        .to_owned())
}

fn container_adapter() -> &'static str {
    "/source/scripts/release/build-testnet-sgx-bundle-in-container.sh"
}

fn run_status(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .wrap_err_with(|| format!("failed to start command: {description}"))?;
    if !status.success() {
        bail!("{description} failed with {status}");
    }
    Ok(())
}

fn run_output(command: &mut Command, description: &str) -> Result<String> {
    let Output {
        status,
        stdout,
        stderr,
    } = command
        .output()
        .wrap_err_with(|| format!("failed to start command: {description}"))?;
    if !status.success() {
        bail!(
            "{description} failed with {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    String::from_utf8(stdout).wrap_err_with(|| format!("{description} emitted non-UTF-8 output"))
}
