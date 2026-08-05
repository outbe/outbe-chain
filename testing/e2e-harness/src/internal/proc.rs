//! Owned processes and containers, plus the small IO helpers the node/enclave
//! launchers share.
//!
//! Every process the harness launches is **owned**: nodes are held as
//! [`ChildGuard`]s (killed + reaped on drop, no `nohup`/pid-files) and enclave
//! containers as [`EnclaveGuard`]s — the `docker run` runs in the **foreground**
//! (no `-d`) as an owned child, with a `docker rm -f` backstop on drop. Because a
//! fresh `World` is built per scenario, dropping it tears everything down; the
//! `Localnet`/`Nodes` handles that hold these guards are non-`Clone`.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::net::TcpStream;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::hex;
use eyre::{bail, eyre, Result, WrapErr};

const TEST_ENCLAVE_IMAGE: &str = "outbe-tee-enclave-gramine-test";
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

const SENSITIVE_ARG_FLAGS: &[&str] = &[
    "--private-key",
    "--node-private-key",
    "--p2p-secret-key-hex",
    "--dkg-seed",
];

/// Preserve a command's diagnostic shape without emitting secret argument
/// values into CI logs, evidence capture, or agent transcripts.
pub(crate) fn redact_args_for_log(argv: &[String]) -> Vec<String> {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for arg in argv {
        if redact_next {
            redacted.push("<redacted>".to_owned());
            redact_next = false;
            continue;
        }
        if SENSITIVE_ARG_FLAGS.iter().any(|flag| arg == flag) {
            redacted.push(arg.clone());
            redact_next = true;
            continue;
        }
        if let Some((flag, _)) = arg.split_once('=') {
            if SENSITIVE_ARG_FLAGS.contains(&flag) {
                redacted.push(format!("{flag}=<redacted>"));
                continue;
            }
        }
        redacted.push(arg.clone());
    }
    redacted
}

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

/// Build a `Vec<String>` of process arguments from `Display` tokens.
///
/// Every argument is stringified once (`to_string`), so callers write clean
/// literals, ports, and paths — pass a path's `.display()` — without sprinkling
/// `.into()` / `.to_string()` / `.display().to_string()`. `.extend(args![…])` a
/// base list with conditional or role-specific tails.
macro_rules! args {
    ($($x:expr),* $(,)?) => {
        ::std::vec![$($x.to_string()),*]
    };
}
pub(crate) use args;

/// An owned child process: killed and reaped on drop.
#[derive(Debug)]
pub(crate) struct ChildGuard {
    #[allow(dead_code)] // retained for Debug / future diagnostics
    label: String,
    child: Child,
}

impl ChildGuard {
    pub(crate) fn spawn(label: impl Into<String>, mut cmd: Command) -> Result<Self> {
        let label = label.into();
        let child = cmd.spawn().wrap_err_with(|| format!("spawn {label}"))?;
        Ok(Self { label, child })
    }

    /// Whether the child has already exited (non-blocking).
    pub(crate) fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// The OS process id (for `--debug` launch logging).
    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Stop and synchronously reap this owned process. Idempotent after exit.
    pub(crate) fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An owned docker container: `docker rm -f` on drop.
#[derive(Debug)]
pub(crate) struct DockerGuard {
    name: String,
    sudo: bool,
}

impl DockerGuard {
    pub(crate) fn new(name: impl Into<String>, sudo: bool) -> Self {
        Self {
            name: name.into(),
            sudo,
        }
    }
}

impl Drop for DockerGuard {
    fn drop(&mut self) {
        docker_rm(&self.name, self.sudo);
    }
}

/// An owned enclave: the foreground `docker run` child (killed on drop) plus a
/// `docker rm -f` backstop for the container itself. Field order matters — the
/// `docker run` client is dropped first, then the container is force-removed.
#[derive(Debug)]
pub(crate) struct EnclaveGuard {
    #[allow(dead_code)] // owned for its Drop (kills the `docker run` client)
    child: ChildGuard,
    #[allow(dead_code)] // owned for its Drop (`docker rm -f`)
    docker: DockerGuard,
}

/// Optional sealed-restart parameters (persistent `/tee` mount + chain-id).
pub(crate) struct SealSpec {
    pub tee_dir: PathBuf,
    pub chain_id_hex: String,
}

/// Everything needed to launch one enclave container (mirrors `run-testnet.sh:215-293`).
pub(crate) struct EnclaveSpec {
    pub name: String,
    pub tee_port: u16,
    /// Host enclave binary bind-mounted read-only at `/app/outbe-tee-enclave`.
    pub enclave_bin: PathBuf,
    /// Scenario-scoped test signing key, mounted read-only and never baked into
    /// the Gramine test image. Reused across restarts to preserve MRSIGNER.
    pub signing_key: PathBuf,
    /// Immutable Docker image identity resolved before the scenario starts.
    pub image_id: DockerImageId,
    pub sudo: bool,
    /// Pass real SGX device nodes through when the host exposes them.
    pub pass_sgx_devices: bool,
    /// `--dkg-seed <hex>` for the container, or `None` (real+seal self-generates).
    pub dkg_seed: Option<String>,
    pub seal: Option<SealSpec>,
    /// Where the container's stdout/stderr are streamed (`<node>/enclave.log`).
    pub log_path: PathBuf,
    /// Log the built `docker` command + container/port under `--debug`.
    pub debug: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestSgxMeasurement {
    pub mrenclave: String,
    pub mrsigner: String,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
}

/// Sign the exact test manifest used by every localnet enclave and return the
/// policy inputs before genesis is frozen. This runs only the signer/viewer;
/// it does not launch SGX or make a hardware-attestation claim.
pub(crate) fn inspect_test_sgx_measurement(
    repo: &Path,
    enclave_bin: &Path,
    signing_key: &Path,
    image_id: &DockerImageId,
    sudo: bool,
) -> Result<TestSgxMeasurement> {
    let enclave_bin = enclave_bin
        .canonicalize()
        .wrap_err("resolve test enclave binary")?;
    let signing_key = signing_key
        .canonicalize()
        .wrap_err("resolve scenario test SGX signing key")?;
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

fn parse_test_sgx_measurement(output: &str) -> Result<TestSgxMeasurement> {
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
) -> Result<DockerImageId> {
    // The first setup call creates the scenario signing key and freezes the
    // mutable image tag. Join/restart calls must inspect that same tag instead
    // of rebuilding it: a rebuild can produce a new image ID and would no
    // longer match the SIGSTRUCT already bound into genesis. A fresh scenario
    // has no key and therefore always rebuilds from the current worktree.
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
        return inspect_enclave_image_id(sudo);
    }

    let ctx = repo.join("bin/outbe-tee-enclave/gramine");
    let dockerfile = ctx.join("Dockerfile.test");
    let status = base_cmd("docker", sudo)
        .args(["build", "-f"])
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

fn build_enclave_command(spec: &EnclaveSpec) -> Result<Command> {
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

    // Real SGX: fail closed when no enclave device exists. The same test image
    // also supports the separately selected GramineDirectDev lane, so silently
    // omitting the device here would record a false hardware-mode observation.
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
        cmd.args(pinned_qvl_mount_args()?);
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
    cmd.args([
        "-v",
        &format!("{}:/app/outbe-tee-enclave:ro", bin.display()),
        "-v",
        &format!(
            "{}:/run/secrets/outbe-test-sgx-key.pem:ro",
            signing_key.display()
        ),
        spec.image_id.as_str(),
        "--socket",
        &format!("127.0.0.1:{}", spec.tee_port),
    ]);
    if let Some(seed) = &spec.dkg_seed {
        cmd.args(["--dkg-seed", seed]);
    }
    if let Some(seal) = &spec.seal {
        cmd.args(["--tee-dir", "/tee", "--chain-id", &seal.chain_id_hex]);
    }
    Ok(cmd)
}

fn pinned_qvl_mount_args() -> Result<Vec<String>> {
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

/// `docker run` the enclave in the **foreground** (no `-d`) as an owned child,
/// returning a guard that kills the client + `docker rm -f`s the container on drop.
/// The caller waits on socket readiness with [`wait_tcp`].
pub(crate) fn spawn_enclave(spec: EnclaveSpec) -> Result<EnclaveGuard> {
    // Remove any stale container of the same name first.
    docker_rm(&spec.name, spec.sudo);

    let mut cmd = build_enclave_command(&spec)?;

    // Foreground: own the `docker run` child, stream its logs to <node>/enclave.log.
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)
        .wrap_err_with(|| format!("open {}", spec.log_path.display()))?;
    let log2 = log.try_clone()?;
    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .stdin(Stdio::null());

    if spec.debug {
        let prog = cmd.get_program().to_string_lossy().into_owned();
        let rest: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let rest = redact_args_for_log(&rest);
        eprintln!(
            "[localnet] enclave {} (tee {}): {prog} {}",
            spec.name,
            spec.tee_port,
            rest.join(" ")
        );
        eprintln!("           log: {}", spec.log_path.display());
    }

    let child = ChildGuard::spawn(format!("enclave {}", spec.name), cmd)?;
    Ok(EnclaveGuard {
        child,
        docker: DockerGuard::new(spec.name, spec.sudo),
    })
}

fn select_sgx_enclave_device(
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
    bail!("real SGX mode requires /dev/sgx_enclave or /dev/sgx/enclave")
}

/// A `Command` for `program`, `sudo`-wrapped when requested.
pub(crate) fn base_cmd(program: &str, sudo: bool) -> Command {
    if sudo {
        let mut c = Command::new("sudo");
        c.arg(program);
        c
    } else {
        Command::new(program)
    }
}

/// Best-effort `docker rm -f <name>`.
pub(crate) fn docker_rm(name: &str, sudo: bool) {
    let _ = base_cmd("docker", sudo)
        .args(["rm", "-f", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Redirect a spawned node's stdout+stderr to `<node_dir>/node.log` (append),
/// with no stdin — the owned-process analogue of the shell `>> node.log 2>&1`.
pub(crate) fn attach_log(cmd: &mut Command, node_dir: &Path) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(node_dir.join("node.log"))?;
    let log2 = log.try_clone()?;
    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .stdin(Stdio::null());
    Ok(())
}

/// Whitespace-stripped file contents (`tr -d '[:space:]'`).
pub(crate) fn read_trimmed(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect())
}

/// The `0x`-prefixed EVM key from `<vd>/evm-key.hex`.
pub(crate) fn read_evm_key(vd: &Path) -> Result<String> {
    let hex = read_trimmed(&vd.join("evm-key.hex"))?;
    Ok(if hex.starts_with("0x") {
        hex
    } else {
        format!("0x{hex}")
    })
}

/// 32 random bytes as hex (was `python3 secrets.token_hex(32)` / `openssl rand`).
pub(crate) fn random_hex_32() -> Result<String> {
    let mut buf = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(hex::encode(buf))
}

/// The first run of `>= min_len` hex digits in `s` (keygen pubkey/signature).
pub(crate) fn first_hex(s: &str, min_len: usize) -> Option<String> {
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            cur.push(c);
        } else {
            if cur.len() >= min_len {
                return Some(cur);
            }
            cur.clear();
        }
    }
    (cur.len() >= min_len).then_some(cur)
}

/// Wait for a TCP listener on `127.0.0.1:port` (enclave socket readiness).
pub(crate) fn wait_tcp(port: u16, tries: u32) -> bool {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..tries {
        if TcpStream::connect(&addr).is_ok() {
            return true;
        }
        sleep(Duration::from_millis(100));
    }
    false
}

/// Run `program args…`, returning stdout on success or an error carrying stderr.
pub(crate) fn run_capture(program: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .wrap_err_with(|| format!("run {}", program.display()))?;
    if !out.status.success() {
        return Err(eyre!(
            "{} {:?} failed: {}",
            program.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigstruct_parser_requires_exact_measurements_and_versions() {
        let parsed = parse_test_sgx_measurement(&format!(
            "mr_enclave: 0x{}\nmr_signer: {}\nisv_prod_id: 7\nisv_svn: 0x0008\n",
            "ab".repeat(32),
            "cd".repeat(32)
        ))
        .unwrap();
        assert_eq!(parsed.mrenclave, "ab".repeat(32));
        assert_eq!(parsed.mrsigner, "cd".repeat(32));
        assert_eq!(parsed.isv_prod_id, 7);
        assert_eq!(parsed.isv_svn, 8);
        assert!(parse_test_sgx_measurement("mr_enclave: 00\n").is_err());
    }

    #[test]
    fn docker_image_identity_accepts_only_a_canonical_sha256_id() {
        let digest = format!("sha256:{}\n", "ab".repeat(32));
        let identity =
            DockerImageId::from_inspect_output(&digest).expect("canonical Docker image ID");
        assert_eq!(identity.as_str(), digest.trim());

        for invalid in [
            TEST_ENCLAVE_IMAGE.to_owned(),
            format!("sha256:{}", "ab".repeat(31)),
            format!("sha256:{}", "AB".repeat(32)),
            format!("sha256:{}z", "ab".repeat(31)),
        ] {
            assert!(
                DockerImageId::from_inspect_output(&invalid).is_err(),
                "accepted non-canonical Docker image identity: {invalid}"
            );
        }
    }

    #[test]
    fn enclave_command_uses_the_pinned_image_id_instead_of_the_mutable_tag() {
        let root = tempfile::tempdir().expect("temporary enclave command inputs");
        let enclave_bin = root.path().join("outbe-tee-enclave");
        let signing_key = root.path().join("signing-key.pem");
        fs::write(&enclave_bin, b"binary").expect("write enclave binary fixture");
        fs::write(&signing_key, b"key").expect("write signing key fixture");
        let image_id = DockerImageId::from_inspect_output(&format!("sha256:{}", "cd".repeat(32)))
            .expect("pinned image ID");
        let spec = EnclaveSpec {
            name: "validator-0-tee".to_owned(),
            tee_port: 19500,
            enclave_bin,
            signing_key,
            image_id: image_id.clone(),
            sudo: false,
            pass_sgx_devices: false,
            dkg_seed: None,
            seal: None,
            log_path: root.path().join("enclave.log"),
            debug: false,
        };

        let command = build_enclave_command(&spec).expect("build enclave command");
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(arguments
            .iter()
            .any(|argument| argument == image_id.as_str()));
        assert!(!arguments
            .iter()
            .any(|argument| argument == TEST_ENCLAVE_IMAGE));
    }

    #[test]
    fn first_hex_runs() {
        assert_eq!(
            first_hex("pub: abcdef0123", 6),
            Some("abcdef0123".to_string())
        );
        assert_eq!(first_hex("0xDEAD 12", 4), Some("DEAD".to_string()));
        assert_eq!(first_hex("short ab", 4), None);
    }

    #[test]
    fn args_stringifies_display_tokens() {
        let port: u16 = 8545;
        let path = PathBuf::from("/tmp/x/data");
        let a = args!["node", "--http.port", port, "--datadir", path.display()];
        assert_eq!(
            a,
            vec!["node", "--http.port", "8545", "--datadir", "/tmp/x/data"]
        );
    }

    #[test]
    fn diagnostic_argv_redacts_secret_values_in_both_supported_forms() {
        let argv = vec![
            "node".to_owned(),
            "--private-key".to_owned(),
            "secret-a".to_owned(),
            "--p2p-secret-key-hex=secret-b".to_owned(),
            "--dkg-seed".to_owned(),
            "secret-c".to_owned(),
            "--rpc-url".to_owned(),
            "http://127.0.0.1:8545".to_owned(),
        ];
        let redacted = redact_args_for_log(&argv);
        assert_eq!(
            redacted,
            vec![
                "node",
                "--private-key",
                "<redacted>",
                "--p2p-secret-key-hex=<redacted>",
                "--dkg-seed",
                "<redacted>",
                "--rpc-url",
                "http://127.0.0.1:8545",
            ]
        );
        let rendered = redacted.join(" ");
        for secret in ["secret-a", "secret-b", "secret-c"] {
            assert!(!rendered.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn real_mode_fails_closed_without_an_sgx_enclave_device() {
        assert!(select_sgx_enclave_device(true, false, false).is_err());
        assert_eq!(
            select_sgx_enclave_device(false, true, true).unwrap(),
            None,
            "gramine-direct must not inherit host SGX devices"
        );
    }

    #[test]
    fn real_sgx_manifest_bounds_threads_for_four_validator_e2e() {
        let manifest = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../bin/outbe-tee-enclave/gramine/outbe-tee-enclave.manifest.template"
        ));
        let max_threads = manifest
            .lines()
            .find_map(|line| line.trim().strip_prefix("sgx.max_threads = "))
            .and_then(|value| value.parse::<u32>().ok())
            .expect("manifest declares numeric sgx.max_threads");
        assert!(
            max_threads <= 16,
            "four real enclaves must not reserve more than 64 SGX thread slots"
        );
    }
}
