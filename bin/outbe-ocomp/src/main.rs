use std::env;
use std::path::PathBuf;
use std::time::Duration;

use alloy_primitives::B256;
use clap::{Args, Parser, Subcommand};
use outbe_ocomp::bundle::PinnedProtocolBundle;
use outbe_ocomp::cas::CasLimits;
use outbe_ocomp::control::{
    poc_schema_limits, require_effective_user, uid_for_user, EndpointIdentity,
};
use outbe_ocomp::supervisor::{SupervisorDiscovery, SupervisorDiscoveryConfig};
use outbe_ocomp::worker::{run_one_from_inherited_fd, WorkerConfig};

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
const SOCKET_ACTIVATION_FD: i32 = 0;
const NODE_USER: &str = "outbe";
const NODE_CONTROL_SOCKET: &str = "/run/outbe-ocomp/node.sock";
const SUPERVISOR_JOURNAL_ROOT: &str = "/var/lib/outbe-ocomp/supervisor-v1/discovery";
const SUPERVISOR_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
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
                connection_fd: SOCKET_ACTIVATION_FD,
                protocol_bundle,
            })?;
            Ok(())
        }
        Role::Supervisor => run_supervisor(),
        Role::SnapshotExporter => run_resident(SNAPSHOT_EXPORTER_USER),
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
    let discovery = SupervisorDiscovery::open(SupervisorDiscoveryConfig {
        node_socket: PathBuf::from(NODE_CONTROL_SOCKET),
        journal_root: PathBuf::from(SUPERVISOR_JOURNAL_ROOT),
        expected_node_uid: uid_for_user(NODE_USER)?,
        identity,
        limits: poc_schema_limits(),
    })?;
    loop {
        if let Err(error) = discovery.reconcile_once() {
            eprintln!("OCOMP supervisor discovery retry: {error}");
        }
        std::thread::sleep(SUPERVISOR_RECONCILE_INTERVAL);
    }
}

fn run_resident(expected_user: &str) -> Result<(), Box<dyn std::error::Error>> {
    require_effective_user(expected_user)?;
    loop {
        std::thread::park();
    }
}

fn required_env(name: &'static str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name)
        .map_err(|_| format!("required OCOMP environment variable {name} is missing").into())
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
