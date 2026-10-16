//! The test-only Gramine image, its signing key, and its SGX measurement.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use eyre::{bail, eyre, Result, WrapErr};

use super::base_cmd;

pub(super) const TEST_ENCLAVE_IMAGE: &str = "outbe-tee-enclave-gramine-test";
pub(super) const TEST_ENCLAVE_IMAGE_BUILD_ARGS: &[&str] = &["build", "--provenance=false", "-f"];
const PINNED_QVL_RUNTIME_FILES: &[(&str, &str)] = &[
    (
        "/usr/lib/x86_64-linux-gnu/libsgx_dcap_quoteverify.so.1.13.103.0",
        "libsgx_dcap_quoteverify.so.1",
    ),
    (
        "/usr/lib/x86_64-linux-gnu/libstdc++.so.6.0.33",
        "libstdc++.so.6",
    ),
    ("/usr/lib/x86_64-linux-gnu/libgcc_s.so.1", "libgcc_s.so.1"),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DockerImageId(String);

impl DockerImageId {
    pub(crate) fn from_inspect_output(output: &str) -> Result<Self> {
        let value = output.trim();
        let digest = value
            .strip_prefix("sha256:")
            .ok_or_else(|| eyre!("Docker image identity is not a sha256 ID"))?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("Docker image identity is not a canonical sha256 ID");
        }
        Ok(Self(value.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestSgxMeasurement {
    pub mrenclave: String,
    pub mrsigner: String,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
}

/// Host files the measurement inspector mounts into the test image.
pub(crate) struct MeasurementMounts<'a> {
    pub enclave_bin: &'a Path,
    pub signing_key: &'a Path,
    pub network_descriptor: &'a Path,
}

/// Sign the exact test manifest used by every localnet enclave and return the
/// policy inputs before genesis is frozen. This runs only the signer/viewer;
/// it does not launch SGX or make a hardware-attestation claim.
pub(crate) fn inspect_test_sgx_measurement(
    repo: &Path,
    image_id: &DockerImageId,
    sudo: bool,
    mounts: MeasurementMounts<'_>,
) -> Result<TestSgxMeasurement> {
    let enclave_bin = mounts
        .enclave_bin
        .canonicalize()
        .wrap_err("resolve test enclave binary")?;
    let signing_key = mounts
        .signing_key
        .canonicalize()
        .wrap_err("resolve scenario test SGX signing key")?;
    let network_descriptor = mounts
        .network_descriptor
        .canonicalize()
        .wrap_err("resolve seeded network descriptor")?;
    let inspector = repo
        .join("bin/outbe-tee-enclave/gramine/inspect-test-measurement.sh")
        .canonicalize()
        .wrap_err("resolve test SGX measurement inspector")?;
    let output = base_cmd("docker", sudo)
        .args(["run", "--rm", "--entrypoint", "/inspect-test-measurement"])
        .args(pinned_qvl_mount_args()?)
        .args([
            "-v",
            &format!("{}:/app/outbe-tee-enclave:ro", enclave_bin.display()),
        ])
        .args([
            "-v",
            &format!(
                "{}:/opt/outbe/sgx/network-descriptor-v1.bin:ro",
                network_descriptor.display()
            ),
        ])
        .args([
            "-v",
            &format!(
                "{}:/run/secrets/outbe-test-sgx-key.pem:ro",
                signing_key.display()
            ),
        ])
        .args([
            "-v",
            &format!("{}:/inspect-test-measurement:ro", inspector.display()),
        ])
        .arg(image_id.as_str())
        .output()
        .wrap_err("inspect exact test SGX measurement")?;
    if !output.status.success() {
        bail!(
            "test SGX measurement inspection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    parse_test_sgx_measurement(&String::from_utf8_lossy(&output.stdout))
}

pub(super) fn parse_test_sgx_measurement(output: &str) -> Result<TestSgxMeasurement> {
    fn field<'a>(output: &'a str, name: &str) -> Result<&'a str> {
        output
            .lines()
            .find_map(|line| {
                let (actual, value) = line.split_once(':')?;
                actual
                    .trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim())
            })
            .ok_or_else(|| eyre!("SIGSTRUCT output has no {name}"))
    }
    fn measurement(output: &str, name: &str) -> Result<String> {
        let value = field(output, name)?
            .strip_prefix("0x")
            .unwrap_or(field(output, name)?);
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("SIGSTRUCT {name} is not 32 hexadecimal bytes");
        }
        Ok(value.to_ascii_lowercase())
    }
    fn number(output: &str, name: &str) -> Result<u16> {
        let value = field(output, name)?;
        if let Some(hex) = value.strip_prefix("0x") {
            u16::from_str_radix(hex, 16).wrap_err_with(|| format!("parse SIGSTRUCT {name}"))
        } else {
            value
                .parse::<u16>()
                .wrap_err_with(|| format!("parse SIGSTRUCT {name}"))
        }
    }
    Ok(TestSgxMeasurement {
        mrenclave: measurement(output, "mr_enclave")?,
        mrsigner: measurement(output, "mr_signer")?,
        isv_prod_id: number(output, "isv_prod_id")?,
        isv_svn: number(output, "isv_svn")?,
    })
}

/// Build the explicit test-only Gramine image and create one scenario-scoped
/// signing key outside the image. Release images are pre-signed and do not use
/// this adapter.
pub(crate) fn ensure_enclave_image(
    repo: &Path,
    sudo: bool,
    signing_key: &Path,
    established_image_id: Option<&DockerImageId>,
) -> Result<DockerImageId> {
    // The scenario's first setup call creates its signing key and freezes the
    // mutable image tag to one immutable image ID. All later starts must use
    // that retained ID. Another concurrent E2E run may legitimately retag the
    // process-global test image without changing this scenario's SIGSTRUCT.
    if signing_key.exists() {
        let metadata = fs::symlink_metadata(signing_key)?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!(
                "unsafe existing test SGX signing key: {}",
                signing_key.display()
            );
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!(
                "test SGX signing key has unsafe permissions: {}",
                signing_key.display()
            );
        }
        if let Some(image_id) = established_image_id {
            return Ok(image_id.clone());
        }
        return inspect_enclave_image_id(sudo);
    }

    let ctx = repo.join("bin/outbe-tee-enclave/gramine");
    let dockerfile = ctx.join("Dockerfile.test");
    let status = base_cmd("docker", sudo)
        // BuildKit's default provenance attestation changes the top-level
        // manifest-list digest on every otherwise identical build. The exact
        // E2E artifact contract requires one stable immutable image ID across
        // independently launched scenarios, so the test adapter publishes the
        // deterministic platform manifest instead.
        .args(TEST_ENCLAVE_IMAGE_BUILD_ARGS)
        .arg(&dockerfile)
        .args(["-t", TEST_ENCLAVE_IMAGE])
        .arg(&ctx)
        .status()
        .wrap_err("docker build test-only Gramine enclave image")?;
    if !status.success() {
        bail!("docker build {TEST_ENCLAVE_IMAGE} failed");
    }

    let image_id = inspect_enclave_image_id(sudo)?;

    let parent = signing_key
        .parent()
        .ok_or_else(|| eyre!("test SGX signing key has no parent"))?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = signing_key
        .file_name()
        .ok_or_else(|| eyre!("test SGX signing key has no file name"))?;
    let owner = fs::metadata(&parent)?;
    let status = base_cmd("docker", sudo)
        .args(["run", "--rm", "--user"])
        .arg(format!("{}:{}", owner.uid(), owner.gid()))
        .args(["--entrypoint", "gramine-sgx-gen-private-key", "-v"])
        .arg(format!("{}:/keys", parent.display()))
        .arg(image_id.as_str())
        .arg(Path::new("/keys").join(name))
        .status()
        .wrap_err("generate scenario-scoped test SGX signing key")?;
    if !status.success() {
        bail!("test SGX signing key generation failed");
    }
    fs::set_permissions(signing_key, fs::Permissions::from_mode(0o600))?;
    Ok(image_id)
}

fn inspect_enclave_image_id(sudo: bool) -> Result<DockerImageId> {
    let inspected = base_cmd("docker", sudo)
        .args([
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            TEST_ENCLAVE_IMAGE,
        ])
        .output()
        .wrap_err("inspect test-only Gramine enclave image identity")?;
    if !inspected.status.success() {
        bail!(
            "inspect Docker image {TEST_ENCLAVE_IMAGE} failed: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
    }
    DockerImageId::from_inspect_output(&String::from_utf8_lossy(&inspected.stdout))
        .wrap_err("validate test-only Gramine enclave image identity")
}

pub(super) fn pinned_qvl_mount_args() -> Result<Vec<String>> {
    let mut arguments = Vec::with_capacity(PINNED_QVL_RUNTIME_FILES.len() * 2);
    for (source, install_name) in PINNED_QVL_RUNTIME_FILES {
        let source = Path::new(source);
        if !source.is_file() {
            bail!(
                "pinned native-QVL runtime artifact is missing: {}",
                source.display()
            );
        }
        arguments.push("-v".to_owned());
        arguments.push(format!("{}:/qvl/{install_name}:ro", source.display()));
    }
    Ok(arguments)
}
