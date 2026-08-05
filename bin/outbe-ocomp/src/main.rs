use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy_primitives::B256;
use clap::{Args, Parser, Subcommand};
use outbe_compressed_entities::{
    CeTopologyV1, EnvironmentIdentity, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_consensus::{config::init_consensus_chain_id, proof::constants::consensus_chain_id};
use outbe_ocomp::bundle::PinnedProtocolBundle;
use outbe_ocomp::cas::CasLimits;
use outbe_ocomp::control::{
    effective_uid, poc_schema_limits, require_effective_uid, uid_for_user, EndpointIdentity,
};
use outbe_ocomp::inbox::WorkerInboxLimits;
use outbe_ocomp::snapshot_client::SnapshotExporterNodeConfig;
use outbe_ocomp::snapshot_exporter::{SnapshotExporter, SnapshotExporterConfig};
use outbe_ocomp::supervisor::{SupervisorDiscovery, SupervisorDiscoveryConfig};
use outbe_ocomp::supervisor_export::{
    SupervisorExportAdoption, SupervisorExportAdoptionConfig, SupervisorExportAdoptionOutcome,
};
use outbe_ocomp::supervisor_job::{
    CompletedSupervisorJobV1, DevelopmentWorkerLaunchV1, SupervisorJobRunnerConfigV1,
    SupervisorJobRunnerV1,
};
use outbe_ocomp::vote_submitter::{
    LocalVoteTransactionPreparerV1, PublicVoteRpcClientV1, SupervisorVoteSubmitterV1,
    VoteSubmissionConfigV1, VoteSubmissionOutcomeV1,
};
use outbe_ocomp::worker::{run_one_from_inherited_fd, WorkerConfig};
use outbe_ocomp_protocol::capacity::OCOMP_POC_CAS_QUOTA_BYTES;
use outbe_offchain_storage::MongoStorageConfig;
use outbe_primitives::signer::OutbeEvmSigner;

#[derive(Debug, Parser)]
#[command(name = "outbe-ocomp")]
#[command(about = "Fixed Off-chain Computation PoC process roles")]
struct Cli {
    #[command(subcommand)]
    role: Role,
}

#[derive(Debug, Subcommand)]
enum Role {
    Supervisor(RuntimeArgs),
    /// Keyless FullNode role: computes and durably commits Lysis, never votes.
    Follower(RuntimeArgs),
    SnapshotExporter(RuntimeArgs),
    Worker(WorkerArgs),
    /// Print the address of the role-delegated OCOMP transaction signer.
    SignerAddress(RuntimeArgs),
}

#[derive(Clone, Debug, Args)]
struct WorkerArgs {
    #[command(flatten)]
    runtime: RuntimeArgs,
    #[arg(long)]
    chain_id: u64,
    #[arg(long)]
    genesis_hash: B256,
    #[arg(long)]
    boot_nonce: B256,
    #[arg(long)]
    protocol_bundle_hash: B256,
    #[arg(long)]
    session_generation: u64,
}

#[derive(Clone, Debug, Default, Args)]
struct RuntimeArgs {
    /// Unprivileged OCM measurement namespace; accepted only by debug builds.
    #[arg(long, hide = true, value_name = "PATH")]
    development_root: Option<PathBuf>,

    /// Exact validator CE data directory for the development SnapshotExporter.
    #[arg(long, hide = true, value_name = "PATH", requires = "development_root")]
    development_ce_datadir: Option<PathBuf>,

    /// Exact development node Supervisor socket; artifacts remain under root.
    #[arg(long, hide = true, value_name = "PATH", requires = "development_root")]
    development_node_supervisor_socket: Option<PathBuf>,

    /// Exact development node SnapshotExporter socket; artifacts remain under root.
    #[arg(long, hide = true, value_name = "PATH", requires = "development_root")]
    development_node_snapshot_exporter_socket: Option<PathBuf>,
}

const SUPERVISOR_USER: &str = "outbe-ocomp-supervisor";
const SNAPSHOT_EXPORTER_USER: &str = "outbe-ocomp-export";
const WORKER_USER: &str = "outbe-ocomp-worker";
const BASE_PATH: &str = "/opt/outbe-chain";
const BASE_PATH_ENV: &str = "OUTBE_OCOMP_BASE_PATH";
const NODE_USER_ENV: &str = "OUTBE_OCOMP_NODE_USER";
const CAS_MAX_OBJECT_BYTES: u64 = 1_048_576;
const CAS_MAX_TOTAL_BYTES: u64 = OCOMP_POC_CAS_QUOTA_BYTES;
const WORKER_INBOX_MAX_ARTIFACT_BYTES: u64 = 1_048_576;
const WORKER_INBOX_MAX_TOTAL_BYTES: u64 = 67_108_864;
const SOCKET_ACTIVATION_FD: i32 = 0;
const SUPERVISOR_VOTE_RPC_MAX_RESPONSE_BYTES: usize = 1_048_576;
const SUPERVISOR_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const SNAPSHOT_EXPORTER_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const SNAPSHOT_EXPORTER_TRIBUTE_PAGE_LIMIT: usize = 256;
const SNAPSHOT_EXPORTER_MAX_RECOVERABLE_PREPARED_JOBS: usize = 8;
const PROJECTION_START_BLOCK: u64 = 1;
const CE_TREE_FORMAT: &str = "ckb-smt-v0.6.1-poseidon-catalog-v3";
const CE_VENDOR_REVISION: &str = "ad555350c866b2265d87d2d7fbd146fbc918bfe5";

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductionConfig {
    base_path: PathBuf,
    node_user: String,
}

impl ProductionConfig {
    fn from_environment() -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_lookup(|name| env::var_os(name))
    }

    fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let base_path = lookup(BASE_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(BASE_PATH));
        let node_user = lookup(NODE_USER_ENV)
            .ok_or_else(|| format!("required environment variable {NODE_USER_ENV} is missing"))?
            .into_string()
            .map_err(|_| format!("{NODE_USER_ENV} must be valid UTF-8"))?;
        if node_user.is_empty() {
            return Err(format!("{NODE_USER_ENV} must not be empty").into());
        }
        Ok(Self {
            base_path,
            node_user,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductionLayout {
    node_supervisor_socket: PathBuf,
    node_snapshot_exporter_socket: PathBuf,
    worker_socket: PathBuf,
    cas_root: PathBuf,
    worker_inbox_root: PathBuf,
    supervisor_journal_root: PathBuf,
    supervisor_export_binding_root: PathBuf,
    supervisor_job_root: PathBuf,
    supervisor_vote_submission_root: PathBuf,
    snapshot_exporter_ce_datadir: PathBuf,
    snapshot_exporter_input_ref_root: PathBuf,
    snapshot_exporter_receipt_root: PathBuf,
    ocomp_evm_key_path: PathBuf,
    protocol_bundle_path: PathBuf,
}

impl ProductionLayout {
    fn from_base(base_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !base_path.is_absolute() || base_path.parent().is_none() || base_path == Path::new("/") {
            return Err("OCOMP base path must be a non-root absolute path".into());
        }

        let ocomp_root = base_path.join("ocomp");
        let runtime_root = ocomp_root.join("run");
        let data_root = ocomp_root.join("data");
        let supervisor_root = data_root.join("supervisor-v1");
        let exporter_root = data_root.join("exporter-v1");
        Ok(Self {
            node_supervisor_socket: runtime_root.join("node-supervisor.sock"),
            node_snapshot_exporter_socket: runtime_root.join("node-snapshot-exporter.sock"),
            worker_socket: runtime_root.join("worker.sock"),
            cas_root: data_root.join("cas-v1"),
            worker_inbox_root: data_root.join("worker-inbox-v1"),
            supervisor_journal_root: supervisor_root.join("discovery"),
            supervisor_export_binding_root: supervisor_root.join("export-bindings"),
            supervisor_job_root: supervisor_root.join("jobs"),
            supervisor_vote_submission_root: supervisor_root.join("vote-submissions"),
            snapshot_exporter_ce_datadir: ocomp_root.join("ce"),
            snapshot_exporter_input_ref_root: exporter_root.join("input-refs"),
            snapshot_exporter_receipt_root: exporter_root.join("receipts"),
            ocomp_evm_key_path: data_root.join("keys").join("ocomp-evm-key.hex"),
            protocol_bundle_path: base_path.join("protocol-bundle-v1.ocb1"),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessRole {
    Supervisor,
    Follower,
    SnapshotExporter,
    Worker,
}

impl ProcessRole {
    const fn production_user(self) -> &'static str {
        match self {
            Self::Supervisor | Self::Follower => SUPERVISOR_USER,
            Self::SnapshotExporter => SNAPSHOT_EXPORTER_USER,
            Self::Worker => WORKER_USER,
        }
    }
}

#[derive(Clone, Debug)]
struct RuntimeProfile {
    effective_role_uid: u32,
    supervisor_uid: u32,
    worker_uid: u32,
    node_uid: u32,
    node_supervisor_socket: PathBuf,
    node_snapshot_exporter_socket: PathBuf,
    worker_socket: PathBuf,
    cas_root: PathBuf,
    worker_inbox_root: PathBuf,
    supervisor_journal_root: PathBuf,
    supervisor_export_binding_root: PathBuf,
    supervisor_job_root: PathBuf,
    supervisor_vote_submission_root: PathBuf,
    snapshot_exporter_ce_datadir: PathBuf,
    snapshot_exporter_input_ref_root: PathBuf,
    snapshot_exporter_receipt_root: PathBuf,
    ocomp_evm_key_path: PathBuf,
    protocol_bundle_path: PathBuf,
}

impl RuntimeProfile {
    fn resolve(args: &RuntimeArgs, role: ProcessRole) -> Result<Self, Box<dyn std::error::Error>> {
        let Some(root) = args.development_root.as_ref() else {
            if args.development_ce_datadir.is_some()
                || args.development_node_supervisor_socket.is_some()
                || args.development_node_snapshot_exporter_socket.is_some()
            {
                return Err("development path overrides require --development-root".into());
            }
            let production = ProductionConfig::from_environment()?;
            let layout = ProductionLayout::from_base(&production.base_path)?;
            return Ok(Self {
                effective_role_uid: uid_for_user(role.production_user())?,
                supervisor_uid: uid_for_user(SUPERVISOR_USER)?,
                worker_uid: uid_for_user(WORKER_USER)?,
                node_uid: uid_for_user(&production.node_user)?,
                node_supervisor_socket: layout.node_supervisor_socket,
                node_snapshot_exporter_socket: layout.node_snapshot_exporter_socket,
                worker_socket: layout.worker_socket,
                cas_root: layout.cas_root,
                worker_inbox_root: layout.worker_inbox_root,
                supervisor_journal_root: layout.supervisor_journal_root,
                supervisor_export_binding_root: layout.supervisor_export_binding_root,
                supervisor_job_root: layout.supervisor_job_root,
                supervisor_vote_submission_root: layout.supervisor_vote_submission_root,
                snapshot_exporter_ce_datadir: layout.snapshot_exporter_ce_datadir,
                snapshot_exporter_input_ref_root: layout.snapshot_exporter_input_ref_root,
                snapshot_exporter_receipt_root: layout.snapshot_exporter_receipt_root,
                ocomp_evm_key_path: layout.ocomp_evm_key_path,
                protocol_bundle_path: layout.protocol_bundle_path,
            });
        };

        if !cfg!(debug_assertions) {
            return Err("--development-root is unavailable in release outbe-ocomp builds".into());
        }
        if !root.is_absolute() || root.parent().is_none() || root == std::path::Path::new("/") {
            return Err("--development-root must be a non-root absolute path".into());
        }
        let metadata = std::fs::symlink_metadata(root)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("--development-root must name an existing real directory".into());
        }
        let snapshot_exporter_ce_datadir = match (role, args.development_ce_datadir.as_ref()) {
            (ProcessRole::SnapshotExporter, Some(path)) if path.is_absolute() => path.clone(),
            (ProcessRole::SnapshotExporter, _) => {
                return Err(
                    "development SnapshotExporter requires an absolute --development-ce-datadir"
                        .into(),
                );
            }
            (_, Some(_)) => {
                return Err(
                    "--development-ce-datadir is valid only for the SnapshotExporter role".into(),
                );
            }
            _ => root.join("unused-ce-datadir"),
        };
        let node_supervisor_socket = match (role, args.development_node_supervisor_socket.as_ref())
        {
            (ProcessRole::Supervisor | ProcessRole::Follower, Some(path)) if path.is_absolute() => {
                path.clone()
            }
            (ProcessRole::Supervisor | ProcessRole::Follower, None) => {
                root.join("node-supervisor.sock")
            }
            (_, Some(_)) => {
                return Err(
                    "--development-node-supervisor-socket is valid only for the Supervisor role"
                        .into(),
                );
            }
            _ => root.join("unused-node-supervisor.sock"),
        };
        let node_snapshot_exporter_socket = match (
            role,
            args.development_node_snapshot_exporter_socket.as_ref(),
        ) {
            (ProcessRole::SnapshotExporter, Some(path)) if path.is_absolute() => path.clone(),
            (ProcessRole::SnapshotExporter, None) => root.join("node-snapshot-exporter.sock"),
            (_, Some(_)) => {
                return Err(
                        "--development-node-snapshot-exporter-socket is valid only for the SnapshotExporter role"
                            .into(),
                    );
            }
            _ => root.join("unused-node-snapshot-exporter.sock"),
        };
        let uid = effective_uid()?;
        Ok(Self {
            effective_role_uid: uid,
            supervisor_uid: uid,
            worker_uid: uid,
            node_uid: uid,
            node_supervisor_socket,
            node_snapshot_exporter_socket,
            worker_socket: root.join("worker.sock"),
            cas_root: root.join("cas-v1"),
            worker_inbox_root: root.join("worker-inbox-v1"),
            supervisor_journal_root: root.join("supervisor-v1").join("discovery"),
            supervisor_export_binding_root: root.join("supervisor-v1").join("export-bindings"),
            supervisor_job_root: root.join("supervisor-v1").join("jobs"),
            supervisor_vote_submission_root: root.join("supervisor-v1").join("vote-submissions"),
            snapshot_exporter_ce_datadir,
            snapshot_exporter_input_ref_root: root.join("exporter-v1").join("input-refs"),
            snapshot_exporter_receipt_root: root.join("exporter-v1").join("receipts"),
            ocomp_evm_key_path: root.join("ocomp-evm-key.hex"),
            protocol_bundle_path: root.join("protocol-bundle-v1.ocb1"),
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().role {
        Role::Worker(args) => {
            install_consensus_domain(args.chain_id)?;
            let runtime = RuntimeProfile::resolve(&args.runtime, ProcessRole::Worker)?;
            let limits = poc_schema_limits();
            let canonical_bundle = std::fs::read(&runtime.protocol_bundle_path)?;
            let protocol_bundle = PinnedProtocolBundle::decode(
                &canonical_bundle,
                args.protocol_bundle_hash,
                &limits,
            )?;
            run_one_from_inherited_fd(WorkerConfig {
                expected_effective_uid: runtime.effective_role_uid,
                expected_supervisor_uid: runtime.supervisor_uid,
                identity: EndpointIdentity {
                    chain_id: args.chain_id,
                    genesis_hash: args.genesis_hash,
                    boot_nonce: args.boot_nonce,
                    protocol_bundle_hash: args.protocol_bundle_hash,
                },
                session_generation: args.session_generation,
                cas_root: runtime.cas_root,
                cas_limits: CasLimits {
                    max_object_bytes: CAS_MAX_OBJECT_BYTES,
                    max_total_bytes: CAS_MAX_TOTAL_BYTES,
                },
                inbox_root: runtime.worker_inbox_root,
                inbox_limits: WorkerInboxLimits {
                    max_artifact_bytes: WORKER_INBOX_MAX_ARTIFACT_BYTES,
                    max_total_bytes: WORKER_INBOX_MAX_TOTAL_BYTES,
                },
                connection_fd: SOCKET_ACTIVATION_FD,
                protocol_bundle,
            })?;
            Ok(())
        }
        Role::Supervisor(args) => run_supervisor(&args),
        Role::Follower(args) => run_follower(&args),
        Role::SnapshotExporter(args) => run_snapshot_exporter(&args),
        Role::SignerAddress(args) => print_signer_address(&args),
    }
}

fn print_signer_address(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeProfile::resolve(args, ProcessRole::Supervisor)?;
    require_effective_uid(runtime.effective_role_uid)?;
    let signer =
        OutbeEvmSigner::from_strict_file(runtime.ocomp_evm_key_path, runtime.effective_role_uid)?;
    println!("{}", signer.address());
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComputeMode {
    Validator,
    Follower,
}

impl ComputeMode {
    const fn process_role(self) -> ProcessRole {
        match self {
            Self::Validator => ProcessRole::Supervisor,
            Self::Follower => ProcessRole::Follower,
        }
    }
}

type VotingRuntime<'a> = (
    LocalVoteTransactionPreparerV1<'a, SupervisorDiscovery>,
    SupervisorVoteSubmitterV1<PublicVoteRpcClientV1>,
);

fn open_voting_runtime<'a>(
    mode: ComputeMode,
    discovery: &'a SupervisorDiscovery,
    runtime: &RuntimeProfile,
    identity: EndpointIdentity,
    limits: outbe_ocomp_protocol::SchemaLimits,
) -> Result<Option<VotingRuntime<'a>>, Box<dyn std::error::Error>> {
    if mode == ComputeMode::Follower {
        return Ok(None);
    }
    let evm_signer =
        OutbeEvmSigner::from_strict_file(&runtime.ocomp_evm_key_path, runtime.effective_role_uid)?;
    let vote_preparer =
        LocalVoteTransactionPreparerV1::new(discovery, evm_signer, identity.chain_id, limits)?;
    let sender_address = vote_preparer.sender_address();
    let vote_rpc = PublicVoteRpcClientV1::new(
        required_env("OUTBE_OCOMP_RPC_URL")?,
        SUPERVISOR_VOTE_RPC_MAX_RESPONSE_BYTES,
    )?;
    let vote_submitter = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: runtime.supervisor_vote_submission_root.clone(),
            expected_chain_id: identity.chain_id,
            sender_address,
            limits,
        },
        vote_rpc,
    )?;
    Ok(Some((vote_preparer, vote_submitter)))
}

fn run_supervisor(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    run_compute(args, ComputeMode::Validator)
}

fn run_follower(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    run_compute(args, ComputeMode::Follower)
}

fn run_compute(args: &RuntimeArgs, mode: ComputeMode) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeProfile::resolve(args, mode.process_role())?;
    require_effective_uid(runtime.effective_role_uid)?;
    let limits = poc_schema_limits();
    let identity = EndpointIdentity {
        chain_id: required_env("OCOMP_CHAIN_ID")?.parse()?,
        genesis_hash: required_env("OCOMP_GENESIS_HASH")?.parse()?,
        boot_nonce: required_env("OCOMP_BOOT_NONCE")?.parse()?,
        protocol_bundle_hash: required_env("OCOMP_PROTOCOL_BUNDLE_HASH")?.parse()?,
    };
    install_consensus_domain(identity.chain_id)?;
    let protocol_bundle = PinnedProtocolBundle::decode(
        &std::fs::read(&runtime.protocol_bundle_path)?,
        identity.protocol_bundle_hash,
        &limits,
    )?;
    let discovery = SupervisorDiscovery::open(SupervisorDiscoveryConfig {
        node_socket: runtime.node_supervisor_socket.clone(),
        journal_root: runtime.supervisor_journal_root.clone(),
        expected_node_uid: runtime.node_uid,
        identity,
        limits,
    })?;
    let mut voting = open_voting_runtime(mode, &discovery, &runtime, identity, limits)?;
    let adoption = SupervisorExportAdoption::open(SupervisorExportAdoptionConfig {
        cas_root: runtime.cas_root.clone(),
        cas_limits: CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: CAS_MAX_TOTAL_BYTES,
        },
        input_ref_root: runtime.snapshot_exporter_input_ref_root.clone(),
        receipt_root: runtime.snapshot_exporter_receipt_root.clone(),
        binding_root: runtime.supervisor_export_binding_root.clone(),
        protocol_bundle: protocol_bundle.clone(),
        limits,
    })?;
    let runner = SupervisorJobRunnerV1::open(SupervisorJobRunnerConfigV1 {
        cas_root: runtime.cas_root,
        cas_limits: CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: CAS_MAX_TOTAL_BYTES,
        },
        input_ref_root: runtime.snapshot_exporter_input_ref_root,
        job_root: runtime.supervisor_job_root,
        worker_inbox_root: runtime.worker_inbox_root,
        worker_inbox_limits: WorkerInboxLimits {
            max_artifact_bytes: WORKER_INBOX_MAX_ARTIFACT_BYTES,
            max_total_bytes: WORKER_INBOX_MAX_TOTAL_BYTES,
        },
        worker_socket: runtime.worker_socket,
        expected_worker_uid: runtime.worker_uid,
        identity,
        protocol_bundle,
        limits,
        development_worker: args
            .development_root
            .as_ref()
            .map(|root| {
                Ok::<_, Box<dyn std::error::Error>>(DevelopmentWorkerLaunchV1 {
                    binary: std::env::current_exe()?,
                    root: root.clone(),
                })
            })
            .transpose()?,
    })?;
    let mut handled_job_id = None;
    let mut completed_result: Option<CompletedSupervisorJobV1> = None;
    let mut last_submission_outcome = None;
    loop {
        if let Err(error) = discovery.reconcile_once() {
            eprintln!("OCOMP supervisor discovery retry: {error}");
        }
        if let Some(record) = discovery.current_record()? {
            let job_id = record.spec.summary.job_id;
            if handled_job_id != Some(job_id) {
                if completed_result
                    .as_ref()
                    .is_some_and(|completed| completed.job_id != job_id)
                {
                    completed_result = None;
                    last_submission_outcome = None;
                }
                match adoption.try_adopt(&record) {
                    Ok(SupervisorExportAdoptionOutcome::Adopted(binding)) => {
                        if completed_result.is_none() {
                            match runner.run_to_result(&record, &binding) {
                                Ok(completed) => {
                                    eprintln!(
                                        "OCOMP supervisor computed job {} with {} units and result {}",
                                        completed.job_id,
                                        completed.exact_unit_count,
                                        completed.result_digest
                                    );
                                    completed_result = Some(completed);
                                }
                                Err(error) => {
                                    eprintln!("OCOMP supervisor Lysis job retry: {error}");
                                }
                            }
                        }
                        if let Some(completed) = completed_result.as_ref() {
                            if let Some((vote_preparer, vote_submitter)) = voting.as_mut() {
                                match vote_submitter.reconcile(
                                    &*vote_preparer,
                                    completed.job_id,
                                    completed.result_digest,
                                    &completed.canonical_result,
                                ) {
                                    Ok(outcome) => {
                                        if last_submission_outcome != Some(outcome) {
                                            eprintln!(
                                                "OCOMP supervisor vote submission for {}: {outcome:?}",
                                                completed.job_id
                                            );
                                            last_submission_outcome = Some(outcome);
                                        }
                                        if let VoteSubmissionOutcomeV1::Finalized(inclusion) =
                                            outcome
                                        {
                                            if inclusion.success {
                                                eprintln!(
                                                    "OCOMP supervisor finalized result vote {} in block {}",
                                                    completed.result_digest, inclusion.block_number
                                                );
                                            } else {
                                                eprintln!(
                                                    "OCOMP supervisor result vote {} finalized with a reverted receipt in block {}",
                                                    completed.result_digest, inclusion.block_number
                                                );
                                            }
                                            handled_job_id = Some(job_id);
                                            completed_result = None;
                                        }
                                    }
                                    Err(error) => {
                                        eprintln!(
                                            "OCOMP supervisor vote-submission retry: {error}"
                                        );
                                    }
                                }
                            } else {
                                match discovery.commit_local_result(&completed.canonical_result) {
                                    Ok(committed) => {
                                        eprintln!(
                                            "OCOMP FullNode follower durably committed result {} for job {}",
                                            committed.result_digest, committed.job_id
                                        );
                                        handled_job_id = Some(job_id);
                                        completed_result = None;
                                    }
                                    Err(error) => {
                                        eprintln!("OCOMP FullNode result-commit retry: {error}");
                                    }
                                }
                            }
                        }
                    }
                    Ok(SupervisorExportAdoptionOutcome::Pending) => {
                        if let Err(error) = discovery.open_snapshot_lease(job_id) {
                            eprintln!("OCOMP supervisor snapshot-open retry: {error}");
                        }
                    }
                    Err(error) => eprintln!("OCOMP supervisor export-adoption retry: {error}"),
                }
            }
        }
        std::thread::sleep(SUPERVISOR_RECONCILE_INTERVAL);
    }
}

fn run_snapshot_exporter(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeProfile::resolve(args, ProcessRole::SnapshotExporter)?;
    require_effective_uid(runtime.effective_role_uid)?;
    let limits = poc_schema_limits();
    let identity = EndpointIdentity {
        chain_id: required_env("OCOMP_CHAIN_ID")?.parse()?,
        genesis_hash: required_env("OCOMP_GENESIS_HASH")?.parse()?,
        boot_nonce: required_env("OCOMP_BOOT_NONCE")?.parse()?,
        protocol_bundle_hash: required_env("OCOMP_PROTOCOL_BUNDLE_HASH")?.parse()?,
    };
    install_consensus_domain(identity.chain_id)?;
    let protocol_bundle = PinnedProtocolBundle::decode(
        &std::fs::read(&runtime.protocol_bundle_path)?,
        identity.protocol_bundle_hash,
        &limits,
    )?;
    let mut exporter = SnapshotExporter::open(SnapshotExporterConfig {
        node: SnapshotExporterNodeConfig {
            node_socket: runtime.node_snapshot_exporter_socket,
            expected_node_uid: runtime.node_uid,
            identity,
            limits,
        },
        ce_datadir: runtime.snapshot_exporter_ce_datadir,
        ce_environment: EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: identity.chain_id,
            genesis_hash: identity.genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: CE_TREE_FORMAT.to_owned(),
            vendor_revision: CE_VENDOR_REVISION.to_owned(),
        },
        mongo: MongoStorageConfig {
            uri: required_env("OUTBE_PROJECTION_MONGODB_URI")?,
            database: required_env("OUTBE_PROJECTION_MONGODB_DATABASE")?,
        },
        cas_root: runtime.cas_root,
        cas_limits: CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: CAS_MAX_TOTAL_BYTES,
        },
        input_ref_root: runtime.snapshot_exporter_input_ref_root,
        receipt_root: runtime.snapshot_exporter_receipt_root,
        protocol_bundle,
        tribute_page_limit: SNAPSHOT_EXPORTER_TRIBUTE_PAGE_LIMIT,
        max_recoverable_prepared_jobs: SNAPSHOT_EXPORTER_MAX_RECOVERABLE_PREPARED_JOBS,
        projection_start_block: PROJECTION_START_BLOCK,
    })?;
    let mut after_lease_generation = 0_u64;
    loop {
        match exporter.reconcile_once(after_lease_generation) {
            Ok(reconciled) => {
                after_lease_generation = reconciled.next_lease_generation;
            }
            Err(error) => eprintln!("OCOMP snapshot exporter retry: {error}"),
        }
        std::thread::sleep(SNAPSHOT_EXPORTER_RECONCILE_INTERVAL);
    }
}

fn required_env(name: &'static str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("required environment variable {name} is missing").into())
}

fn install_consensus_domain(chain_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    init_consensus_chain_id(chain_id);
    let installed = consensus_chain_id();
    if installed != chain_id {
        return Err(format!(
            "process consensus domain is already bound to chain {installed}, requested {chain_id}"
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn fixed_role_cli_rejects_caller_selected_uids_and_paths() {
        assert!(Cli::try_parse_from([
            "outbe-ocomp",
            "supervisor",
            "--expected-effective-user",
            "attacker"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "outbe-ocomp",
            "worker",
            "--expected-effective-user",
            "attacker",
            "--expected-supervisor-user",
            "attacker",
            "--chain-id",
            "1",
            "--genesis-hash",
            HASH,
            "--boot-nonce",
            HASH,
            "--protocol-bundle-hash",
            HASH,
            "--session-generation",
            "1",
            "--cas-root",
            "/tmp/attacker-cas",
            "--cas-max-object-bytes",
            "1",
            "--cas-max-total-bytes",
            "1",
            "--connection-fd",
            "9",
        ])
        .is_err());
    }

    #[test]
    fn development_profile_is_rooted_and_does_not_relax_release_defaults() {
        let root = tempfile::tempdir().unwrap();
        let supervisor_socket = root.path().join("short-supervisor.sock");
        let args = RuntimeArgs {
            development_root: Some(root.path().to_path_buf()),
            development_ce_datadir: None,
            development_node_supervisor_socket: Some(supervisor_socket.clone()),
            development_node_snapshot_exporter_socket: None,
        };
        let profile = RuntimeProfile::resolve(&args, ProcessRole::Supervisor).unwrap();

        assert_eq!(profile.effective_role_uid, effective_uid().unwrap());
        assert_eq!(profile.node_supervisor_socket, supervisor_socket);
        assert!(profile.cas_root.starts_with(root.path()));
        assert!(profile.protocol_bundle_path.starts_with(root.path()));
        assert!(
            RuntimeProfile::resolve(&args, ProcessRole::SnapshotExporter).is_err(),
            "the exporter must receive the real validator CE data path"
        );

        let relative = RuntimeArgs {
            development_root: Some(PathBuf::from("relative-domain")),
            development_ce_datadir: None,
            development_node_supervisor_socket: None,
            development_node_snapshot_exporter_socket: None,
        };
        assert!(RuntimeProfile::resolve(&relative, ProcessRole::Supervisor).is_err());
    }

    #[test]
    fn production_layout_derives_every_path_from_one_base_path() {
        let config = ProductionConfig::from_lookup(|name| match name {
            "OUTBE_OCOMP_BASE_PATH" => Some("/srv/outbe".into()),
            "OUTBE_OCOMP_NODE_USER" => Some("validator-node".into()),
            _ => None,
        })
        .unwrap();
        let layout = ProductionLayout::from_base(&config.base_path).unwrap();

        assert_eq!(config.node_user, "validator-node");
        assert_eq!(
            layout.node_supervisor_socket,
            PathBuf::from("/srv/outbe/ocomp/run/node-supervisor.sock")
        );
        assert_eq!(
            layout.supervisor_vote_submission_root,
            PathBuf::from("/srv/outbe/ocomp/data/supervisor-v1/vote-submissions")
        );
        assert_eq!(
            layout.protocol_bundle_path,
            PathBuf::from("/srv/outbe/protocol-bundle-v1.ocb1")
        );
        assert_eq!(
            layout.ocomp_evm_key_path,
            PathBuf::from("/srv/outbe/ocomp/data/keys/ocomp-evm-key.hex")
        );
    }

    #[test]
    fn production_config_requires_node_user_and_rejects_unsafe_base_paths() {
        assert!(ProductionConfig::from_lookup(|_| None).is_err());
        assert!(ProductionLayout::from_base(std::path::Path::new("/")).is_err());
        assert!(ProductionLayout::from_base(std::path::Path::new("relative")).is_err());
    }

    #[test]
    fn fullnode_follower_builds_compute_runtime_without_any_voting_material() {
        let root = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "outbe-ocomp",
            "follower",
            "--development-root",
            root.path().to_str().unwrap(),
        ])
        .expect("parse keyless FullNode follower role");
        let Role::Follower(args) = cli.role else {
            panic!("expected follower role");
        };
        let runtime = RuntimeProfile::resolve(&args, ProcessRole::Follower).unwrap();
        assert!(!runtime.ocomp_evm_key_path.exists());
        let limits = poc_schema_limits();
        let identity = EndpointIdentity {
            chain_id: 42,
            genesis_hash: B256::repeat_byte(1),
            boot_nonce: B256::repeat_byte(2),
            protocol_bundle_hash: B256::repeat_byte(3),
        };
        let discovery = SupervisorDiscovery::open(SupervisorDiscoveryConfig {
            node_socket: runtime.node_supervisor_socket.clone(),
            journal_root: runtime.supervisor_journal_root.clone(),
            expected_node_uid: runtime.node_uid,
            identity,
            limits,
        })
        .unwrap();

        let voting = open_voting_runtime(
            ComputeMode::Follower,
            &discovery,
            &runtime,
            identity,
            limits,
        )
        .expect("follower does not read validator voting material");
        assert!(voting.is_none());
    }

    #[test]
    fn process_consensus_domain_is_bound_to_the_role_chain() {
        const ROLE_CHAIN_ID: u64 = 42;

        install_consensus_domain(ROLE_CHAIN_ID).unwrap();

        assert_eq!(consensus_chain_id(), ROLE_CHAIN_ID);
        assert_eq!(
            outbe_consensus::config::outbe_app_namespace(),
            [b"outbe".as_slice(), ROLE_CHAIN_ID.to_be_bytes().as_slice()].concat()
        );
    }
}
