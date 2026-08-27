//! Fresh exact-release Intel DCAP evidence runner.
//!
//! This host-side runner owns acquisition only. Quote generation and the public
//! Begin/Chunk/Finish verifier both execute in the exact published Gramine SGX
//! enclave; QPL/PCCS never enters consensus or supplies a verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read as _;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::B256;
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use clap::{Parser, ValueEnum};
use eyre::{bail, ensure, eyre, Result, WrapErr as _};
use outbe_evm::tee_attestation_activation::DcapChainSpecBindingV1;
use outbe_primitives::chain::OutbeNetwork;
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapCollateralComponentV1,
    DcapCollateralKind, DcapEvidenceV1, NodeIdV1, RegistrationIntentV1,
};
use outbe_tee::dcap_protocol::{DcapPckCaV1, DcapPlatformTcbStatusV1, DcapVerificationOutcomeV1};
use outbe_tee::node_host::{
    connect_or_initialize_node_host_enclave, load_committed_enclave_manifest_v1, NodeHostIdentityV1,
};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const COMPONENT_FILES: [(DcapCollateralKind, &str); 8] = [
    (
        DcapCollateralKind::PckCertificateChain,
        "pck-certificate-chain.pem0",
    ),
    (DcapCollateralKind::PckCrl, "pck.crl.der"),
    (
        DcapCollateralKind::PckCrlIssuerChain,
        "pck-crl-issuer-chain.pem",
    ),
    (DcapCollateralKind::RootCaCrl, "root-ca.crl.der"),
    (DcapCollateralKind::TcbInfo, "tcb-info.json"),
    (
        DcapCollateralKind::TcbInfoIssuerChain,
        "tcb-info-issuer-chain.pem",
    ),
    (DcapCollateralKind::QeIdentity, "qe-identity.json"),
    (
        DcapCollateralKind::QeIdentityIssuerChain,
        "qe-identity-issuer-chain.pem",
    ),
];

const COMMON_RELEASE_DCAP_ARTIFACT_PATHS: [&str; 17] = [
    "collateral/capture-provenance.json",
    "collateral/pck-certificate-chain.pem0",
    "collateral/pck-crl-issuer-chain.pem",
    "collateral/pck.crl.der",
    "collateral/qe-identity-issuer-chain.pem",
    "collateral/qe-identity.json",
    "collateral/root-ca.crl.der",
    "collateral/tcb-info-issuer-chain.pem",
    "collateral/tcb-info.json",
    "enclave-signature.bin",
    "evidence-v1.bin",
    "intent-v1.bin",
    "node-signature.bin",
    "policy-schedule-v1.bin",
    "policy-v1.bin",
    "quote-v3.bin",
    "verifier-outcome-v1.bin",
];

/// Exact non-secret archive contract for one production release network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseDcapArtifactSetV1 {
    Testnet,
    Mainnet,
}

impl ReleaseDcapArtifactSetV1 {
    #[must_use]
    pub const fn for_network(network: OutbeNetwork) -> Option<Self> {
        match network {
            OutbeNetwork::Testnet => Some(Self::Testnet),
            OutbeNetwork::Mainnet => Some(Self::Mainnet),
            OutbeNetwork::Devnet => None,
        }
    }

    #[must_use]
    pub const fn genesis_artifact_path(self) -> &'static str {
        match self {
            Self::Testnet => "testnet-genesis.json",
            Self::Mainnet => "mainnet-genesis.json",
        }
    }

    #[must_use]
    pub fn paths(self) -> BTreeSet<&'static str> {
        COMMON_RELEASE_DCAP_ARTIFACT_PATHS
            .into_iter()
            .chain([self.genesis_artifact_path()])
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ExpectedPckCa {
    Processor,
    Platform,
}

impl ExpectedPckCa {
    const fn label(self) -> &'static str {
        match self {
            Self::Processor => "processor",
            Self::Platform => "platform",
        }
    }

    const fn verdict(self) -> DcapPckCaV1 {
        match self {
            Self::Processor => DcapPckCaV1::Processor,
            Self::Platform => DcapPckCaV1::Platform,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "outbe-release-dcap-evidence")]
struct Cli {
    /// Exact Cosign-verified OCI reference, including @sha256:<digest>.
    #[arg(long)]
    image: String,
    /// Extracted exact signed SGX bundle.
    #[arg(long)]
    bundle: PathBuf,
    /// Final testnet genesis whose block-1 DCAP policy authorizes this release.
    #[arg(long)]
    testnet_genesis: PathBuf,
    /// Intel PCK CA required from the enclave-verified signed PCK chain.
    #[arg(long, value_enum)]
    expected_pck_ca: ExpectedPckCa,
    /// New output directory retained as a protected release artifact.
    #[arg(long)]
    output_dir: PathBuf,
    /// Repository checkout matching the signed bundle source commit.
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Retain the private temporary runtime directory after success.
    #[arg(long)]
    keep_work_dir: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct Measurements {
    debug: bool,
    isv_prod_id: u16,
    isv_svn: u16,
    mrenclave: String,
    mrsigner: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SourceIdentity {
    commit: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BundleManifest {
    measurements: Measurements,
    source: SourceIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct EpcRegion {
    node: String,
    total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct HostPreflight {
    architecture: String,
    cpu_model: String,
    epc: Vec<EpcRegion>,
    physical_package_count: u64,
    sgx_enclave_device: String,
    sgx_provision_device: String,
}

struct RunningEnclave {
    child: Child,
    name: String,
}

impl Drop for RunningEnclave {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.wait();
    }
}

pub fn main() -> Result<()> {
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<()> {
    let repo = fs::canonicalize(cli.repo.unwrap_or_else(default_repo))
        .wrap_err("resolve repository checkout")?;
    let bundle = fs::canonicalize(&cli.bundle).wrap_err("resolve signed SGX bundle")?;
    let output_dir = absolute_path(&cli.output_dir)?;
    ensure!(!output_dir.exists(), "output directory already exists");
    let image_digest = exact_image_digest(&cli.image)?.to_owned();
    let bundle_manifest: BundleManifest =
        read_json(&bundle.join("metadata/testnet-sgx-bundle.json"))?;
    let testnet_genesis = fs::canonicalize(&cli.testnet_genesis)
        .wrap_err("resolve final testnet genesis ChainSpec")?;
    let testnet_genesis_bytes =
        fs::read(&testnet_genesis).wrap_err("read final testnet genesis ChainSpec")?;

    command_ok(
        Command::new("cargo")
            .args(["xtask", "release", "sgx", "verify", "--bundle"])
            .arg(&bundle)
            .current_dir(&repo),
        "verify exact signed SGX bundle",
    )?;
    let binding =
        DcapChainSpecBindingV1::from_genesis_path(&testnet_genesis).map_err(eyre::Report::msg)?;
    ensure!(
        !bundle_manifest.measurements.debug,
        "debug enclave cannot satisfy H1"
    );
    binding
        .ensure_exact_release_measurements(
            parse_b256(&bundle_manifest.measurements.mrenclave)?,
            parse_b256(&bundle_manifest.measurements.mrsigner)?,
            bundle_manifest.measurements.isv_prod_id,
            bundle_manifest.measurements.isv_svn,
        )
        .map_err(eyre::Report::msg)?;
    command_ok(
        Command::new("docker")
            .args(["image", "inspect", &cli.image])
            .current_dir(&repo),
        "inspect exact pulled image",
    )?;

    // Host topology is untrusted provenance only. The enclave-resident verifier
    // authenticates the PCK CA from the signed type-5 certificate chain below.
    let preflight = host_preflight()?;
    let run_started_at = unix_seconds()?;
    let work = tempfile::Builder::new()
        .prefix("outbe-release-dcap-")
        .tempdir()
        .wrap_err("create DCAP evidence work directory")?;
    let seal_dir = work.path().join("sealed-state");
    let node_data_dir = work.path().join("node-data");
    fs::create_dir(&seal_dir)?;
    fs::create_dir(&node_data_dir)?;
    fs::set_permissions(&seal_dir, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&node_data_dir, fs::Permissions::from_mode(0o700))?;

    let endpoint = allocate_loopback_endpoint()?;
    let mut enclave = start_enclave(
        &cli.image,
        &endpoint,
        &seal_dir,
        &work.path().join("enclave.log"),
        binding.chain_id,
    )?;
    wait_for_log(
        &mut enclave.child,
        &work.path().join("enclave.log"),
        "listening on tcp://",
        Duration::from_secs(180),
    )?;

    let node_signer = random_node_signer()?;
    let reth_p2p_public = node_signer
        .credential()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre!("NodeHost public key is not SEC1-33"))?;
    let identity = NodeHostIdentityV1 {
        chain_id: binding.chain_id,
        genesis_hash: binding.genesis_hash,
        reth_p2p_public,
    };
    let mut client =
        connect_or_initialize_node_host_enclave(&endpoint, &node_data_dir, identity, |hash| {
            node_signature(&node_signer, hash)
        })?;
    let manifest = load_committed_enclave_manifest_v1(&node_data_dir)?;
    let policy = binding.policy.clone();
    let policy_hash = binding.policy_hash;

    let binding_id = random_nonzero_b256()?;
    let requested_valid_until = run_started_at
        .checked_add(86_400)
        .ok_or_else(|| eyre!("requested validity overflow"))?;
    let intent = RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash,
        node_id: manifest.node_id.clone(),
        enclave_id: manifest
            .enclave_id()
            .map_err(|error| eyre!("derive enclave ID: {error}"))?,
        binding_id,
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest
            .node_host_authorization_hash()
            .map_err(|error| eyre!("derive NodeHost authorization hash: {error}"))?,
    };
    ensure!(
        intent.node_id == NodeIdV1 { reth_p2p_public },
        "initialized manifest changed the NodeHost identity"
    );
    let intent_bytes = intent
        .encode_canonical()
        .map_err(|error| eyre!("encode registration intent: {error}"))?;
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre!("hash registration intent: {error}"))?;
    let registration_node_signature =
        node_signature(&node_signer, intent_hash).map_err(|error| eyre!(error))?;
    let generated = client.generate_dcap_quote(&intent)?;
    let quote_generated_at = unix_seconds()?;
    let quote_path = work.path().join("quote-v3.bin");
    fs::write(&quote_path, &generated.quote_body)?;

    let collateral_started_at = unix_seconds()?;
    let collateral_dir = work.path().join("collateral");
    command_ok(
        Command::new("python3")
            .arg(repo.join("scripts/release/capture_dcap_collateral.py"))
            .args(["--quote"])
            .arg(&quote_path)
            .args(["--output-dir"])
            .arg(&collateral_dir)
            .args(["--expected-pck-ca", cli.expected_pck_ca.label()]),
        "acquire exact host-only Intel collateral",
    )?;
    let collateral_completed_at = unix_seconds()?;

    let components = COMPONENT_FILES
        .iter()
        .map(|(kind, name)| {
            Ok(DcapCollateralComponentV1 {
                kind: *kind,
                bytes: fs::read(collateral_dir.join(name))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body.clone(),
        components,
        transition_key_ready_proof: generated.transition_key_ready_proof,
    });
    let evidence_bytes = evidence
        .encode_canonical()
        .map_err(|error| eyre!("encode self-contained DCAP evidence: {error}"))?;
    let policy_bytes = policy
        .encode_canonical()
        .map_err(|error| eyre!("encode release policy: {error}"))?;
    let consensus_timestamp = unix_seconds()?;
    let outcome =
        client.verify_dcap_evidence_v1(&evidence_bytes, &policy_bytes, consensus_timestamp)?;
    let verified_at = unix_seconds()?;
    let verdict = match &outcome {
        DcapVerificationOutcomeV1::Accepted(verdict) => verdict,
        DcapVerificationOutcomeV1::Rejected(code) => {
            bail!(
                "public enclave verifier rejected fresh evidence with {:#06x}",
                code.code()
            );
        }
    };
    ensure!(
        verdict.pck_ca == cli.expected_pck_ca.verdict(),
        "hardware returned {:?}, expected {:?}",
        verdict.pck_ca,
        cli.expected_pck_ca.verdict()
    );
    ensure!(
        verdict.mrenclave == parse_b256(&bundle_manifest.measurements.mrenclave)?
            && verdict.mrsigner == parse_b256(&bundle_manifest.measurements.mrsigner)?
            && verdict.isv_prod_id == bundle_manifest.measurements.isv_prod_id
            && verdict.isv_svn == bundle_manifest.measurements.isv_svn,
        "accepted verdict does not bind the signed bundle measurements"
    );
    let outcome_bytes = outcome
        .encode_canonical()
        .map_err(|code| eyre!("encode accepted verifier outcome: {:#06x}", code.code()))?;

    fs::create_dir(&output_dir)?;
    let retained_collateral = output_dir.join("collateral");
    fs::create_dir(&retained_collateral)?;
    let mut artifacts = BTreeMap::new();
    write_artifact(
        &output_dir,
        "testnet-genesis.json",
        &testnet_genesis_bytes,
        &mut artifacts,
    )?;
    write_artifact(&output_dir, "policy-v1.bin", &policy_bytes, &mut artifacts)?;
    write_artifact(
        &output_dir,
        "policy-schedule-v1.bin",
        &binding.policy_schedule_bytes,
        &mut artifacts,
    )?;
    write_artifact(&output_dir, "intent-v1.bin", &intent_bytes, &mut artifacts)?;
    write_artifact(
        &output_dir,
        "quote-v3.bin",
        &generated.quote_body,
        &mut artifacts,
    )?;
    write_artifact(
        &output_dir,
        "evidence-v1.bin",
        &evidence_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &output_dir,
        "verifier-outcome-v1.bin",
        &outcome_bytes,
        &mut artifacts,
    )?;
    write_artifact(
        &output_dir,
        "node-signature.bin",
        &registration_node_signature,
        &mut artifacts,
    )?;
    write_artifact(
        &output_dir,
        "enclave-signature.bin",
        &generated.enclave_signature,
        &mut artifacts,
    )?;
    for (_, name) in COMPONENT_FILES {
        let bytes = fs::read(collateral_dir.join(name))?;
        write_artifact(&retained_collateral, name, &bytes, &mut artifacts)?;
    }
    let capture_provenance = fs::read(collateral_dir.join("capture-provenance.json"))?;
    let capture: Value = serde_json::from_slice(&capture_provenance)?;
    write_artifact(
        &retained_collateral,
        "capture-provenance.json",
        &capture_provenance,
        &mut artifacts,
    )?;
    let declared_artifacts = artifacts
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    ensure!(
        declared_artifacts == ReleaseDcapArtifactSetV1::Testnet.paths(),
        "release DCAP runner did not retain the exact canonical artifact set"
    );
    ensure_no_secret_markers(&output_dir)?;

    let pck_crl = file_record(&retained_collateral.join("pck.crl.der"))?;
    let root_crl = file_record(&retained_collateral.join("root-ca.crl.der"))?;
    let pck_crl_evidence = release_crl_evidence(
        &capture,
        "/crls/pck",
        cli.expected_pck_ca.label(),
        "collateral/pck.crl.der",
        &pck_crl,
    )?;
    let root_crl_evidence = release_crl_evidence(
        &capture,
        "/crls/root",
        "root",
        "collateral/root-ca.crl.der",
        &root_crl,
    )?;
    let evidence_document = serde_json::json!({
        "artifacts": artifacts,
        "attestation": {
            "advisory_ids": verdict.advisory_ids,
            "collateral_valid_until": verdict.collateral_valid_until,
            "fmspc": hex::encode(verdict.fmspc),
            "pce_id": verdict.pce_id,
            "pck_ca": cli.expected_pck_ca.label(),
            "platform_tcb_status": platform_status(verdict.platform_tcb_status),
            "public_verifier": "enclave-resident-begin-chunk-finish-v1",
            "qe_tcb_evaluation_data_number": verdict.qe_tcb_evaluation_data_number,
            "tcb_evaluation_data_number": verdict.tcb_evaluation_data_number
        },
        "collateral": {
            "capture_provenance": capture,
            "component_count": 8,
            "pck_crl": pck_crl_evidence,
            "root_crl": root_crl_evidence
        },
        "environment": {
            "architecture": preflight.architecture,
            "backend": "gramine-sgx",
            "cpu_model": preflight.cpu_model,
            "dcap": true,
            "epc": preflight.epc,
            "hardware_sgx": true,
            "physical_package_count": preflight.physical_package_count,
            "sgx_enclave_device": preflight.sgx_enclave_device,
            "sgx_provision_device": preflight.sgx_provision_device
        },
        "freshness": {
            "binding_id_nonzero": !binding_id.is_zero(),
            "binding_id_sha256": hex::encode(Sha256::digest(binding_id.as_slice())),
            "collateral_completed_at": collateral_completed_at,
            "collateral_started_at": collateral_started_at,
            "consensus_timestamp": consensus_timestamp,
            "quote_generated_at": quote_generated_at,
            "run_started_at": run_started_at,
            "verified_at": verified_at
        },
        "image": {"digest": {"algorithm": "sha256", "value": image_digest}, "reference": cli.image},
        "intent": {"profile": "validator", "sha256": hex::encode(Sha256::digest(&intent_bytes))},
        "policy": {
            "activation_height": binding.activation_height,
            "chain_id": binding.chain_id,
            "genesis_hash": hex::encode(binding.genesis_hash),
            "policy_hash": hex::encode(binding.policy_hash),
            "policy_schedule_hash": hex::encode(binding.policy_schedule_hash),
            "policy_schedule_sha256": hex::encode(Sha256::digest(&binding.policy_schedule_bytes)),
            "policy_version": binding.policy_version,
            "profile_selection": "validator exercises the consensus-participating testnet registration path; the canonical block-1 policy binds the same release measurement for validator and full-node",
            "sha256": hex::encode(Sha256::digest(&policy_bytes))
        },
        "measurements": bundle_manifest.measurements,
        "result": "passed",
        "schema_version": "1.0.0",
        "secret_scan": "passed",
        "source_commit": bundle_manifest.source.commit
    });
    fs::write(
        output_dir.join("hardware-dcap-evidence.json"),
        canonical_json(evidence_document)?,
    )?;

    drop(enclave);
    if cli.keep_work_dir {
        eprintln!("retained private work directory: {}", work.keep().display());
    }
    println!(
        "fresh accepted {} DCAP evidence: {}",
        cli.expected_pck_ca.label(),
        output_dir.display()
    );
    Ok(())
}

fn host_preflight() -> Result<HostPreflight> {
    ensure!(std::env::consts::ARCH == "x86_64", "H1 requires x86_64");
    let (sgx_enclave_device, sgx_provision_device) = sgx_devices()?;
    let lscpu: Value = serde_json::from_slice(
        &Command::new("lscpu")
            .arg("--json")
            .output()
            .wrap_err("run lscpu preflight")?
            .stdout,
    )
    .wrap_err("parse lscpu JSON")?;
    let fields = lscpu_fields(&lscpu)?;
    let packages = parse_lscpu_u64(&fields, &["Socket(s):", "Sockets:"])?;
    let cpu_model = field_value(&fields, &["Model name:"])?;
    let cpuinfo = fs::read_to_string("/proc/cpuinfo")?;
    ensure!(
        cpuinfo.lines().any(|line| {
            line.starts_with("flags")
                && line
                    .split_whitespace()
                    .any(|flag| flag == "sgx" || flag == "sgx_lc")
        }),
        "CPU flags do not expose SGX"
    );
    let epc = epc_regions()?;
    ensure!(
        epc.iter().any(|region| region.total_bytes > 0),
        "no non-zero SGX EPC region is exposed"
    );
    Ok(HostPreflight {
        architecture: "x86_64".to_owned(),
        cpu_model,
        epc,
        physical_package_count: packages,
        sgx_enclave_device,
        sgx_provision_device,
    })
}

fn lscpu_fields(value: &Value) -> Result<BTreeMap<String, String>> {
    let rows = value
        .get("lscpu")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("lscpu JSON has no lscpu array"))?;
    let mut fields = BTreeMap::new();
    for row in rows {
        let field = row.get("field").and_then(Value::as_str);
        let data = row.get("data").and_then(Value::as_str);
        if let (Some(field), Some(data)) = (field, data) {
            fields.insert(field.to_owned(), data.to_owned());
        }
    }
    Ok(fields)
}

fn parse_lscpu_u64(fields: &BTreeMap<String, String>, names: &[&str]) -> Result<u64> {
    field_value(fields, names)?
        .parse()
        .wrap_err_with(|| format!("lscpu {} is not u64", names.join("/")))
}

fn field_value(fields: &BTreeMap<String, String>, names: &[&str]) -> Result<String> {
    names
        .iter()
        .find_map(|name| fields.get(*name))
        .cloned()
        .ok_or_else(|| eyre!("lscpu lacks {}", names.join("/")))
}

fn epc_regions() -> Result<Vec<EpcRegion>> {
    let mut regions = Vec::new();
    let nodes = Path::new("/sys/devices/system/node");
    if nodes.is_dir() {
        for entry in fs::read_dir(nodes)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("node") || !name[4..].bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let path = entry.path().join("x86/sgx_total_bytes");
            if path.is_file() {
                let total_bytes = fs::read_to_string(&path)?.trim().parse::<u64>()?;
                regions.push(EpcRegion {
                    node: name,
                    total_bytes,
                });
            }
        }
    }
    regions.sort_by(|a, b| a.node.cmp(&b.node));
    Ok(regions)
}

fn sgx_devices() -> Result<(String, String)> {
    for (enclave, provision) in [
        ("/dev/sgx_enclave", "/dev/sgx_provision"),
        ("/dev/sgx/enclave", "/dev/sgx/provision"),
    ] {
        if Path::new(enclave).exists() && Path::new(provision).exists() {
            return Ok((enclave.to_owned(), provision.to_owned()));
        }
    }
    bail!("required SGX enclave and provision devices are absent");
}

fn start_enclave(
    image: &str,
    endpoint: &str,
    seal_dir: &Path,
    log_path: &Path,
    chain_id: u64,
) -> Result<RunningEnclave> {
    let name = format!("outbe-release-dcap-{}", std::process::id());
    let log = File::create(log_path)?;
    let (enclave, provision) = sgx_devices()?;
    let mut command = Command::new("docker");
    command
        .args(["run", "--rm", "--name", &name, "--network", "host"])
        .args(["--device", &format!("{enclave}:/dev/sgx_enclave")])
        .args(["--device", &format!("{provision}:/dev/sgx_provision")])
        .args(["-v", &format!("{}:/var/lib/outbe/tee", seal_dir.display())])
        .arg(image)
        .args([
            "--socket",
            endpoint,
            "--tee-dir",
            "/var/lib/outbe/tee",
            "--chain-id",
            &format!("0x{chain_id:064x}"),
        ])
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let child = command.spawn().wrap_err("start exact release enclave")?;
    Ok(RunningEnclave { child, name })
}

fn wait_for_log(child: &mut Child, path: &Path, marker: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let content = fs::read_to_string(path).unwrap_or_default();
        if content.contains(marker) {
            return Ok(());
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
    bail!("failed to generate a valid ephemeral node signer");
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

fn random_nonzero_b256() -> Result<B256> {
    for _ in 0..16 {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if bytes != [0; 32] {
            return Ok(B256::from(bytes));
        }
    }
    bail!("OS RNG repeatedly returned a zero binding ID");
}

#[derive(Serialize)]
struct FileRecord {
    size: u64,
    sha256: String,
}

fn release_crl_evidence(
    capture: &Value,
    pointer: &str,
    expected_kind: &str,
    path: &str,
    file: &FileRecord,
) -> Result<Value> {
    let source = capture
        .pointer(pointer)
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("capture provenance lacks {pointer}"))?;
    let captured_kind = source.get("type").and_then(Value::as_str);
    let captured_size = source.get("size").and_then(Value::as_u64);
    let captured_sha256 = source.get("sha256").and_then(Value::as_str);
    ensure!(
        captured_kind == Some(expected_kind)
            && captured_size == Some(file.size)
            && captured_sha256 == Some(file.sha256.as_str()),
        "capture CRL provenance does not bind the retained {expected_kind} CRL"
    );
    let mut record = source.clone();
    record.remove("type");
    record.insert("kind".to_owned(), Value::String(expected_kind.to_owned()));
    record.insert("path".to_owned(), Value::String(path.to_owned()));
    Ok(Value::Object(record))
}

fn file_record(path: &Path) -> Result<FileRecord> {
    Ok(FileRecord {
        size: fs::metadata(path)?.len(),
        sha256: sha256_file(path)?,
    })
}

fn write_artifact(
    root: &Path,
    name: &str,
    bytes: &[u8],
    artifacts: &mut BTreeMap<String, Value>,
) -> Result<()> {
    let path = root.join(name);
    fs::write(&path, bytes)?;
    let relative = if root.file_name().and_then(|name| name.to_str()) == Some("collateral") {
        format!("collateral/{name}")
    } else {
        name.to_owned()
    };
    artifacts.insert(
        relative,
        serde_json::json!({"size": bytes.len(), "sha256": hex::encode(Sha256::digest(bytes))}),
    );
    Ok(())
}

fn ensure_no_secret_markers(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            ensure_no_secret_markers(&path)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let lower = fs::read(&path)?
            .into_iter()
            .map(|byte| byte.to_ascii_lowercase())
            .collect::<Vec<_>>();
        for forbidden in [
            b"pcs_api_key".as_slice(),
            b"subscription-key",
            b"user_token",
        ] {
            ensure!(
                !lower
                    .windows(forbidden.len())
                    .any(|window| window == forbidden),
                "retained metadata contains a credential marker: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn platform_status(status: DcapPlatformTcbStatusV1) -> &'static str {
    match status {
        DcapPlatformTcbStatusV1::UpToDate => "up-to-date",
        DcapPlatformTcbStatusV1::SWHardeningNeeded => "sw-hardening-needed",
        DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded => {
            "configuration-and-sw-hardening-needed"
        }
    }
}

fn exact_image_digest(image: &str) -> Result<&str> {
    let (_, digest) = image
        .rsplit_once("@sha256:")
        .ok_or_else(|| eyre!("release image must be addressed by @sha256 digest"))?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "release image digest must be 64 lowercase hexadecimal characters"
    );
    Ok(digest)
}

fn parse_b256(value: &str) -> Result<B256> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "expected 32 hexadecimal bytes"
    );
    Ok(B256::from_slice(&hex::decode(value)?))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let mut file = File::open(path).wrap_err_with(|| format!("open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).wrap_err_with(|| format!("parse {}", path.display()))
}

fn canonical_json(value: Value) -> Result<Vec<u8>> {
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
    let mut bytes = serde_json::to_vec(&sort(value))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn command_ok(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .wrap_err_with(|| format!("start command: {description}"))?;
    ensure!(status.success(), "{description} failed with {status}");
    Ok(())
}

fn unix_seconds() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
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

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{
        chain::TESTNET_CHAIN_ID,
        tee_attestation_v1::{PlatformTcbStatusSetV1, QvlTcbStatusV1, TeePolicyV1},
        tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1, ProductionSgxMeasurementV1},
    };

    #[test]
    fn release_artifact_sets_are_closed_and_differ_only_by_genesis() {
        let testnet = ReleaseDcapArtifactSetV1::for_network(OutbeNetwork::Testnet).unwrap();
        let mainnet = ReleaseDcapArtifactSetV1::for_network(OutbeNetwork::Mainnet).unwrap();
        assert!(ReleaseDcapArtifactSetV1::for_network(OutbeNetwork::Devnet).is_none());
        assert_eq!(testnet.paths().len(), 18);
        assert_eq!(mainnet.paths().len(), 18);
        assert!(testnet.paths().contains("testnet-genesis.json"));
        assert!(!testnet.paths().contains("mainnet-genesis.json"));
        assert!(mainnet.paths().contains("mainnet-genesis.json"));
        assert!(!mainnet.paths().contains("testnet-genesis.json"));

        let common = testnet
            .paths()
            .intersection(&mainnet.paths())
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(common.len(), 17);
    }

    fn testnet_policy(measurements: &Measurements) -> TeePolicyV1 {
        initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
                mrenclave: parse_b256(&measurements.mrenclave).unwrap(),
                mrsigner: parse_b256(&measurements.mrsigner).unwrap(),
                isv_prod_id: measurements.isv_prod_id,
                minimum_isv_svn: measurements.isv_svn,
                minimum_tcb_evaluation_data_number: 1,
            }),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(3),
        )
        .unwrap()
    }

    #[test]
    fn exact_image_reference_requires_lowercase_sha256() {
        let digest = "a".repeat(64);
        assert_eq!(
            exact_image_digest(&format!("ghcr.io/outbe/enclave@sha256:{digest}"))
                .expect("exact digest"),
            digest
        );
        assert!(exact_image_digest("ghcr.io/outbe/enclave:latest").is_err());
        assert!(
            exact_image_digest(&format!("ghcr.io/outbe/enclave@sha256:{}", "A".repeat(64)))
                .is_err()
        );
    }

    #[test]
    fn records_lscpu_package_topology_as_untrusted_provenance() {
        let value = serde_json::json!({"lscpu": [
            {"field": "Model name:", "data": "Intel(R) Xeon(R)"},
            {"field": "Socket(s):", "data": "2"}
        ]});
        let fields = lscpu_fields(&value).expect("lscpu fields");
        assert_eq!(parse_lscpu_u64(&fields, &["Socket(s):"]).unwrap(), 2);
        assert_eq!(
            field_value(&fields, &["Model name:"]).unwrap(),
            "Intel(R) Xeon(R)"
        );
    }

    #[test]
    fn testnet_policy_fixture_binds_signed_measurements_and_strict_statuses() {
        let measurements = Measurements {
            debug: false,
            isv_prod_id: 1,
            isv_svn: 2,
            mrenclave: "11".repeat(32),
            mrsigner: "22".repeat(32),
        };
        let policy = testnet_policy(&measurements);
        assert_eq!(
            policy.accepted_platform_tcb_statuses,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded
        );
        assert_eq!(policy.accepted_qe_tcb_status, QvlTcbStatusV1::UpToDate);
        assert_eq!(policy.measurement_rules.len(), 1);
        assert_eq!(
            policy.measurement_rules[0].mrenclave,
            B256::repeat_byte(0x11)
        );
        assert_eq!(
            policy.measurement_rules[0].mrsigner,
            B256::repeat_byte(0x22)
        );
    }
}
