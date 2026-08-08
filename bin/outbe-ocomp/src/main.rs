use std::env;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use alloy_primitives::{keccak256, B256};
use clap::{Args, Parser, Subcommand};
use outbe_consensus::{config::init_consensus_chain_id, proof::constants::consensus_chain_id};
use outbe_ocomp::bundle::PinnedProtocolBundle;
use outbe_ocomp::cas::CasLimits;
use outbe_ocomp::control::{effective_uid, poc_schema_limits, EndpointIdentity};
use outbe_ocomp::inbox::WorkerInboxLimits;
use outbe_ocomp::result_attestation::LocalResultVoteAttesterV1;
use outbe_ocomp::result_signer::OcompSigner;
use outbe_ocomp::rpc_discovery::{FinalizedRpcDiscoveryConfigV1, FinalizedRpcDiscoveryV1};
use outbe_ocomp::rpc_input_exporter::{RpcInputExporterConfigV1, RpcInputExporterV1};
use outbe_ocomp::rpc_projection::RpcProjectionConfigV1;
use outbe_ocomp::sign_once::SignOnceStore;
use outbe_ocomp::supervisor_export::{
    SupervisorExportAdoption, SupervisorExportAdoptionConfig, SupervisorExportAdoptionOutcome,
};
use outbe_ocomp::supervisor_job::{
    CompletedSupervisorJobV1, SupervisorJobRunnerConfigV1, SupervisorJobRunnerErrorV1,
    SupervisorJobRunnerV1,
};
use outbe_ocomp::vote_submitter::{
    LocalVoteTransactionPreparerV1, PublicVoteRpcClientV1, SupervisorVoteSubmitterV1,
    VoteSubmissionConfigV1, VoteSubmissionOutcomeV1,
};
use outbe_ocomp::worker::{run_worker, WorkerConfig};
use outbe_ocomp::worker_transport::{SupervisorWorkerServerV1, MAX_REGISTERED_WORKERS};
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
    /// Stable ordinal of this Worker process on the host (0..4 exclusive).
    #[arg(long, default_value_t = 0)]
    worker_ordinal: u8,
}

#[derive(Clone, Debug, Args)]
struct RuntimeArgs {
    /// Unprivileged OCM measurement namespace; accepted only by debug builds.
    #[arg(long, hide = true, value_name = "PATH")]
    development_root: Option<PathBuf>,
    /// Loopback Supervisor endpoint where OCOMP workers register for work.
    #[arg(long, value_name = "IP:PORT", default_value = "127.0.0.1:9765")]
    supervisor_address: SocketAddr,
}

const BASE_PATH_ENV: &str = "OUTBE_OCOMP_BASE_PATH";
const VALIDATOR_INDEX_ENV: &str = "OCOMP_VALIDATOR_INDEX";
const CAS_MAX_OBJECT_BYTES: u64 = 1_048_576;
const CAS_MAX_TOTAL_BYTES: u64 = OCOMP_POC_CAS_QUOTA_BYTES;
const WORKER_INBOX_MAX_ARTIFACT_BYTES: u64 = 1_048_576;
const WORKER_INBOX_MAX_TOTAL_BYTES: u64 = 67_108_864;
const SUPERVISOR_RPC_MAX_RESPONSE_BYTES: usize = 33_554_432;
const SUPERVISOR_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const OCOMP_TRIBUTE_PAGE_LIMIT: usize = 256;
const PROJECTION_START_BLOCK: u64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductionConfig {
    base_path: PathBuf,
    validator_index: u16,
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
            .ok_or("OUTBE_OCOMP_BASE_PATH is required")?;
        let validator_index = lookup(VALIDATOR_INDEX_ENV)
            .ok_or("OCOMP_VALIDATOR_INDEX is required")?
            .into_string()
            .map_err(|_| "OCOMP_VALIDATOR_INDEX must be valid UTF-8")?
            .parse::<u16>()
            .map_err(|_| "OCOMP_VALIDATOR_INDEX must be an unsigned 16-bit integer")?;
        Ok(Self {
            base_path,
            validator_index,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductionLayout {
    cas_root: PathBuf,
    worker_inbox_root: PathBuf,
    supervisor_journal_root: PathBuf,
    snapshot_exporter_journal_root: PathBuf,
    supervisor_export_binding_root: PathBuf,
    supervisor_job_root: PathBuf,
    supervisor_vote_submission_root: PathBuf,
    supervisor_sign_once_root: PathBuf,
    snapshot_exporter_input_ref_root: PathBuf,
    snapshot_exporter_receipt_root: PathBuf,
    ocomp_evm_key_path: PathBuf,
    ocomp_result_key_path: PathBuf,
    protocol_bundle_path: PathBuf,
}

impl ProductionLayout {
    fn from_base(
        base_path: &Path,
        validator_index: u16,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !base_path.is_absolute() || base_path.parent().is_none() || base_path == Path::new("/") {
            return Err("OCOMP base path must be a non-root absolute path".into());
        }

        let domain_root = base_path
            .join(format!("validator-{validator_index}"))
            .join("ocomp")
            .join("domain-v1");
        let supervisor_root = domain_root.join("supervisor-v1");
        let exporter_root = domain_root.join("exporter-v1");
        Ok(Self {
            cas_root: domain_root.join("cas-v1"),
            worker_inbox_root: domain_root.join("worker-inbox-v1"),
            supervisor_journal_root: supervisor_root.join("discovery"),
            snapshot_exporter_journal_root: exporter_root.join("discovery"),
            supervisor_export_binding_root: supervisor_root.join("export-bindings"),
            supervisor_job_root: supervisor_root.join("jobs"),
            supervisor_vote_submission_root: supervisor_root.join("vote-submissions"),
            supervisor_sign_once_root: supervisor_root.join("sign-once"),
            snapshot_exporter_input_ref_root: exporter_root.join("input-refs"),
            snapshot_exporter_receipt_root: exporter_root.join("receipts"),
            ocomp_evm_key_path: domain_root.join("ocomp-evm-key.hex"),
            ocomp_result_key_path: domain_root.join("ocomp-key-v1.hex"),
            protocol_bundle_path: domain_root.join("protocol-bundle-v1.ocb1"),
        })
    }
}

#[derive(Clone, Debug)]
struct RuntimeProfile {
    owner_uid: u32,
    supervisor_address: SocketAddr,
    cas_root: PathBuf,
    worker_inbox_root: PathBuf,
    supervisor_journal_root: PathBuf,
    snapshot_exporter_journal_root: PathBuf,
    supervisor_export_binding_root: PathBuf,
    supervisor_job_root: PathBuf,
    supervisor_vote_submission_root: PathBuf,
    supervisor_sign_once_root: PathBuf,
    snapshot_exporter_input_ref_root: PathBuf,
    snapshot_exporter_receipt_root: PathBuf,
    ocomp_evm_key_path: PathBuf,
    ocomp_result_key_path: PathBuf,
    protocol_bundle_path: PathBuf,
}

impl RuntimeProfile {
    fn resolve(args: &RuntimeArgs) -> Result<Self, Box<dyn std::error::Error>> {
        if !args.supervisor_address.ip().is_loopback() || args.supervisor_address.port() == 0 {
            return Err(
                "--supervisor-address must be a nonzero loopback registration endpoint".into(),
            );
        }
        let Some(root) = args.development_root.as_ref() else {
            let production = ProductionConfig::from_environment()?;
            let layout =
                ProductionLayout::from_base(&production.base_path, production.validator_index)?;
            return Ok(Self {
                owner_uid: effective_uid()?,
                supervisor_address: args.supervisor_address,
                cas_root: layout.cas_root,
                worker_inbox_root: layout.worker_inbox_root,
                supervisor_journal_root: layout.supervisor_journal_root,
                snapshot_exporter_journal_root: layout.snapshot_exporter_journal_root,
                supervisor_export_binding_root: layout.supervisor_export_binding_root,
                supervisor_job_root: layout.supervisor_job_root,
                supervisor_vote_submission_root: layout.supervisor_vote_submission_root,
                supervisor_sign_once_root: layout.supervisor_sign_once_root,
                snapshot_exporter_input_ref_root: layout.snapshot_exporter_input_ref_root,
                snapshot_exporter_receipt_root: layout.snapshot_exporter_receipt_root,
                ocomp_evm_key_path: layout.ocomp_evm_key_path,
                ocomp_result_key_path: layout.ocomp_result_key_path,
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
        let uid = effective_uid()?;
        Ok(Self {
            owner_uid: uid,
            supervisor_address: args.supervisor_address,
            cas_root: root.join("cas-v1"),
            worker_inbox_root: root.join("worker-inbox-v1"),
            supervisor_journal_root: root.join("supervisor-v1").join("discovery"),
            snapshot_exporter_journal_root: root.join("exporter-v1").join("discovery"),
            supervisor_export_binding_root: root.join("supervisor-v1").join("export-bindings"),
            supervisor_job_root: root.join("supervisor-v1").join("jobs"),
            supervisor_vote_submission_root: root.join("supervisor-v1").join("vote-submissions"),
            supervisor_sign_once_root: root.join("supervisor-v1").join("sign-once"),
            snapshot_exporter_input_ref_root: root.join("exporter-v1").join("input-refs"),
            snapshot_exporter_receipt_root: root.join("exporter-v1").join("receipts"),
            ocomp_evm_key_path: root.join("ocomp-evm-key.hex"),
            ocomp_result_key_path: root.join("ocomp-key-v1.hex"),
            protocol_bundle_path: root.join("protocol-bundle-v1.ocb1"),
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().role {
        Role::Worker(args) => {
            install_consensus_domain(args.chain_id)?;
            let runtime = RuntimeProfile::resolve(&args.runtime)?;
            let limits = poc_schema_limits();
            let canonical_bundle = std::fs::read(&runtime.protocol_bundle_path)?;
            let protocol_bundle = PinnedProtocolBundle::decode(
                &canonical_bundle,
                args.protocol_bundle_hash,
                &limits,
            )?;
            run_worker(WorkerConfig {
                identity: EndpointIdentity {
                    chain_id: args.chain_id,
                    genesis_hash: args.genesis_hash,
                    boot_nonce: worker_process_nonce(args.boot_nonce, args.worker_ordinal)?,
                    protocol_bundle_hash: args.protocol_bundle_hash,
                },
                supervisor_address: runtime.supervisor_address,
                observability_address: worker_observability_address(
                    runtime.supervisor_address,
                    args.worker_ordinal,
                )?,
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
                protocol_bundle,
            })?;
            Ok(())
        }
        Role::Supervisor(args) => run_supervisor(&args),
        Role::SnapshotExporter(args) => run_snapshot_exporter(&args),
        Role::SignerAddress(args) => print_signer_address(&args),
    }
}

fn worker_process_nonce(host_boot_nonce: B256, worker_ordinal: u8) -> Result<B256, &'static str> {
    if usize::from(worker_ordinal) >= MAX_REGISTERED_WORKERS {
        return Err("worker ordinal exceeds the supported per-Supervisor worker count");
    }
    let mut preimage = Vec::with_capacity(64);
    preimage.extend_from_slice(b"OCOMP_WORKER_PROCESS_NONCE_V1");
    preimage.extend_from_slice(host_boot_nonce.as_slice());
    preimage.push(worker_ordinal);
    let nonce = keccak256(preimage);
    if nonce.is_zero() {
        Err("derived worker process nonce is reserved zero")
    } else {
        Ok(nonce)
    }
}

fn worker_observability_address(
    supervisor_address: SocketAddr,
    worker_ordinal: u8,
) -> Result<SocketAddr, &'static str> {
    if usize::from(worker_ordinal) >= MAX_REGISTERED_WORKERS {
        return Err("worker ordinal exceeds the supported per-Supervisor worker count");
    }
    let offset = 2_u16
        .checked_add(u16::from(worker_ordinal))
        .ok_or("worker observability port offset overflow")?;
    let port = supervisor_address
        .port()
        .checked_add(offset)
        .ok_or("worker observability port overflow")?;
    Ok(SocketAddr::new(supervisor_address.ip(), port))
}

fn print_signer_address(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeProfile::resolve(args)?;
    let signer = OutbeEvmSigner::from_strict_file(runtime.ocomp_evm_key_path, runtime.owner_uid)?;
    println!("{}", signer.address());
    Ok(())
}

fn run_supervisor(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeProfile::resolve(args)?;
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
    let fork_id = protocol_bundle.bundle().fork_id;
    let rpc_url = required_env("OUTBE_OCOMP_RPC_URL")?;
    let mut discovery = FinalizedRpcDiscoveryV1::open(FinalizedRpcDiscoveryConfigV1 {
        rpc_url: rpc_url.clone(),
        rpc_max_response_bytes: SUPERVISOR_RPC_MAX_RESPONSE_BYTES,
        journal_root: runtime.supervisor_journal_root.clone(),
        projection_start_block: PROJECTION_START_BLOCK,
        identity,
        fork_id: protocol_bundle.bundle().fork_id,
        limits,
    })?;
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
    let worker_server = SupervisorWorkerServerV1::start(
        runtime.supervisor_address,
        identity,
        required_env("OCOMP_REGISTRY_GENERATION")?.parse()?,
        limits,
    )?;
    let runner = Arc::new(SupervisorJobRunnerV1::open(
        SupervisorJobRunnerConfigV1 {
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
            protocol_bundle,
            limits,
        },
        worker_server.dispatcher(),
    )?);
    let evm_signer =
        OutbeEvmSigner::from_strict_file(&runtime.ocomp_evm_key_path, runtime.owner_uid)?;
    let result_signer = OcompSigner::from_file(&runtime.ocomp_result_key_path, runtime.owner_uid)?;
    let sign_once =
        SignOnceStore::open(runtime.supervisor_sign_once_root, runtime.owner_uid, limits)?;
    let attester =
        LocalResultVoteAttesterV1::new(identity, fork_id, result_signer, sign_once, limits)?;
    let vote_preparer =
        LocalVoteTransactionPreparerV1::new(evm_signer, attester, identity.chain_id, limits)?;
    let sender_address = vote_preparer.sender_address();
    let vote_rpc = PublicVoteRpcClientV1::new(rpc_url, SUPERVISOR_RPC_MAX_RESPONSE_BYTES)?;
    let mut vote_submitter = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: runtime.supervisor_vote_submission_root,
            expected_chain_id: identity.chain_id,
            sender_address,
            limits,
        },
        vote_rpc,
    )?;
    let mut handled_job_id = None;
    let mut completed_result: Option<CompletedSupervisorJobV1> = None;
    let mut last_submission_outcome = None;
    let mut running_job_id = None;
    let mut running_job_cancel: Option<Arc<AtomicBool>> = None;
    let (job_result_tx, job_result_rx) = mpsc::channel::<(
        B256,
        Result<CompletedSupervisorJobV1, SupervisorJobRunnerErrorV1>,
    )>();
    loop {
        if let Err(error) = runner.poll_workers() {
            eprintln!("OCOMP supervisor worker-registration retry: {error}");
        }
        if let Err(error) = discovery.reconcile_once() {
            eprintln!("OCOMP supervisor discovery retry: {error}");
        }
        cancel_superseded_job(
            running_job_id,
            discovery
                .current_record()
                .map(|record| record.spec.summary.job_id),
            running_job_cancel.as_deref(),
        );
        match job_result_rx.try_recv() {
            Ok((job_id, Ok(completed))) => {
                eprintln!(
                    "OCOMP supervisor computed job {} with {} units and result {}",
                    completed.job_id, completed.exact_unit_count, completed.result_digest
                );
                if running_job_id == Some(job_id) {
                    running_job_id = None;
                    running_job_cancel = None;
                }
                if discovery
                    .current_record()
                    .is_some_and(|record| record.spec.summary.job_id == job_id)
                {
                    completed_result = Some(completed);
                }
            }
            Ok((job_id, Err(error))) => {
                if running_job_id == Some(job_id) {
                    running_job_id = None;
                    running_job_cancel = None;
                }
                eprintln!("OCOMP supervisor Lysis job {job_id} retry: {error}");
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("OCOMP Supervisor job result channel disconnected".into());
            }
        }
        if let Some(record) = discovery.current_record() {
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
                        if completed_result.is_none() && running_job_id.is_none() {
                            let runner = Arc::clone(&runner);
                            let result_tx = job_result_tx.clone();
                            let job_record = record.clone();
                            let job_id = job_record.spec.summary.job_id;
                            let cancelled = Arc::new(AtomicBool::new(false));
                            running_job_id = Some(job_id);
                            running_job_cancel = Some(Arc::clone(&cancelled));
                            std::thread::Builder::new()
                                .name("ocomp-supervisor-job".to_owned())
                                .spawn(move || {
                                    let result =
                                        runner.run_to_result(&job_record, &binding, &cancelled);
                                    let _ = result_tx.send((job_id, result));
                                })?;
                        }
                        if let Some(completed) = completed_result.as_ref() {
                            match vote_submitter.reconcile(
                                &vote_preparer,
                                completed.job_id,
                                completed.result_digest,
                                &completed.canonical_result,
                                &record.spec,
                            ) {
                                Ok(outcome) => {
                                    if last_submission_outcome != Some(outcome) {
                                        eprintln!(
                                            "OCOMP supervisor vote submission for {}: {outcome:?}",
                                            completed.job_id
                                        );
                                        last_submission_outcome = Some(outcome);
                                    }
                                    if let VoteSubmissionOutcomeV1::Finalized(inclusion) = outcome {
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
                                    eprintln!("OCOMP supervisor vote-submission retry: {error}");
                                }
                            }
                        }
                    }
                    Ok(SupervisorExportAdoptionOutcome::Pending) => {}
                    Err(error) => eprintln!("OCOMP supervisor export-adoption retry: {error}"),
                }
            }
        }
        std::thread::sleep(SUPERVISOR_RECONCILE_INTERVAL);
    }
}

fn cancel_superseded_job(
    running_job_id: Option<B256>,
    current_job_id: Option<B256>,
    cancelled: Option<&AtomicBool>,
) {
    if running_job_id.is_some() && running_job_id != current_job_id {
        if let Some(cancelled) = cancelled {
            cancelled.store(true, Ordering::Release);
        }
    }
}

fn run_snapshot_exporter(args: &RuntimeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeProfile::resolve(args)?;
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
    let rpc_url = required_env("OUTBE_OCOMP_RPC_URL")?;
    let mut discovery = FinalizedRpcDiscoveryV1::open(FinalizedRpcDiscoveryConfigV1 {
        rpc_url: rpc_url.clone(),
        rpc_max_response_bytes: SUPERVISOR_RPC_MAX_RESPONSE_BYTES,
        journal_root: runtime.snapshot_exporter_journal_root,
        projection_start_block: PROJECTION_START_BLOCK,
        identity,
        fork_id: protocol_bundle.bundle().fork_id,
        limits,
    })?;
    let mut exporter = RpcInputExporterV1::open(RpcInputExporterConfigV1 {
        rpc_url: rpc_url.clone(),
        rpc_max_response_bytes: SUPERVISOR_RPC_MAX_RESPONSE_BYTES,
        projection: RpcProjectionConfigV1 {
            rpc_url,
            rpc_max_response_bytes: SUPERVISOR_RPC_MAX_RESPONSE_BYTES,
            mongo: MongoStorageConfig {
                uri: required_env("OUTBE_OCOMP_PROJECTION_MONGODB_URI")?,
                database: required_env("OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE")?,
            },
            chain_id: identity.chain_id,
            genesis_hash: identity.genesis_hash,
            start_block: PROJECTION_START_BLOCK,
            tribute_page_limit: OCOMP_TRIBUTE_PAGE_LIMIT,
        },
        chain_id: identity.chain_id,
        genesis_hash: identity.genesis_hash,
        fork_id: protocol_bundle.bundle().fork_id,
        protocol_bundle_hash: identity.protocol_bundle_hash,
        cas_root: runtime.cas_root,
        cas_limits: CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: CAS_MAX_TOTAL_BYTES,
        },
        input_ref_root: runtime.snapshot_exporter_input_ref_root,
        receipt_root: runtime.snapshot_exporter_receipt_root,
        protocol_bundle,
        limits,
    })?;
    let mut exported_job_id = None;
    loop {
        if let Err(error) = discovery.reconcile_once() {
            eprintln!("OCOMP snapshot exporter discovery retry: {error}");
        }
        if let Some(record) = discovery.current_record() {
            let job_id = record.spec.summary.job_id;
            if exported_job_id != Some(job_id) {
                match exporter.export(&record) {
                    Ok(()) => {
                        eprintln!("OCOMP snapshot exporter published finalized input for {job_id}");
                        exported_job_id = Some(job_id);
                    }
                    Err(error) => eprintln!("OCOMP snapshot exporter public input retry: {error}"),
                }
            }
        }
        std::thread::sleep(SUPERVISOR_RECONCILE_INTERVAL);
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
    fn newer_discovery_cancels_the_running_job_before_replacement() {
        let running = B256::repeat_byte(0xA1);
        let newer = B256::repeat_byte(0xB2);
        let cancelled = AtomicBool::new(false);

        cancel_superseded_job(Some(running), Some(running), Some(&cancelled));
        assert!(!cancelled.load(Ordering::Acquire));

        cancel_superseded_job(Some(running), Some(newer), Some(&cancelled));
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn removed_uid_cli_options_are_rejected() {
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
        let args = RuntimeArgs {
            development_root: Some(root.path().to_path_buf()),
            supervisor_address: "127.0.0.1:9765".parse().unwrap(),
        };
        let profile = RuntimeProfile::resolve(&args).unwrap();

        assert_eq!(profile.owner_uid, effective_uid().unwrap());
        assert!(profile.cas_root.starts_with(root.path()));
        assert!(profile.protocol_bundle_path.starts_with(root.path()));

        let relative = RuntimeArgs {
            development_root: Some(PathBuf::from("relative-domain")),
            supervisor_address: "127.0.0.1:9765".parse().unwrap(),
        };
        assert!(RuntimeProfile::resolve(&relative).is_err());
    }

    #[test]
    fn worker_ordinals_derive_four_distinct_process_nonces() {
        let host_boot_nonce = B256::repeat_byte(0x44);
        let nonces = (0_u8..4)
            .map(|ordinal| worker_process_nonce(host_boot_nonce, ordinal).unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(nonces.len(), 4);
        assert!(worker_process_nonce(host_boot_nonce, 4).is_err());
    }

    #[test]
    fn worker_observability_ports_follow_the_worker_ordinal() {
        let supervisor = "127.0.0.1:9765".parse().unwrap();
        assert_eq!(
            worker_observability_address(supervisor, 0).unwrap(),
            "127.0.0.1:9767".parse().unwrap()
        );
        assert_eq!(
            worker_observability_address(supervisor, 3).unwrap(),
            "127.0.0.1:9770".parse().unwrap()
        );
        assert!(worker_observability_address(supervisor, 4).is_err());
    }

    #[test]
    fn production_layout_is_isolated_under_the_selected_validator_directory() {
        let base = tempfile::tempdir().unwrap();
        let config = ProductionConfig::from_lookup(|name| match name {
            "OUTBE_OCOMP_BASE_PATH" => Some(base.path().as_os_str().to_owned()),
            "OCOMP_VALIDATOR_INDEX" => Some("7".into()),
            _ => None,
        })
        .unwrap();
        let layout =
            ProductionLayout::from_base(&config.base_path, config.validator_index).unwrap();
        let domain = base
            .path()
            .join("validator-7")
            .join("ocomp")
            .join("domain-v1");

        assert_eq!(
            layout.supervisor_vote_submission_root,
            domain.join("supervisor-v1").join("vote-submissions")
        );
        assert_eq!(
            layout.protocol_bundle_path,
            domain.join("protocol-bundle-v1.ocb1")
        );
        assert_eq!(layout.ocomp_evm_key_path, domain.join("ocomp-evm-key.hex"));
        assert_eq!(
            layout.ocomp_result_key_path,
            domain.join("ocomp-key-v1.hex")
        );
        assert_eq!(
            layout.supervisor_sign_once_root,
            domain.join("supervisor-v1").join("sign-once")
        );
    }

    #[test]
    fn production_config_requires_explicit_base_path_and_validator_index() {
        let base = tempfile::tempdir().unwrap();
        assert!(ProductionConfig::from_lookup(|_| None).is_err());
        assert!(ProductionConfig::from_lookup(|name| {
            (name == "OUTBE_OCOMP_BASE_PATH").then(|| base.path().as_os_str().to_owned())
        })
        .is_err());
        assert!(ProductionLayout::from_base(std::path::Path::new("/"), 0).is_err());
        assert!(ProductionLayout::from_base(std::path::Path::new("relative"), 0).is_err());
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
