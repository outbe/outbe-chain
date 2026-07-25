use std::env;
use std::path::PathBuf;
use std::time::Duration;

use alloy_primitives::B256;
use clap::{Args, Parser, Subcommand};
use outbe_compressed_entities::{
    CeTopologyV1, EnvironmentIdentity, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_ocomp::bundle::PinnedProtocolBundle;
use outbe_ocomp::cas::CasLimits;
use outbe_ocomp::control::{
    poc_schema_limits, require_effective_user, uid_for_user, EndpointIdentity,
};
use outbe_ocomp::inbox::WorkerInboxLimits;
use outbe_ocomp::snapshot_client::SnapshotExporterNodeConfig;
use outbe_ocomp::snapshot_exporter::{SnapshotExporter, SnapshotExporterConfig};
use outbe_ocomp::supervisor::{SupervisorDiscovery, SupervisorDiscoveryConfig};
use outbe_ocomp::supervisor_export::{
    SupervisorExportAdoption, SupervisorExportAdoptionConfig, SupervisorExportAdoptionOutcome,
};
use outbe_ocomp::worker::{run_one_from_inherited_fd, WorkerConfig};
use outbe_offchain_storage::MongoStorageConfig;

#[derive(Debug, Parser)]
#[command(name = "outbe-ocomp")]
#[command(about = "Fixed Off-chain Computation PoC process roles")]
struct Cli {
    #[command(subcommand)]
    role: Role,
}

#[derive(Debug, Subcommand)]
enum Role {
    Supervisor,
    SnapshotExporter,
    Worker(WorkerArgs),
    Relay,
}

#[derive(Clone, Debug, Args)]
struct WorkerArgs {
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

const SUPERVISOR_USER: &str = "outbe-ocomp-supervisor";
const SNAPSHOT_EXPORTER_USER: &str = "outbe-ocomp-export";
const WORKER_USER: &str = "outbe-ocomp-worker";
const RELAY_USER: &str = "outbe-ocomp-relay";
const CAS_ROOT: &str = "/var/lib/outbe-ocomp/cas-v1";
const CAS_MAX_OBJECT_BYTES: u64 = 1_048_576;
const CAS_MAX_TOTAL_BYTES: u64 = 8_589_934_592;
const WORKER_INBOX_ROOT: &str = "/var/lib/outbe-ocomp/worker-inbox-v1";
const WORKER_INBOX_MAX_ARTIFACT_BYTES: u64 = 1_048_576;
const WORKER_INBOX_MAX_TOTAL_BYTES: u64 = 67_108_864;
const SOCKET_ACTIVATION_FD: i32 = 0;
const NODE_USER: &str = "outbe";
const NODE_CONTROL_SOCKET: &str = "/run/outbe-ocomp/node.sock";
const SUPERVISOR_JOURNAL_ROOT: &str = "/var/lib/outbe-ocomp/supervisor-v1/discovery";
const SUPERVISOR_EXPORT_BINDING_ROOT: &str = "/var/lib/outbe-ocomp/supervisor-v1/export-bindings";
const SUPERVISOR_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const SNAPSHOT_EXPORTER_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const SNAPSHOT_EXPORTER_CE_DATADIR: &str = "/var/lib/outbe/ce";
const SNAPSHOT_EXPORTER_INPUT_REF_ROOT: &str = "/var/lib/outbe-ocomp/exporter-v1/input-refs";
const SNAPSHOT_EXPORTER_RECEIPT_ROOT: &str = "/var/lib/outbe-ocomp/exporter-v1/receipts";
const SNAPSHOT_EXPORTER_TRIBUTE_PAGE_LIMIT: usize = 256;
const SNAPSHOT_EXPORTER_MAX_RECOVERABLE_PREPARED_JOBS: usize = 8;
const PROJECTION_START_BLOCK: u64 = 1;
const CE_TREE_FORMAT: &str = "ckb-smt-v0.6.1-poseidon-catalog-v3";
const CE_VENDOR_REVISION: &str = "ad555350c866b2265d87d2d7fbd146fbc918bfe5";
const PROTOCOL_BUNDLE_PATH: &str = "/etc/outbe/ocomp/protocol-bundle-v1.ocb1";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().role {
        Role::Worker(args) => {
            let limits = poc_schema_limits();
            let canonical_bundle = std::fs::read(PROTOCOL_BUNDLE_PATH)?;
            let protocol_bundle = PinnedProtocolBundle::decode(
                &canonical_bundle,
                args.protocol_bundle_hash,
                &limits,
            )?;
            run_one_from_inherited_fd(WorkerConfig {
                expected_effective_user: WORKER_USER.to_owned(),
                expected_supervisor_user: SUPERVISOR_USER.to_owned(),
                identity: EndpointIdentity {
                    chain_id: args.chain_id,
                    genesis_hash: args.genesis_hash,
                    boot_nonce: args.boot_nonce,
                    protocol_bundle_hash: args.protocol_bundle_hash,
                },
                session_generation: args.session_generation,
                cas_root: PathBuf::from(CAS_ROOT),
                cas_limits: CasLimits {
                    max_object_bytes: CAS_MAX_OBJECT_BYTES,
                    max_total_bytes: CAS_MAX_TOTAL_BYTES,
                },
                inbox_root: PathBuf::from(WORKER_INBOX_ROOT),
                inbox_limits: WorkerInboxLimits {
                    max_artifact_bytes: WORKER_INBOX_MAX_ARTIFACT_BYTES,
                    max_total_bytes: WORKER_INBOX_MAX_TOTAL_BYTES,
                },
                connection_fd: SOCKET_ACTIVATION_FD,
                protocol_bundle,
            })?;
            Ok(())
        }
        Role::Supervisor => run_supervisor(),
        Role::SnapshotExporter => run_snapshot_exporter(),
        Role::Relay => run_resident(RELAY_USER),
    }
}

fn run_supervisor() -> Result<(), Box<dyn std::error::Error>> {
    require_effective_user(SUPERVISOR_USER)?;
    let identity = EndpointIdentity {
        chain_id: required_env("OCOMP_CHAIN_ID")?.parse()?,
        genesis_hash: required_env("OCOMP_GENESIS_HASH")?.parse()?,
        boot_nonce: required_env("OCOMP_BOOT_NONCE")?.parse()?,
        protocol_bundle_hash: required_env("OCOMP_PROTOCOL_BUNDLE_HASH")?.parse()?,
    };
    let protocol_bundle = PinnedProtocolBundle::decode(
        &std::fs::read(PROTOCOL_BUNDLE_PATH)?,
        identity.protocol_bundle_hash,
        &poc_schema_limits(),
    )?;
    let discovery = SupervisorDiscovery::open(SupervisorDiscoveryConfig {
        node_socket: PathBuf::from(NODE_CONTROL_SOCKET),
        journal_root: PathBuf::from(SUPERVISOR_JOURNAL_ROOT),
        expected_node_uid: uid_for_user(NODE_USER)?,
        identity,
        limits: poc_schema_limits(),
    })?;
    let adoption = SupervisorExportAdoption::open(SupervisorExportAdoptionConfig {
        cas_root: PathBuf::from(CAS_ROOT),
        cas_limits: CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: CAS_MAX_TOTAL_BYTES,
        },
        input_ref_root: PathBuf::from(SNAPSHOT_EXPORTER_INPUT_REF_ROOT),
        receipt_root: PathBuf::from(SNAPSHOT_EXPORTER_RECEIPT_ROOT),
        binding_root: PathBuf::from(SUPERVISOR_EXPORT_BINDING_ROOT),
        protocol_bundle,
        limits: poc_schema_limits(),
    })?;
    let mut adopted_job_id = None;
    loop {
        if let Err(error) = discovery.reconcile_once() {
            eprintln!("OCOMP supervisor discovery retry: {error}");
        }
        if let Some(record) = discovery.current_record()? {
            let job_id = record.spec.summary.job_id;
            if adopted_job_id != Some(job_id) {
                match adoption.try_adopt(&record) {
                    Ok(SupervisorExportAdoptionOutcome::Adopted(_)) => {
                        adopted_job_id = Some(job_id);
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

fn run_snapshot_exporter() -> Result<(), Box<dyn std::error::Error>> {
    require_effective_user(SNAPSHOT_EXPORTER_USER)?;
    let limits = poc_schema_limits();
    let identity = EndpointIdentity {
        chain_id: required_env("OCOMP_CHAIN_ID")?.parse()?,
        genesis_hash: required_env("OCOMP_GENESIS_HASH")?.parse()?,
        boot_nonce: required_env("OCOMP_BOOT_NONCE")?.parse()?,
        protocol_bundle_hash: required_env("OCOMP_PROTOCOL_BUNDLE_HASH")?.parse()?,
    };
    let protocol_bundle = PinnedProtocolBundle::decode(
        &std::fs::read(PROTOCOL_BUNDLE_PATH)?,
        identity.protocol_bundle_hash,
        &limits,
    )?;
    let mut exporter = SnapshotExporter::open(SnapshotExporterConfig {
        node: SnapshotExporterNodeConfig {
            node_socket: PathBuf::from(NODE_CONTROL_SOCKET),
            expected_node_uid: uid_for_user(NODE_USER)?,
            identity,
            limits,
        },
        ce_datadir: PathBuf::from(SNAPSHOT_EXPORTER_CE_DATADIR),
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
        cas_root: PathBuf::from(CAS_ROOT),
        cas_limits: CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: CAS_MAX_TOTAL_BYTES,
        },
        input_ref_root: PathBuf::from(SNAPSHOT_EXPORTER_INPUT_REF_ROOT),
        receipt_root: PathBuf::from(SNAPSHOT_EXPORTER_RECEIPT_ROOT),
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

fn run_resident(expected_user: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_effective_user(expected_user)?;
    loop {
        std::thread::park();
    }
}

fn required_env(name: &'static str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("required environment variable {name} is missing").into())
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
}
