//! Enclave launch profiles and the commands that execute them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{bail, Result, WrapErr};

use super::base_cmd;
use super::sgx_image::{pinned_qvl_mount_args, DockerImageId};

/// Optional sealed-restart parameters (persistent `/tee` mount + chain-id).
pub(crate) struct SealSpec {
    pub tee_dir: PathBuf,
    pub chain_id_hex: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestRemoteAttestation {
    None,
    Dcap,
}

impl TestRemoteAttestation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Dcap => "dcap",
        }
    }

    pub(super) const fn requires_qvl(self) -> bool {
        matches!(self, Self::Dcap)
    }
}

/// How one enclave is executed. Two distinct, explicitly named profiles - never
/// a fallback from one to the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EnclaveLaunch {
    /// Under Gramine, inside the scenario-pinned test container.
    Gramine {
        /// Immutable Docker image identity resolved before the scenario starts.
        image_id: DockerImageId,
    },
    /// The mock enclave as a plain host process: no container, no LibOS, no
    /// manifest, no attestation. Development only, and only on the mock binary.
    NativeHost,
}

/// Everything needed to launch one enclave.
pub(crate) struct EnclaveSpec {
    pub name: String,
    pub tee_port: u16,
    /// Host enclave binary. Bind-mounted read-only at `/app/outbe-tee-enclave`
    /// under Gramine; executed directly by the native profile.
    pub enclave_bin: PathBuf,
    /// Scenario-scoped test signing key, mounted read-only and never baked into
    /// the Gramine test image. Reused across restarts to preserve MRSIGNER.
    /// Unused by [`EnclaveLaunch::NativeHost`], which signs no manifest.
    pub signing_key: PathBuf,
    /// Seeded-genesis authority measured into every real-DCAP test enclave.
    pub network_descriptor: Option<PathBuf>,
    /// Canonical `NetworkBindingV1` hex for the mock binary. Production launch
    /// profiles leave this absent.
    pub dev_network_binding: Option<String>,
    /// Which execution profile runs this enclave.
    pub launch: EnclaveLaunch,
    pub sudo: bool,
    /// Pass real SGX device nodes through when the host exposes them.
    pub pass_sgx_devices: bool,
    /// Explicit Gramine remote-attestation mode. SGX device availability selects
    /// the runtime, not whether DCAP is enabled.
    pub remote_attestation: TestRemoteAttestation,
    /// `--dkg-seed <hex>` for the container, or `None` (real+seal self-generates).
    pub dkg_seed: Option<String>,
    pub seal: Option<SealSpec>,
    /// Where the container's stdout/stderr are streamed (`<node>/enclave.log`).
    pub log_path: PathBuf,
    /// Log the built `docker` command + container/port under `--debug`.
    pub debug: bool,
}

pub(super) fn build_enclave_command(
    spec: &EnclaveSpec,
    image_id: &DockerImageId,
) -> Result<Command> {
    let mut cmd = base_cmd("docker", spec.sudo);
    cmd.args([
        "run",
        "--name",
        &spec.name,
        "--security-opt",
        "seccomp=unconfined",
        "--network",
        "host",
    ]);
    cmd.arg("--env").arg(format!(
        "OUTBE_TEST_REMOTE_ATTESTATION={}",
        spec.remote_attestation.as_str()
    ));

    // Real SGX: fail closed when no enclave device exists. The same test image
    // also supports the separately selected GramineDirectDev lane. A silent
    // omission of the device here would record a false hardware-mode observation.
    if let Some(enclave_device) = select_sgx_enclave_device(
        spec.pass_sgx_devices,
        Path::new("/dev/sgx_enclave").exists(),
        Path::new("/dev/sgx/enclave").exists(),
    )? {
        cmd.args(["--device", enclave_device]);
        if Path::new("/dev/sgx_provision").exists() {
            cmd.args(["--device", "/dev/sgx_provision"]);
        }
        if Path::new("/var/run/aesmd/aesm.socket").exists() {
            cmd.args([
                "-v",
                "/var/run/aesmd/aesm.socket:/var/run/aesmd/aesm.socket",
            ]);
        }
        if spec.remote_attestation.requires_qvl() {
            cmd.args(pinned_qvl_mount_args()?);
        }
    }

    // Sealed-restart persistent mount.
    if let Some(seal) = &spec.seal {
        fs::create_dir_all(&seal.tee_dir)?;
        let tee_dir = seal.tee_dir.canonicalize().unwrap_or(seal.tee_dir.clone());
        cmd.args(["-v", &format!("{}:/tee", tee_dir.display())]);
    }

    // Host enclave binary (canonicalized so docker gets an absolute path).
    let bin = spec
        .enclave_bin
        .canonicalize()
        .unwrap_or_else(|_| spec.enclave_bin.clone());
    let signing_key = spec
        .signing_key
        .canonicalize()
        .wrap_err("resolve scenario test SGX signing key")?;
    if let Some(descriptor) = &spec.network_descriptor {
        let descriptor = descriptor
            .canonicalize()
            .wrap_err("resolve seeded network descriptor")?;
        cmd.args([
            "-v",
            &format!(
                "{}:/opt/outbe/sgx/network-descriptor-v1.bin:ro",
                descriptor.display()
            ),
        ]);
    }
    cmd.args([
        "-v",
        &format!("{}:/app/outbe-tee-enclave:ro", bin.display()),
        "-v",
        &format!(
            "{}:/run/secrets/outbe-test-sgx-key.pem:ro",
            signing_key.display()
        ),
        image_id.as_str(),
        "--socket",
        &format!("127.0.0.1:{}", spec.tee_port),
    ]);
    if let Some(seed) = &spec.dkg_seed {
        cmd.args(["--dkg-seed", seed]);
    }
    if let Some(binding) = &spec.dev_network_binding {
        cmd.args(["--dev-network-binding", binding]);
    }
    if let Some(seal) = &spec.seal {
        cmd.args(["--tee-dir", "/tee", "--chain-id", &seal.chain_id_hex]);
    }
    Ok(cmd)
}

/// The mock enclave as a plain host process. Same argv contract the Gramine
/// entrypoint passes through (`loader.insecure__use_cmdline_argv`), except that
/// the seal directory is the real host path rather than the container's `/tee`.
pub(super) fn build_native_command(spec: &EnclaveSpec) -> Result<Command> {
    let mut cmd = Command::new(&spec.enclave_bin);
    cmd.args(["--socket", &format!("127.0.0.1:{}", spec.tee_port)]);
    if let Some(seed) = &spec.dkg_seed {
        cmd.args(["--dkg-seed", seed]);
    }
    if let Some(binding) = &spec.dev_network_binding {
        cmd.args(["--dev-network-binding", binding]);
    }
    if let Some(seal) = &spec.seal {
        fs::create_dir_all(&seal.tee_dir)?;
        let tee_dir = seal.tee_dir.canonicalize().unwrap_or(seal.tee_dir.clone());
        cmd.args([
            "--tee-dir",
            &tee_dir.display().to_string(),
            "--chain-id",
            &seal.chain_id_hex,
        ]);
    }
    Ok(cmd)
}

pub(super) fn select_sgx_enclave_device(
    pass_sgx_devices: bool,
    legacy_exists: bool,
    modern_exists: bool,
) -> Result<Option<&'static str>> {
    if !pass_sgx_devices {
        return Ok(None);
    }
    if legacy_exists {
        return Ok(Some("/dev/sgx_enclave"));
    }
    if modern_exists {
        return Ok(Some("/dev/sgx/enclave"));
    }
    bail!("real SGX mode requires /dev/sgx_enclave or /dev/sgx/enclave");
}
