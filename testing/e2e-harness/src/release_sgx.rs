//! Hardware-SGX acceptance test for one exact, already-published release image.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read as _;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::B256;
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use cucumber::cli;
use cucumber::writer::Stats as _;
use cucumber::World as _;
use eyre::{bail, eyre, Result, WrapErr as _};
use outbe_evm::tee_attestation_activation::DcapChainSpecBindingV1;
use outbe_primitives::tee_attestation_v1::NetworkBindingV1;
use outbe_tee::node_host::{
    connect_committed_node_host_enclave, connect_or_initialize_node_host_enclave,
    NodeHostIdentityV1,
};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use walkdir::WalkDir;

use outbe_tee::release_dcap_artifacts::ReleaseDcapArtifactSetV1;

use crate::release_dcap::{add_sgx_runtime_mounts, ReleaseDcapNetworkArg};

const RELEASE_FEATURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/release-features");

#[derive(clap::Args, Clone, Debug)]
pub struct ReleaseSgxCli {
    /// Canonical production network bound by the bundle and runtime identity.
    #[arg(long, value_enum)]
    network: ReleaseDcapNetworkArg,
    /// Exact published OCI reference, including @sha256:<digest>.
    #[arg(long)]
    image: String,
    /// Extracted signed SGX bundle downloaded from the protected release job.
    #[arg(long)]
    bundle: PathBuf,
    /// Exact final network genesis generated from the approved seed and bundle measurements.
    #[arg(long)]
    genesis: PathBuf,
    /// Canonical JSON evidence written only after the complete scenario passes.
    #[arg(long)]
    evidence: PathBuf,
    /// Repository checkout matching the release tag.
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Keep temporary mismatch bundles and logs after a successful run.
    #[arg(long)]
    keep_work_dir: bool,
}

#[derive(Clone, Debug)]
struct ReleaseConfig {
    bundle: PathBuf,
    evidence: PathBuf,
    genesis: PathBuf,
    image: String,
    image_digest: String,
    keep_work_dir: bool,
    network: ReleaseDcapArtifactSetV1,
    repo: PathBuf,
    work_dir: PathBuf,
}

impl ReleaseConfig {
    fn resolve(cli: &ReleaseSgxCli) -> Result<Self> {
        let image_digest = exact_image_digest(&cli.image)?.to_owned();
        let repo = fs::canonicalize(cli.repo.clone().unwrap_or_else(default_repo))
            .wrap_err("resolve repository checkout")?;
        let bundle = fs::canonicalize(&cli.bundle).wrap_err("resolve signed SGX bundle")?;
        let genesis = fs::canonicalize(&cli.genesis).wrap_err("resolve exact final genesis")?;
        let evidence = absolute_path(&cli.evidence)?;
        if evidence.starts_with(&bundle) {
            bail!("hardware evidence must be outside the signed bundle");
        }
        let work_dir = std::env::temp_dir().join(format!(
            "outbe-release-sgx-e2e-{}-{}",
            unix_seconds(),
            std::process::id()
        ));
        fs::create_dir(&work_dir).wrap_err("create release SGX E2E work directory")?;
        Ok(Self {
            bundle,
            evidence,
            genesis,
            image: cli.image.clone(),
            image_digest,
            keep_work_dir: cli.keep_work_dir,
            network: cli.network.artifact_set(),
            repo,
            work_dir,
        })
    }
}

static CONFIG: OnceLock<ReleaseConfig> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct Measurements {
    debug: bool,
    isv_prod_id: u16,
    isv_svn: u16,
    mrenclave: String,
    mrsigner: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BundleFile {
    path: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GramineIdentity {
    builder_image: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BundleManifest {
    authorization_scope: String,
    files: Vec<BundleFile>,
    gramine: GramineIdentity,
    measurements: Measurements,
    sealed_state_schema: u32,
    sigstruct_date: String,
}

#[derive(Debug, cucumber::World)]
pub struct ReleaseSgxWorld {
    cfg: ReleaseConfig,
    manifest: BundleManifest,
    probe: Option<String>,
    first_start: Option<String>,
    second_start: Option<String>,
    mismatch_start: Option<String>,
    mismatch_image: String,
    network_binding: NetworkBindingV1,
    node_data_dir: PathBuf,
    node_signer: PrivateKeySigner,
    seal_dir: PathBuf,
}

impl Default for ReleaseSgxWorld {
    fn default() -> Self {
        let cfg = CONFIG
            .get()
            .expect("release SGX environment is set")
            .clone();
        let manifest =
            read_json::<BundleManifest>(&cfg.bundle.join(cfg.network.bundle_manifest_path()))
                .expect("read signed SGX bundle manifest");
        let network_binding = DcapChainSpecBindingV1::from_genesis_path(&cfg.genesis)
            .expect("read final release genesis binding")
            .policy
            .network_binding();
        let seal_dir = cfg.work_dir.join("sealed-state");
        let node_data_dir = cfg.work_dir.join("node-data");
        fs::create_dir(&seal_dir).expect("create sealed-state directory");
        fs::create_dir(&node_data_dir).expect("create NodeHost state directory");
        fs::set_permissions(&seal_dir, fs::Permissions::from_mode(0o700))
            .expect("protect sealed-state directory");
        fs::set_permissions(&node_data_dir, fs::Permissions::from_mode(0o700))
            .expect("protect NodeHost state directory");
        Self {
            mismatch_image: format!("outbe-release-sgx-mismatch:{}", std::process::id()),
            cfg,
            manifest,
            probe: None,
            first_start: None,
            second_start: None,
            mismatch_start: None,
            network_binding,
            node_data_dir,
            node_signer: random_node_signer().expect("create ephemeral NodeHost signer"),
            seal_dir,
        }
    }
}

impl Drop for ReleaseSgxWorld {
    fn drop(&mut self) {
        let prefix = container_prefix();
        for suffix in ["first", "second", "mismatch"] {
            let _ = Command::new("docker")
                .args(["rm", "-f", &format!("{prefix}-{suffix}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = Command::new("docker")
            .args(["image", "rm", "-f", &self.mismatch_image])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !self.cfg.keep_work_dir {
            let _ = fs::remove_dir_all(&self.cfg.work_dir);
        }
    }
}

pub async fn run() {
    let opts = cli::Opts::<_, _, _, ReleaseSgxCli>::parsed();
    let cfg = ReleaseConfig::resolve(&opts.custom).unwrap_or_else(|error| {
        eprintln!("outbe-release-sgx-e2e: invalid environment: {error:#}");
        std::process::exit(2);
    });
    eprintln!("outbe-release-sgx-e2e: exact image {}", cfg.image);
    eprintln!("outbe-release-sgx-e2e: work dir {}", cfg.work_dir.display());
    CONFIG
        .set(cfg)
        .expect("release SGX environment is set once");

    let writer = ReleaseSgxWorld::cucumber()
        .max_concurrent_scenarios(1)
        .with_cli(opts)
        .run(RELEASE_FEATURES)
        .await;
    if writer.execution_has_failed() {
        eprintln!(
            "outbe-release-sgx-e2e: failed steps={}, parse errors={}, hook errors={}",
            writer.failed_steps(),
            writer.parsing_errors(),
            writer.hook_errors()
        );
        std::process::exit(1);
    }
}

#[cucumber::given("an exact signed production SGX bundle and published image")]
fn exact_release(world: &mut ReleaseSgxWorld) {
    assert_eq!(
        world.manifest.authorization_scope,
        world.cfg.network.label()
    );
    assert!(!world.manifest.measurements.debug);
    assert_eq!(world.manifest.sealed_state_schema, 3);
    assert_eq!(world.manifest.measurements.isv_svn, 2);
    command_ok(
        Command::new("docker")
            .args(["image", "inspect", &world.cfg.image])
            .current_dir(&world.cfg.repo),
        "inspect exact published image",
    )
    .expect("exact published image must already be pulled and addressable by digest");
}

#[cucumber::then("the signed bundle and immutable runtime layout verify")]
fn verify_bundle_and_runtime(world: &mut ReleaseSgxWorld) {
    command_ok(
        Command::new("cargo")
            .args([
                "xtask",
                "release",
                "sgx",
                "verify",
                "--network",
                world.cfg.network.label(),
                "--bundle",
            ])
            .arg(&world.cfg.bundle)
            .arg("--genesis")
            .arg(&world.cfg.genesis)
            .current_dir(&world.cfg.repo),
        "verify exact signed SGX bundle",
    )
    .expect("signed bundle verification");

    for file in &world.manifest.files {
        let lower = file.path.to_ascii_lowercase();
        assert!(
            !lower.contains("private")
                && !lower.ends_with(".pem")
                && !lower.contains("enclave-key"),
            "release bundle contains signing material: {}",
            file.path
        );
    }
    let inspect = command_output(
        Command::new("docker")
            .args([
                "image",
                "inspect",
                "--format",
                "{{json .Config.Entrypoint}}",
                &world.cfg.image,
            ])
            .current_dir(&world.cfg.repo),
        "inspect release image entrypoint",
    )
    .expect("inspect entrypoint");
    assert_eq!(
        inspect.trim(),
        "[\"/opt/outbe/sgx/bin/outbe-tee-enclave-launch\"]"
    );
    let entrypoint = fs::read_to_string(
        world
            .cfg
            .bundle
            .join("rootfs/opt/outbe/sgx/bin/outbe-tee-enclave-launch"),
    )
    .expect("read release entrypoint");
    for forbidden in [
        "gramine-sgx-sign",
        "gramine-sgx-gen-private-key",
        "gramine-direct",
    ] {
        assert!(
            !entrypoint.contains(forbidden),
            "runtime entrypoint contains {forbidden}"
        );
    }
}

#[cucumber::when("the published image probes hardware SGX")]
fn probe_hardware(world: &mut ReleaseSgxWorld) {
    let mut command = Command::new("docker");
    command.arg("run").arg("--rm");
    add_sgx_runtime_mounts(&mut command).expect("SGX runtime mounts");
    command.args([&world.cfg.image, "--probe-attestation"]);
    let probe = command_combined_output(&mut command, "probe release image")
        .expect("release hardware probe");
    assert!(
        !probe.contains("dcap_quote: UNAVAILABLE"),
        "release hardware probe has no DCAP quote:\n{probe}"
    );
    world.probe = Some(probe);
}

#[cucumber::then("the hardware report matches the signed enclave measurements")]
fn report_matches(world: &mut ReleaseSgxWorld) {
    let probe = world.probe.as_ref().expect("probe output");
    assert!(
        probe.contains("gramine-sgx"),
        "not a gramine-sgx run:\n{probe}"
    );
    assert!(
        probe.contains("sealing_key(mrsigner): 16 bytes")
            && probe.contains("sealing_key(mrenclave): 16 bytes"),
        "real EGETKEY evidence missing:\n{probe}"
    );
    let observed = parse_local_report(probe).expect("local SGX report measurements");
    assert_eq!(observed, world.manifest.measurements);
}

#[cucumber::when("the published image starts twice with one sealed identity directory")]
fn restart_same_signer(world: &mut ReleaseSgxWorld) {
    world.first_start = Some(
        run_initializing_start(
            &world.cfg.image,
            &world.seal_dir,
            &world.node_data_dir,
            &world.network_binding,
            &world.node_signer,
            world
                .cfg
                .network
                .network()
                .chain_id()
                .expect("assigned network"),
            "first",
            &world.cfg.work_dir,
        )
        .expect("first release enclave start"),
    );
    world.second_start = Some(
        run_restored_start(
            &world.cfg.image,
            &world.seal_dir,
            &world.node_data_dir,
            world
                .cfg
                .network
                .network()
                .chain_id()
                .expect("assigned network"),
            "second",
            &world.cfg.work_dir,
        )
        .expect("second release enclave start"),
    );
}

#[cucumber::then("the second start restores the same-signer sealed identity")]
fn same_signer_restores(world: &mut ReleaseSgxWorld) {
    let first = world.first_start.as_ref().expect("first start log");
    let second = world.second_start.as_ref().expect("second start log");
    assert!(first.contains("self-generated (fresh, pending network-bound seal)"));
    assert!(second.contains("self-generated (restored from network-bound seal)"));
    assert!(!second.contains("identity init failed"));
}

#[cucumber::when("an artifact in the signed bundle is substituted")]
fn substitute_artifact(world: &mut ReleaseSgxWorld) {
    let tampered = world.cfg.work_dir.join("tampered-bundle");
    copy_tree(&world.cfg.bundle, &tampered).expect("copy tamper fixture");
    let signature = tampered.join("rootfs/opt/outbe/sgx/outbe-tee-enclave.sig");
    let mut bytes = fs::read(&signature).expect("read signature");
    bytes[0] ^= 0x01;
    fs::write(&signature, bytes).expect("substitute signature byte");
}

#[cucumber::then("release verification rejects the substituted artifact")]
fn substitution_rejected(world: &mut ReleaseSgxWorld) {
    let tampered_bundle = world.cfg.work_dir.join("tampered-bundle");
    let output = Command::new("cargo")
        .args([
            "xtask",
            "release",
            "sgx",
            "verify",
            "--network",
            world.cfg.network.label(),
            "--bundle",
        ])
        .arg(&tampered_bundle)
        .arg("--genesis")
        .arg(&world.cfg.genesis)
        .current_dir(&world.cfg.repo)
        .output()
        .expect("run tampered verification");
    assert!(
        !output.status.success(),
        "substituted release artifact was accepted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tampered_signature = tampered_bundle.join("rootfs/opt/outbe/sgx/outbe-tee-enclave.sig");
    assert!(
        stderr.contains("checksum mismatch")
            && stderr.contains(&tampered_signature.display().to_string()),
        "verification failed for an unexpected reason: {stderr}"
    );
}

#[cucumber::when("the rendered manifest is signed by a different test key")]
fn resign_with_different_key(world: &mut ReleaseSgxWorld) {
    let mismatch = world.cfg.work_dir.join("mismatch-bundle");
    copy_tree(&world.cfg.bundle, &mismatch).expect("copy mismatch bundle");
    let mount = format!("{}:/work", mismatch.display());
    let user = container_user().expect("resolve mismatch container user");

    command_ok(
        Command::new("docker").args([
            "run",
            "--rm",
            "-v",
            &mount,
            "--user",
            &user,
            "--entrypoint",
            "gramine-sgx-gen-private-key",
            &world.manifest.gramine.builder_image,
            "/work/mismatch-key.pem",
        ]),
        "generate mismatch-only SGX key",
    )
    .expect("generate mismatch key");
    fs::remove_file(mismatch.join("rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest.sgx"))
        .expect("remove protected-job signed manifest copy");
    fs::remove_file(mismatch.join("rootfs/opt/outbe/sgx/outbe-tee-enclave.sig"))
        .expect("remove protected-job SIGSTRUCT copy");
    command_ok(
        Command::new("docker").args([
            "run",
            "--rm",
            "-v",
            &mount,
            "--user",
            &user,
            "--entrypoint",
            "gramine-sgx-sign",
            &world.manifest.gramine.builder_image,
            "--date",
            &world.manifest.sigstruct_date,
            "--key",
            "/work/mismatch-key.pem",
            "--chroot",
            "/work/rootfs",
            "--libpal",
            "/work/rootfs/opt/outbe/sgx/gramine/libpal.so",
            "--manifest",
            "/work/rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest",
            "--output",
            "/work/rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest.sgx",
        ]),
        "sign mismatch-only SGX bundle",
    )
    .expect("sign mismatch bundle");
    fs::remove_file(mismatch.join("mismatch-key.pem")).expect("remove mismatch private key");

    command_ok(
        Command::new("docker")
            .args(["build", "--file"])
            .arg(
                world
                    .cfg
                    .repo
                    .join("bin/outbe-tee-enclave/gramine/Dockerfile"),
            )
            .args(["--tag", &world.mismatch_image])
            .arg(&mismatch),
        "build mismatch-signer test image",
    )
    .expect("build mismatch image");
    world.mismatch_start = Some(
        run_mismatched_signer_start(
            &world.mismatch_image,
            &world.seal_dir,
            world
                .cfg
                .network
                .network()
                .chain_id()
                .expect("assigned network"),
            "mismatch",
            &world.cfg.work_dir,
        )
        .expect("mismatch-signer enclave start"),
    );
}

#[cucumber::then("the prior sealed identity is not silently restored by the different signer")]
fn mismatch_does_not_restore(world: &mut ReleaseSgxWorld) {
    let log = world.mismatch_start.as_ref().expect("mismatch start log");
    assert!(
        log.contains("identity init failed:")
            && log.contains("sealed identity at")
            && log.contains("is unusable and will not be replaced"),
        "mismatch was not detected:\n{log}"
    );
    assert!(!log.contains("self-generated (fresh, pending network-bound seal)"));
    assert!(!log.contains("self-generated (restored from network-bound seal)"));
}

#[cucumber::then("canonical hardware release evidence is written")]
fn write_evidence(world: &mut ReleaseSgxWorld) {
    let dcap = world
        .probe
        .as_deref()
        .is_some_and(|output| !output.contains("dcap_quote: UNAVAILABLE"));
    let document = serde_json::json!({
        "checks": [
            "exact-image-by-digest",
            "signed-bundle-verification",
            "no-runtime-signing",
            "local-report-measurements",
            "egetkey-mrsigner-and-mrenclave",
            "same-signer-seal-restart-unseal",
            "artifact-substitution-rejected",
            "different-signer-seal-rejected"
        ],
        "environment": {
            "backend": "gramine-sgx",
            "dcap": dcap,
            "hardware_sgx": true
        },
        "image": {
            "digest": {"algorithm": "sha256", "value": world.cfg.image_digest},
            "reference": world.cfg.image
        },
        "measurements": world.manifest.measurements,
        "network": {
            "chain_id": world.cfg.network.network().chain_id().expect("assigned network"),
            "chain_name": world.cfg.network.network().chain_name(),
            "profile": world.cfg.network.label()
        },
        "result": "passed",
        "schema_version": "1.0.0"
    });
    if let Some(parent) = world.cfg.evidence.parent() {
        fs::create_dir_all(parent).expect("create evidence parent");
    }
    fs::write(&world.cfg.evidence, canonical_json(document)).expect("write hardware evidence");
}

fn exact_image_digest(image: &str) -> Result<&str> {
    let (_, digest) = image
        .rsplit_once("@sha256:")
        .ok_or_else(|| eyre!("release image must be addressed by @sha256 digest"))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("release image digest must be 64 lowercase hexadecimal characters");
    }
    Ok(digest)
}

fn parse_local_report(output: &str) -> Result<Measurements> {
    let line = output
        .lines()
        .find(|line| line.trim_start().starts_with("local_report: mrenclave="))
        .ok_or_else(|| eyre!("hardware probe did not emit local_report measurements"))?;
    let fields = line
        .trim()
        .strip_prefix("local_report: ")
        .expect("matched prefix")
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect::<BTreeMap<_, _>>();
    Ok(Measurements {
        debug: false,
        isv_prod_id: fields
            .get("isv_prod_id")
            .ok_or_else(|| eyre!("local report lacks ISVPRODID"))?
            .parse()?,
        isv_svn: fields
            .get("isv_svn")
            .ok_or_else(|| eyre!("local report lacks ISVSVN"))?
            .parse()?,
        mrenclave: validate_measurement(fields.get("mrenclave"), "MRENCLAVE")?,
        mrsigner: validate_measurement(fields.get("mrsigner"), "MRSIGNER")?,
    })
}

fn validate_measurement(value: Option<&&str>, name: &str) -> Result<String> {
    let value = value.ok_or_else(|| eyre!("local report lacks {name}"))?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("local report {name} is not 32 lowercase hexadecimal bytes");
    }
    Ok((*value).to_owned())
}

struct RunningReleaseEnclave {
    child: Option<Child>,
    log_path: PathBuf,
    name: String,
}

impl RunningReleaseEnclave {
    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("release enclave child exists")
    }

    fn stop_and_collect(mut self) -> Result<String> {
        let _ = Command::new("docker")
            .args(["stop", "--time", "3", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        fs::read_to_string(&self.log_path).wrap_err("read release enclave log")
    }
}

impl Drop for RunningReleaseEnclave {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

fn start_release_enclave(
    image: &str,
    seal_dir: &Path,
    chain_id: u64,
    suffix: &str,
    endpoint: &str,
    work_dir: &Path,
) -> Result<RunningReleaseEnclave> {
    let name = format!("{}-{suffix}", container_prefix());
    let log_path = work_dir.join(format!("{suffix}.log"));
    let log = File::create(&log_path).wrap_err("create enclave log")?;
    let mut command = Command::new("docker");
    let chain_id = format!("0x{chain_id:064x}");
    command.args(["run", "--rm", "--name", &name, "--network", "host"]);
    add_sgx_runtime_mounts(&mut command)?;
    command
        .args(["-v", &format!("{}:/var/lib/outbe/tee", seal_dir.display())])
        .arg(image)
        .args([
            "--socket",
            endpoint,
            "--tee-dir",
            "/var/lib/outbe/tee",
            "--chain-id",
            &chain_id,
        ])
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let child = command
        .spawn()
        .wrap_err("start release enclave container")?;
    Ok(RunningReleaseEnclave {
        child: Some(child),
        log_path,
        name,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_initializing_start(
    image: &str,
    seal_dir: &Path,
    node_data_dir: &Path,
    network_binding: &NetworkBindingV1,
    node_signer: &PrivateKeySigner,
    chain_id: u64,
    suffix: &str,
    work_dir: &Path,
) -> Result<String> {
    let endpoint = allocate_loopback_endpoint()?;
    let mut running =
        start_release_enclave(image, seal_dir, chain_id, suffix, &endpoint, work_dir)?;
    let log_path = running.log_path.clone();
    wait_for_marker(
        running.child_mut(),
        &log_path,
        "self-generated (fresh, pending network-bound seal)",
        Duration::from_secs(120),
    )?;
    let identity = node_host_identity(network_binding, node_signer)?;
    let client =
        connect_or_initialize_node_host_enclave(&endpoint, node_data_dir, identity, |hash| {
            node_signature(node_signer, hash)
        })?;
    drop(client);
    if !seal_dir.join("sealed_identity.bin").is_file() {
        bail!("NodeHost initialization did not persist the network-bound identity seal");
    }
    running.stop_and_collect()
}

fn run_restored_start(
    image: &str,
    seal_dir: &Path,
    node_data_dir: &Path,
    chain_id: u64,
    suffix: &str,
    work_dir: &Path,
) -> Result<String> {
    let endpoint = allocate_loopback_endpoint()?;
    let mut running =
        start_release_enclave(image, seal_dir, chain_id, suffix, &endpoint, work_dir)?;
    let log_path = running.log_path.clone();
    wait_for_marker(
        running.child_mut(),
        &log_path,
        "self-generated (restored from network-bound seal)",
        Duration::from_secs(120),
    )?;
    let client = connect_committed_node_host_enclave(&endpoint, node_data_dir)?;
    drop(client);
    running.stop_and_collect()
}

fn run_mismatched_signer_start(
    image: &str,
    seal_dir: &Path,
    chain_id: u64,
    suffix: &str,
    work_dir: &Path,
) -> Result<String> {
    let endpoint = allocate_loopback_endpoint()?;
    let mut running =
        start_release_enclave(image, seal_dir, chain_id, suffix, &endpoint, work_dir)?;
    let log_path = running.log_path.clone();
    let (status, output) = wait_for_exit(running.child_mut(), &log_path, Duration::from_secs(120))?;
    if status.success() {
        bail!("different-signer enclave unexpectedly accepted the prior sealed identity");
    }
    drop(running);
    Ok(output)
}

fn wait_for_marker(
    child: &mut Child,
    path: &Path,
    marker: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let content = fs::read_to_string(path).unwrap_or_default();
        if content.contains(marker) {
            return Ok(content);
        }
        if let Some(status) = child.try_wait()? {
            bail!("release enclave exited before {marker:?} with {status}:\n{content}");
        }
        if Instant::now() >= deadline {
            bail!("release enclave did not emit {marker:?} within {timeout:?}:\n{content}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_exit(
    child: &mut Child,
    path: &Path,
    timeout: Duration,
) -> Result<(ExitStatus, String)> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, fs::read_to_string(path).unwrap_or_default()));
        }
        if Instant::now() >= deadline {
            let content = fs::read_to_string(path).unwrap_or_default();
            bail!("release enclave did not exit within {timeout:?}:\n{content}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn allocate_loopback_endpoint() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?.to_string();
    drop(listener);
    Ok(endpoint)
}

fn random_node_signer() -> Result<PrivateKeySigner> {
    for _ in 0..16 {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if let Ok(signer) = PrivateKeySigner::from_bytes(&B256::from(bytes)) {
            return Ok(signer);
        }
    }
    bail!("failed to generate a valid ephemeral NodeHost signer");
}

fn node_host_identity(
    network_binding: &NetworkBindingV1,
    signer: &PrivateKeySigner,
) -> Result<NodeHostIdentityV1> {
    let reth_p2p_public = signer
        .credential()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre!("NodeHost public key is not SEC1-33"))?;
    Ok(NodeHostIdentityV1 {
        network_binding: network_binding.clone(),
        reth_p2p_public,
    })
}

fn node_signature(signer: &PrivateKeySigner, hash: B256) -> Result<[u8; 65], String> {
    let signature = signer
        .sign_hash_sync(&hash)
        .map_err(|error| format!("sign NodeHost authorization: {error}"))?;
    let mut bytes = [0_u8; 65];
    bytes[..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
    bytes[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
    bytes[64] = u8::from(signature.v());
    Ok(bytes)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination).wrap_err("create copied bundle")?;
    for entry in WalkDir::new(source).min_depth(1).sort_by_file_name() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!("release bundle contains a symlink: {}", relative.display());
        }
        if metadata.is_dir() {
            fs::create_dir(&target)?;
            fs::set_permissions(&target, metadata.permissions())?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, metadata.permissions())?;
        } else {
            bail!(
                "release bundle contains unsupported entry: {}",
                relative.display()
            );
        }
    }
    Ok(())
}

fn command_ok(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .wrap_err_with(|| format!("start command: {description}"))?;
    if !status.success() {
        bail!("{description} failed with {status}");
    }
    Ok(())
}

fn container_user() -> Result<String> {
    let uid = command_output(Command::new("id").arg("-u"), "resolve uid")?;
    let gid = command_output(Command::new("id").arg("-g"), "resolve gid")?;
    Ok(format!("{}:{}", uid.trim(), gid.trim()))
}

fn command_output(command: &mut Command, description: &str) -> Result<String> {
    let output = command
        .output()
        .wrap_err_with(|| format!("start command: {description}"))?;
    successful_output(output, description)
}

fn command_combined_output(command: &mut Command, description: &str) -> Result<String> {
    let output = command
        .output()
        .wrap_err_with(|| format!("start command: {description}"))?;
    let mut combined = output.stdout;
    combined.extend_from_slice(&output.stderr);
    if !output.status.success() {
        bail!(
            "{description} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&combined)
        );
    }
    String::from_utf8(combined).wrap_err_with(|| format!("{description} emitted non-UTF-8"))
}

fn successful_output(output: Output, description: &str) -> Result<String> {
    if !output.status.success() {
        bail!(
            "{description} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).wrap_err_with(|| format!("{description} emitted non-UTF-8"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let mut file = File::open(path).wrap_err_with(|| format!("open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).wrap_err_with(|| format!("parse {}", path.display()))
}

fn canonical_json(value: Value) -> Vec<u8> {
    fn sort(value: Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.into_iter().map(sort).collect()),
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, sort(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            scalar => scalar,
        }
    }
    let mut bytes = serde_json::to_vec(&sort(value)).expect("serialize canonical evidence");
    bytes.push(b'\n');
    bytes
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn default_repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("e2e harness is two levels below repository root")
        .to_owned()
}

fn container_prefix() -> String {
    format!("outbe-release-sgx-e2e-{}", std::process::id())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use sha2::{Digest as _, Sha256};

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        release: ReleaseSgxCli,
    }

    #[test]
    fn hardware_sgx_cli_requires_an_explicit_production_network() {
        let parsed = TestCli::try_parse_from([
            "release-sgx",
            "--network",
            "mainnet",
            "--image",
            "ghcr.io/outbe/enclave@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--bundle",
            "/tmp/bundle",
            "--genesis",
            "/tmp/genesis.json",
            "--evidence",
            "/tmp/evidence.json",
        ])
        .expect("Mainnet hardware CLI");
        assert_eq!(
            parsed.release.network.artifact_set(),
            ReleaseDcapArtifactSetV1::Mainnet
        );
        assert!(TestCli::try_parse_from([
            "release-sgx",
            "--image",
            "ghcr.io/outbe/enclave@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--bundle",
            "/tmp/bundle",
            "--genesis",
            "/tmp/genesis.json",
            "--evidence",
            "/tmp/evidence.json",
        ])
        .is_err());
        assert!(TestCli::try_parse_from([
            "release-sgx",
            "--network",
            "devnet",
            "--image",
            "ghcr.io/outbe/enclave@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--bundle",
            "/tmp/bundle",
            "--genesis",
            "/tmp/genesis.json",
            "--evidence",
            "/tmp/evidence.json",
        ])
        .is_err());
    }

    #[test]
    fn exact_image_reference_requires_lowercase_sha256() {
        let digest = "a".repeat(64);
        let image = format!("ghcr.io/outbe/enclave@sha256:{digest}");
        assert_eq!(exact_image_digest(&image).expect("exact digest"), digest);
        assert!(exact_image_digest("ghcr.io/outbe/enclave:latest").is_err());
        assert!(
            exact_image_digest(&format!("ghcr.io/outbe/enclave@sha256:{}", "A".repeat(64)))
                .is_err()
        );
    }

    #[test]
    fn parses_local_report_measurements() {
        let output = format!(
            "local_report: mrenclave={} mrsigner={} isv_prod_id=1 isv_svn=2",
            "a".repeat(64),
            "b".repeat(64)
        );
        let parsed = parse_local_report(&output).expect("measurements");
        assert_eq!(parsed.isv_prod_id, 1);
        assert_eq!(parsed.isv_svn, 2);
        assert_eq!(parsed.mrenclave, "a".repeat(64));
        assert_eq!(parsed.mrsigner, "b".repeat(64));
    }

    #[test]
    fn canonical_hardware_evidence_has_sorted_keys_and_trailing_newline() {
        let bytes = canonical_json(serde_json::json!({"z": 1, "a": {"y": 2, "b": 3}}));
        assert_eq!(bytes, b"{\"a\":{\"b\":3,\"y\":2},\"z\":1}\n");
        assert_eq!(hex::encode(Sha256::digest(&bytes)).len(), 64);
    }
}
