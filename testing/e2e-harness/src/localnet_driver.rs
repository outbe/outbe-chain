//! Persistent operator commands for the canonical Rust LocalNet harness.
//!
//! The ordinary `outbe-e2e` entry point owns short-lived Cucumber scenarios.
//! This module exposes the same LocalNet implementation as an operator-owned
//! process for `mise run localnet-*`. The hardware SGX lane deliberately has a
//! command surface but remains fail-closed until its separate implementation.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use eyre::{bail, ensure, eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::env::{EnvCli, Environment, TeeMode};
use crate::internal::{config::Config, eth, ports::Ports};
use crate::world::localnet::Localnet;
#[cfg(feature = "ocomp-integration")]
use crate::world::localnet::StartOpts;
use crate::world::mongodb::MongoDb;
#[cfg(feature = "ocomp-integration")]
use crate::world::ocomp::{
    OcompLaunchIdentityV1, OcompProcessRole, OcompRuntimeCountsV1, OcompTopology,
};

#[cfg(not(feature = "ocomp-integration"))]
struct OcompRuntimeCountsV1 {
    supervisors: usize,
    snapshot_exporters: usize,
    workers: usize,
    registered_workers: usize,
    connected_workers: usize,
}

const STATE_FILE: &str = "localnet-state-v1.json";
const BOOTSTRAP_FILE: &str = "localnet-bootstrap-v1.json";
const DRIVER_LOG: &str = "localnet-driver.log";
const START_LOCK_FILE: &str = ".localnet-start-v1.lock";
const STATE_VERSION: u32 = 1;
const START_TIMEOUT: Duration = Duration::from_secs(180);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalnetLane {
    DevMock,
    RealSgx,
}

impl LocalnetLane {
    fn ensure_implemented(self) -> Result<()> {
        match self {
            Self::DevMock => Ok(()),
            Self::RealSgx => {
                bail!(
                    "real-SGX LocalNet is deferred to outbe-chain-8lp; no mock or GramineDirectDev fallback is permitted"
                );
            }
        }
    }
}

#[derive(Clone, Debug, Parser)]
#[command(name = "outbe-e2e localnet")]
#[command(about = "Bootstrap and operate the persistent Rust-harness LocalNet")]
struct LocalnetCli {
    #[command(subcommand)]
    command: LocalnetCommand,

    /// Repository root.
    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,

    /// Persistent LocalNet data directory.
    #[arg(long, global = true, default_value = "/tmp/outbe-testnet")]
    data_dir: PathBuf,

    /// Number of genesis validators.
    #[arg(long, global = true, default_value_t = 4)]
    validators: usize,

    /// Optional genesis seed; defaults to scripts/seed-testnet-lowstake.json.
    #[arg(long, global = true)]
    seed: Option<PathBuf>,

    /// Stream setup and launch diagnostics.
    #[arg(long, global = true)]
    debug: bool,

    /// Do not invoke privileged Docker operations through sudo.
    #[arg(long, global = true)]
    no_sudo: bool,
}

#[derive(Clone, Debug, Subcommand)]
enum LocalnetCommand {
    /// Create fresh keys and genesis without starting processes.
    Bootstrap,
    /// Start the persistent LocalNet owner and wait until blocks advance.
    Start,
    /// Report owner and per-validator RPC status.
    Status,
    /// Stop the exact LocalNet owner and all resources it owns.
    Stop,
    /// Internal foreground owner used by `start`.
    #[command(hide = true)]
    Serve,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DriverPhase {
    Starting,
    Running,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LocalnetStateV1 {
    version: u32,
    lane: String,
    phase: DriverPhase,
    owner_pid: u32,
    owner_process_identity: String,
    repo: PathBuf,
    data_dir: PathBuf,
    validators: usize,
    port_blocks: Vec<u16>,
    rpc_ports: Vec<u16>,
    heights: Vec<u64>,
    ocomp_supervisors: usize,
    ocomp_snapshot_exporters: usize,
    ocomp_workers: usize,
    ocomp_registered_workers: usize,
    ocomp_connected_workers: usize,
    ocomp_processes: Vec<OwnedOcompProcessV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OwnedOcompProcessV1 {
    validator_index: Option<u8>,
    role: String,
    worker_ordinal: Option<u32>,
    pid: u32,
    process_identity: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StartLockRecordV1 {
    pid: u32,
    process_identity: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LocalnetBootstrapV1 {
    version: u32,
    lane: String,
    repo: PathBuf,
    data_dir: PathBuf,
    validators: usize,
    port_blocks: Vec<u16>,
    rpc_ports: Vec<u16>,
}

/// Parse and execute one operator command.
pub async fn run_from<I, T>(lane: LocalnetLane, args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = LocalnetCli::try_parse_from(args)?;
    lane.ensure_implemented()?;
    ensure!(
        cli.validators > 0,
        "LocalNet requires at least one validator"
    );
    let cli = cli.resolve()?;
    match cli.command {
        LocalnetCommand::Bootstrap => bootstrap(&cli),
        LocalnetCommand::Start => start(&cli),
        LocalnetCommand::Status => status(&cli),
        LocalnetCommand::Stop => stop(&cli),
        LocalnetCommand::Serve => serve(&cli).await,
    }
}

impl LocalnetCli {
    fn resolve(mut self) -> Result<Self> {
        self.repo = resolve_existing_path(&self.repo)?;
        self.data_dir = resolve_maybe_missing_path(&self.data_dir)?;
        if let Some(seed) = &self.seed {
            self.seed = Some(if seed.is_absolute() {
                seed.clone()
            } else {
                self.repo.join(seed)
            });
        }
        validate_data_dir(&self.repo, &self.data_dir)?;
        Ok(self)
    }

    fn environment(&self, persisted_blocks: Option<&[u16]>) -> Result<Environment> {
        let cli = EnvCli {
            validators: self.validators,
            no_resolve_ports: persisted_blocks.is_some(),
            no_cleanup: true,
            tee: TeeMode::Mock,
            no_sudo: self.no_sudo,
            all: false,
            debug: self.debug,
            repo: Some(self.repo.clone()),
            data_dir: Some(self.data_dir.clone()),
            evidence_dir: None,
            metadosis_p0_case: None,
            chain_bin: Some(self.repo.join("target/debug/outbe-chain")),
            ocomp_bin: Some(self.repo.join("target/debug/outbe-ocomp")),
            upgraded_chain_bin: None,
            cli_bin: Some(self.repo.join("target/debug/outbe-cli")),
            keygen_bin: Some(self.repo.join("target/debug/outbe-keygen")),
            enclave_bin: Some(self.repo.join("target/debug/outbe-tee-enclave")),
            mock_bin: Some(self.repo.join("target/debug/outbe-tee-enclave-mock")),
            seed: Some(
                self.seed
                    .clone()
                    .unwrap_or_else(|| self.repo.join("scripts/seed-testnet-lowstake.json")),
            ),
            projection_mongodb_uri: "auto".to_owned(),
        };
        let mut env = Environment::from_cli(&cli);
        if let Some(starts) = persisted_blocks {
            ensure!(
                starts.len() == self.validators,
                "bootstrap port block count differs from validator count"
            );
            env.ports = Ports::from_block_starts(starts)?;
        } else {
            env.ports.start_scenario(self.validators)?;
        }
        Ok(env)
    }

    fn append_process_args(&self, command: &mut Command) {
        command
            .arg("--repo")
            .arg(&self.repo)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--validators")
            .arg(self.validators.to_string());
        if let Some(seed) = &self.seed {
            command.arg("--seed").arg(seed);
        }
        if self.debug {
            command.arg("--debug");
        }
        if self.no_sudo {
            command.arg("--no-sudo");
        }
    }
}

fn bootstrap(cli: &LocalnetCli) -> Result<()> {
    ensure_not_running(&cli.data_dir)?;
    let env = cli.environment(None)?;
    let config = Config::resolve(&env);
    let port_blocks = env.ports.block_starts(cli.validators)?;
    let rpc_ports = rpc_ports(&config);
    let localnet = Localnet::new(config);
    localnet.wipe()?;
    localnet.bootstrap(
        cli.validators,
        &[("TESTNET_EPOCH_LENGTH_BLOCKS", "300".to_owned())],
    )?;
    localnet.bind_tee_genesis()?;
    let receipt = LocalnetBootstrapV1 {
        version: STATE_VERSION,
        lane: "dev_mock".to_owned(),
        repo: cli.repo.clone(),
        data_dir: cli.data_dir.clone(),
        validators: cli.validators,
        port_blocks,
        rpc_ports,
    };
    write_json_atomic(&bootstrap_path(&cli.data_dir), &receipt)?;
    println!("LocalNet bootstrapped at {}", cli.data_dir.display());
    Ok(())
}

fn start(cli: &LocalnetCli) -> Result<()> {
    require_ocomp_integration_build()?;
    let receipt = read_bootstrap(cli)?;
    let _start_lock = StartLock::acquire(&cli.data_dir)?;
    if let Some(state) = read_state(&cli.data_dir)? {
        if state_is_live(&state) {
            ensure_matches(cli, &state)?;
            if state.phase == DriverPhase::Running {
                status(cli)?;
                return Ok(());
            }
            return wait_for_existing_start(cli, &receipt, state.owner_pid);
        }
        cleanup_stale_owner(cli, &state)?;
        cleanup_run_scoped(cli)?;
        remove_state(&cli.data_dir)?;
    }

    let executable = std::env::current_exe().wrap_err("resolve outbe-e2e executable")?;
    fs::create_dir_all(&cli.data_dir)?;
    let log_path = cli.data_dir.join(DRIVER_LOG);
    let stdout = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(executable);
    command.args(["localnet", "serve"]);
    cli.append_process_args(&mut command);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .wrap_err("spawn persistent LocalNet owner")?;

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(exit) = child.try_wait()? {
            bail!(
                "LocalNet owner exited during startup ({exit}); see {}",
                log_path.display()
            );
        }
        if let Some(state) = read_state(&cli.data_dir)? {
            ensure_matches(cli, &state)?;
            if state.phase == DriverPhase::Running && state_is_live(&state) {
                ensure!(
                    state.rpc_ports == receipt.rpc_ports,
                    "running LocalNet RPC layout differs from bootstrap"
                );
                ensure!(
                    state.port_blocks == receipt.port_blocks,
                    "running LocalNet service-port layout differs from bootstrap"
                );
                verify_running_state(cli, &state)?;
                println!(
                    "LocalNet running (pid {}, heights {:?}, OCOMP supervisors {}, workers {}/{})",
                    state.owner_pid,
                    state.heights,
                    state.ocomp_supervisors,
                    state.ocomp_connected_workers,
                    state.ocomp_workers
                );
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            terminate_startup_owner(cli, &mut child)?;
            bail!(
                "LocalNet did not become ready within {} seconds; see {}",
                START_TIMEOUT.as_secs(),
                log_path.display()
            );
        }
        sleep(Duration::from_millis(250));
    }
}

fn status(cli: &LocalnetCli) -> Result<()> {
    require_ocomp_integration_build()?;
    let state = read_state(&cli.data_dir)?
        .ok_or_else(|| eyre!("LocalNet is not running at {}", cli.data_dir.display()))?;
    ensure_matches(cli, &state)?;
    ensure!(
        state_is_live(&state),
        "LocalNet state is stale (former pid {})",
        state.owner_pid
    );
    let (heights, counts) = verify_running_state(cli, &state)?;
    println!(
        "LocalNet {:?} (pid {}): RPC ports {:?}, heights {:?}; OCOMP supervisors {}, snapshot exporters {}, workers {}, registered {}, connected {}",
        state.phase,
        state.owner_pid,
        state.rpc_ports,
        heights,
        counts.supervisors,
        counts.snapshot_exporters,
        counts.workers,
        counts.registered_workers,
        counts.connected_workers
    );
    Ok(())
}

fn verify_running_state(
    cli: &LocalnetCli,
    state: &LocalnetStateV1,
) -> Result<(Vec<u64>, OcompRuntimeCountsV1)> {
    ensure!(
        state.phase == DriverPhase::Running,
        "LocalNet owner has not published running readiness"
    );
    ensure_exact_ocomp_processes_live(state)?;
    let heights = observe_advancing_heights(&state.rpc_ports, Duration::from_secs(5))?;
    let counts = observe_ocomp_runtime(cli, &state.port_blocks)?;
    ensure!(
        counts.supervisors == state.ocomp_supervisors
            && counts.snapshot_exporters == state.ocomp_snapshot_exporters
            && counts.workers == state.ocomp_workers
            && counts.registered_workers == state.ocomp_registered_workers
            && counts.connected_workers == state.ocomp_connected_workers,
        "LocalNet OCOMP runtime differs from the owner readiness record"
    );
    Ok((heights, counts))
}

fn observe_advancing_heights(ports: &[u16], timeout: Duration) -> Result<Vec<u64>> {
    let baseline = observe_heights(ports)
        .ok_or_else(|| eyre!("LocalNet owner is live but one or more RPCs are unavailable"))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(heights) = observe_heights(ports) {
            if heights
                .iter()
                .zip(&baseline)
                .all(|(height, first)| height > first)
            {
                return Ok(heights);
            }
        }
        ensure!(
            Instant::now() < deadline,
            "LocalNet RPC heads did not all advance within {} seconds",
            timeout.as_secs()
        );
        sleep(Duration::from_millis(250));
    }
}

fn ensure_exact_ocomp_processes_live(state: &LocalnetStateV1) -> Result<()> {
    let counts = OcompRuntimeCountsV1 {
        supervisors: state.ocomp_supervisors,
        snapshot_exporters: state.ocomp_snapshot_exporters,
        workers: state.ocomp_workers,
        registered_workers: state.ocomp_registered_workers,
        connected_workers: state.ocomp_connected_workers,
    };
    ensure_ocomp_process_inventory(&state.ocomp_processes, counts)?;
    for process in &state.ocomp_processes {
        ensure!(
            process_identity(process.pid).as_deref() == Some(&process.process_identity),
            "validator-{:?} OCOMP {} pid {} is missing or was reused",
            process.validator_index,
            process.role,
            process.pid
        );
    }
    Ok(())
}

fn ensure_ocomp_process_inventory(
    processes: &[OwnedOcompProcessV1],
    counts: OcompRuntimeCountsV1,
) -> Result<()> {
    let mut pids = BTreeSet::new();
    let mut supervisors = 0usize;
    let mut snapshot_exporters = 0usize;
    let mut workers = 0usize;
    for process in processes {
        ensure!(
            pids.insert(process.pid),
            "duplicate OCOMP pid {}",
            process.pid
        );
        match process.role.as_str() {
            "supervisor" => supervisors += 1,
            "snapshot_exporter" => snapshot_exporters += 1,
            "worker" => workers += 1,
            role => {
                bail!("unexpected persistent LocalNet OCOMP role {role}");
            }
        }
    }
    ensure!(
        supervisors == 0
            && counts.supervisors == counts.snapshot_exporters
            && snapshot_exporters == counts.snapshot_exporters
            && workers == counts.workers,
        "exact OCOMP process inventory differs from readiness counts"
    );
    Ok(())
}

fn wait_for_existing_start(
    cli: &LocalnetCli,
    receipt: &LocalnetBootstrapV1,
    owner_pid: u32,
) -> Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let state = read_state(&cli.data_dir)?
            .ok_or_else(|| eyre!("LocalNet owner pid {owner_pid} removed its startup state"))?;
        ensure_matches(cli, &state)?;
        ensure!(
            state.owner_pid == owner_pid && state_is_live(&state),
            "LocalNet owner pid {owner_pid} exited during startup"
        );
        if state.phase == DriverPhase::Running {
            ensure!(
                state.rpc_ports == receipt.rpc_ports,
                "running RPC layout differs"
            );
            ensure!(
                state.port_blocks == receipt.port_blocks,
                "running service-port layout differs"
            );
            verify_running_state(cli, &state)?;
            println!("LocalNet running (pid {})", state.owner_pid);
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "existing LocalNet owner pid {owner_pid} did not become ready within {} seconds",
            START_TIMEOUT.as_secs()
        );
        sleep(Duration::from_millis(250));
    }
}

fn stop(cli: &LocalnetCli) -> Result<()> {
    let Some(state) = read_state(&cli.data_dir)? else {
        println!("LocalNet is already stopped");
        return Ok(());
    };
    ensure_matches(cli, &state)?;
    if !state_is_live(&state) {
        cleanup_stale_owner(cli, &state)?;
        cleanup_run_scoped(cli)?;
        remove_state(&cli.data_dir)?;
        println!("Removed stale LocalNet resources and owner record");
        return Ok(());
    }
    signal_owner(state.owner_pid)?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !state_is_live(&state) {
            remove_state(&cli.data_dir)?;
            println!("LocalNet stopped");
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }
    force_stop_live_owner(cli, &state)?;
    remove_state(&cli.data_dir)?;
    println!(
        "LocalNet owner pid {} required forced process-group cleanup",
        state.owner_pid
    );
    Ok(())
}

fn terminate_startup_owner(cli: &LocalnetCli, child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    signal_owner(child.id())?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }

    signal_process_group(child.id(), "KILL")?;
    let _ = child.wait();
    cleanup_run_scoped(cli).wrap_err("clean resources after forced LocalNet owner stop")
}

fn signal_owner(pid: u32) -> Result<()> {
    signal_pid(pid, "TERM")
}

fn signal_pid(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .wrap_err("signal LocalNet owner")?;
    ensure!(status.success(), "failed to signal LocalNet owner");
    Ok(())
}

fn signal_process_group(group: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args([format!("-{signal}"), "--".to_owned(), format!("-{group}")])
        .status()
        .wrap_err("signal LocalNet owner process group")?;
    ensure!(
        status.success(),
        "failed to signal LocalNet process group {group}"
    );
    Ok(())
}

fn cleanup_stale_owner(_cli: &LocalnetCli, state: &LocalnetStateV1) -> Result<()> {
    for process in &state.ocomp_processes {
        if process_identity(process.pid).as_deref() == Some(&process.process_identity) {
            signal_pid(process.pid, "TERM")?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline
        && state.ocomp_processes.iter().any(|process| {
            process_identity(process.pid).as_deref() == Some(&process.process_identity)
        })
    {
        sleep(Duration::from_millis(100));
    }
    for process in &state.ocomp_processes {
        if process_identity(process.pid).as_deref() == Some(&process.process_identity) {
            signal_pid(process.pid, "KILL")?;
        }
    }
    Ok(())
}

fn force_stop_live_owner(cli: &LocalnetCli, state: &LocalnetStateV1) -> Result<()> {
    ensure!(state_is_live(state), "refuse to kill a reused owner pid");
    signal_process_group(state.owner_pid, "KILL")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && state_is_live(state) {
        sleep(Duration::from_millis(100));
    }
    ensure!(
        !state_is_live(state),
        "LocalNet owner survived forced process-group cleanup"
    );
    cleanup_run_scoped(cli)
}

async fn serve(cli: &LocalnetCli) -> Result<()> {
    require_ocomp_integration_build()?;
    serve_with_ocomp(cli).await
}

#[cfg(feature = "ocomp-integration")]
async fn serve_with_ocomp(cli: &LocalnetCli) -> Result<()> {
    let receipt = read_bootstrap(cli)?;
    ensure_not_running(&cli.data_dir)?;
    let env = cli.environment(Some(&receipt.port_blocks))?;
    env.ports.ensure_available(cli.validators)?;
    let mut config = Config::resolve(&env);
    let rpc_ports = rpc_ports(&config);
    ensure!(
        rpc_ports == receipt.rpc_ports,
        "serve RPC layout differs from bootstrap"
    );
    let _mongo = MongoDb::connect_or_start(&mut config)?;
    let mut localnet = Localnet::new(config.clone());
    let mut ocomp = OcompTopology::new(config);
    let ocomp_identity = ocomp.prepare_bootstrapped_runtime()?;
    let owner_pid = std::process::id();
    let owner_process_identity = process_identity(owner_pid)
        .ok_or_else(|| eyre!("cannot establish LocalNet owner process identity"))?;
    let mut state = LocalnetStateV1 {
        version: STATE_VERSION,
        lane: "dev_mock".to_owned(),
        phase: DriverPhase::Starting,
        owner_pid,
        owner_process_identity,
        repo: cli.repo.clone(),
        data_dir: cli.data_dir.clone(),
        validators: cli.validators,
        port_blocks: receipt.port_blocks,
        rpc_ports,
        heights: vec![0; cli.validators],
        ocomp_supervisors: 0,
        ocomp_snapshot_exporters: 0,
        ocomp_workers: 0,
        ocomp_registered_workers: 0,
        ocomp_connected_workers: 0,
        ocomp_processes: Vec::new(),
    };
    write_json_atomic(&state_path(&cli.data_dir), &state)?;
    let _state_guard = StateGuard(state_path(&cli.data_dir));

    localnet.start(&StartOpts::default())?;
    state.heights =
        wait_for_advancing_heights(&mut localnet, &state.rpc_ports, START_TIMEOUT).await?;
    verify_rpc_chain_identity(&state.rpc_ports, ocomp_identity)?;
    ocomp.start_baseline_runtime(ocomp_identity)?;
    let counts = wait_for_ocomp_runtime(&mut ocomp, START_TIMEOUT).await?;
    apply_ocomp_counts(&mut state, counts);
    state.ocomp_processes = snapshot_ocomp_processes(&ocomp, counts)?;
    state.phase = DriverPhase::Running;
    write_json_atomic(&state_path(&cli.data_dir), &state)?;

    monitor_until_shutdown(&mut localnet, &mut ocomp).await?;
    drop(ocomp);
    localnet.stop()?;
    Ok(())
}

#[cfg(not(feature = "ocomp-integration"))]
async fn serve_with_ocomp(_cli: &LocalnetCli) -> Result<()> {
    unreachable!("require_ocomp_integration_build rejects this build")
}

#[cfg(feature = "ocomp-integration")]
fn apply_ocomp_counts(state: &mut LocalnetStateV1, counts: OcompRuntimeCountsV1) {
    state.ocomp_supervisors = counts.supervisors;
    state.ocomp_snapshot_exporters = counts.snapshot_exporters;
    state.ocomp_workers = counts.workers;
    state.ocomp_registered_workers = counts.registered_workers;
    state.ocomp_connected_workers = counts.connected_workers;
}

#[cfg(feature = "ocomp-integration")]
fn snapshot_ocomp_processes(
    ocomp: &OcompTopology,
    counts: OcompRuntimeCountsV1,
) -> Result<Vec<OwnedOcompProcessV1>> {
    let mut processes = Vec::new();
    for record in ocomp
        .process_records()
        .iter()
        .filter(|record| record.stopped_at_millis.is_none())
    {
        let role = match record.role {
            OcompProcessRole::Supervisor => "supervisor",
            OcompProcessRole::SnapshotExporter => "snapshot_exporter",
            OcompProcessRole::Worker => "worker",
            OcompProcessRole::Follower => {
                bail!("baseline LocalNet unexpectedly owns an OCOMP follower process");
            }
        };
        let process_identity = process_identity(record.pid).ok_or_else(|| {
            eyre!(
                "OCOMP {role} pid {} exited before its exact identity was recorded",
                record.pid
            )
        })?;
        processes.push(OwnedOcompProcessV1 {
            validator_index: record.validator_index,
            role: role.to_owned(),
            worker_ordinal: record.worker_ordinal,
            pid: record.pid,
            process_identity,
        });
    }
    ensure_ocomp_process_inventory(&processes, counts)?;
    Ok(processes)
}

#[cfg(feature = "ocomp-integration")]
fn verify_rpc_chain_identity(ports: &[u16], identity: OcompLaunchIdentityV1) -> Result<()> {
    let expected_genesis = format!("{:#x}", identity.genesis_hash);
    for port in ports {
        let url = format!("http://127.0.0.1:{port}");
        let chain_id = eth::raw_json(&url, "eth_chainId")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("RPC {url} did not report a valid chain ID"))?;
        ensure!(
            chain_id == identity.chain_id,
            "RPC {url} chain ID {chain_id} differs from OCOMP identity {}",
            identity.chain_id
        );
        let genesis = eth::block_hash(&url, 0)
            .ok_or_else(|| eyre!("RPC {url} did not return genesis block 0"))?;
        ensure!(
            genesis.eq_ignore_ascii_case(&expected_genesis),
            "RPC {url} genesis {genesis} differs from OCOMP identity {expected_genesis}"
        );
    }
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
async fn wait_for_ocomp_runtime(
    ocomp: &mut OcompTopology,
    timeout: Duration,
) -> Result<OcompRuntimeCountsV1> {
    let deadline = Instant::now() + timeout;
    loop {
        ocomp.ensure_baseline_processes_alive(1)?;
        match ocomp.observe_baseline_runtime(1) {
            Ok(counts) => return Ok(counts),
            Err(error) if Instant::now() >= deadline => {
                bail!(
                    "OCOMP Supervisors/Workers did not become ready within {} seconds: {error}",
                    timeout.as_secs()
                );
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(feature = "ocomp-integration")]
async fn monitor_until_shutdown(localnet: &mut Localnet, ocomp: &mut OcompTopology) -> Result<()> {
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            () = &mut shutdown => return Ok(()),
            _ = interval.tick() => {
                localnet.ensure_committee_alive()?;
                ocomp.ensure_baseline_runtime_ready(1)?;
            }
        }
    }
}

#[cfg(feature = "ocomp-integration")]
async fn wait_for_advancing_heights(
    localnet: &mut Localnet,
    ports: &[u16],
    timeout: Duration,
) -> Result<Vec<u64>> {
    let deadline = Instant::now() + timeout;
    let mut baseline = None;
    loop {
        localnet.ensure_committee_alive()?;
        if let Some(heights) = observe_heights(ports) {
            if heights.iter().all(|height| *height > 0) {
                match &baseline {
                    Some(first)
                        if heights
                            .iter()
                            .zip(first)
                            .all(|(height, first)| height > first) =>
                    {
                        return Ok(heights);
                    }
                    None => baseline = Some(heights),
                    Some(_) => {}
                }
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "all validator RPCs did not advance beyond genesis within {} seconds",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn cleanup_run_scoped(cli: &LocalnetCli) -> Result<()> {
    let receipt = read_bootstrap(cli)?;
    let env = cli.environment(Some(&receipt.port_blocks))?;
    let config = Config::resolve(&env);
    let mut localnet = Localnet::new(config);
    localnet.teardown();
    MongoDb::teardown_managed_for_run(&env);
    Ok(())
}

fn observe_heights(ports: &[u16]) -> Option<Vec<u64>> {
    ports
        .iter()
        .map(|port| eth::block_number(&format!("http://127.0.0.1:{port}")))
        .collect()
}

fn require_ocomp_integration_build() -> Result<()> {
    #[cfg(feature = "ocomp-integration")]
    {
        Ok(())
    }
    #[cfg(not(feature = "ocomp-integration"))]
    {
        bail!(
            "persistent LocalNet requires outbe-e2e built with --features ocomp-integration so OCOMP Supervisors and Workers are mandatory"
        );
    }
}

#[cfg(feature = "ocomp-integration")]
fn observe_ocomp_runtime(cli: &LocalnetCli, port_blocks: &[u16]) -> Result<OcompRuntimeCountsV1> {
    let env = cli.environment(Some(port_blocks))?;
    OcompTopology::new(Config::resolve(&env)).observe_baseline_runtime(1)
}

#[cfg(not(feature = "ocomp-integration"))]
fn observe_ocomp_runtime(_cli: &LocalnetCli, _port_blocks: &[u16]) -> Result<OcompRuntimeCountsV1> {
    unreachable!("require_ocomp_integration_build rejects this build")
}

#[cfg(feature = "ocomp-integration")]
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn read_bootstrap(cli: &LocalnetCli) -> Result<LocalnetBootstrapV1> {
    let path = bootstrap_path(&cli.data_dir);
    let receipt: LocalnetBootstrapV1 = read_json(&path)
        .wrap_err_with(|| format!("LocalNet is not bootstrapped at {}", cli.data_dir.display()))?;
    ensure!(
        receipt.version == STATE_VERSION,
        "unsupported bootstrap version"
    );
    ensure!(
        receipt.lane == "dev_mock",
        "bootstrap is not the dev/mock lane"
    );
    ensure!(receipt.repo == cli.repo, "bootstrap repository differs");
    ensure!(
        receipt.data_dir == cli.data_dir,
        "bootstrap data directory differs"
    );
    ensure!(
        receipt.validators == cli.validators,
        "bootstrap validator count differs"
    );
    let env = cli.environment(Some(&receipt.port_blocks))?;
    let config = Config::resolve(&env);
    ensure!(
        rpc_ports(&config) == receipt.rpc_ports,
        "bootstrap RPC projection differs from persisted service-port layout"
    );
    Ok(receipt)
}

fn ensure_not_running(data_dir: &Path) -> Result<()> {
    if let Some(state) = read_state(data_dir)? {
        ensure!(
            !state_is_live(&state),
            "LocalNet is already running (pid {})",
            state.owner_pid
        );
        remove_state(data_dir)?;
    }
    Ok(())
}

fn ensure_matches(cli: &LocalnetCli, state: &LocalnetStateV1) -> Result<()> {
    ensure!(state.version == STATE_VERSION, "unsupported state version");
    ensure!(state.lane == "dev_mock", "state is not the dev/mock lane");
    ensure!(state.repo == cli.repo, "state repository differs");
    ensure!(
        state.data_dir == cli.data_dir,
        "state data directory differs"
    );
    ensure!(
        state.validators == cli.validators,
        "state validator count differs"
    );
    Ok(())
}

fn state_is_live(state: &LocalnetStateV1) -> bool {
    process_identity(state.owner_pid).as_deref() == Some(&state.owner_process_identity)
}

fn process_identity(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = stat.rsplit_once(')')?.1.trim();
        let start_ticks = after_name.split_whitespace().nth(19)?;
        let executable = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        Some(format!("linux:{start_ticks}:{}", executable.display()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart=", "-o", "command="])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| format!("ps:{}", String::from_utf8_lossy(&output.stdout).trim()))
    }
}

fn rpc_ports(config: &Config) -> Vec<u16> {
    (0..config.validators)
        .map(|index| config.http_port(index))
        .collect()
}

fn read_state(data_dir: &Path) -> Result<Option<LocalnetStateV1>> {
    let path = state_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
}

fn remove_state(data_dir: &Path) -> Result<()> {
    match fs::remove_file(state_path(data_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path)?).map_err(Into::into)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut file = File::create(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE)
}

fn bootstrap_path(data_dir: &Path) -> PathBuf {
    data_dir.join(BOOTSTRAP_FILE)
}

fn resolve_existing_path(path: &Path) -> Result<PathBuf> {
    let absolute = lexical_absolute(path)?;
    fs::canonicalize(&absolute)
        .wrap_err_with(|| format!("resolve existing path {}", absolute.display()))
}

fn resolve_maybe_missing_path(path: &Path) -> Result<PathBuf> {
    let normalized = lexical_absolute(path)?;
    let mut existing = normalized.as_path();
    let mut tail = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| eyre!("path has no existing ancestor: {}", normalized.display()))?;
        tail.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| eyre!("path has no parent: {}", normalized.display()))?;
    }
    let mut resolved = fs::canonicalize(existing)?;
    for component in tail.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(
                    normalized.pop(),
                    "path escapes the filesystem root: {}",
                    absolute.display()
                );
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn validate_data_dir(repo: &Path, data_dir: &Path) -> Result<()> {
    ensure!(
        data_dir.is_absolute(),
        "LocalNet data directory must be absolute"
    );
    ensure!(
        data_dir != Path::new("/"),
        "refuse root as LocalNet data directory"
    );
    ensure!(
        data_dir != repo,
        "refuse repository root as LocalNet data directory"
    );
    ensure!(
        !repo.starts_with(data_dir),
        "refuse a LocalNet data directory that owns the repository"
    );
    ensure!(
        data_dir.file_name().is_some(),
        "LocalNet data directory has no name"
    );
    Ok(())
}

struct StartLock {
    path: PathBuf,
    record: StartLockRecordV1,
}

impl StartLock {
    fn acquire(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join(START_LOCK_FILE);
        let pid = std::process::id();
        let record = StartLockRecordV1 {
            pid,
            process_identity: process_identity(pid)
                .ok_or_else(|| eyre!("cannot establish localnet-start process identity"))?,
        };
        for _ in 0..2 {
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(&serde_json::to_vec(&record)?)?;
                    file.write_all(b"\n")?;
                    file.sync_all()?;
                    return Ok(Self { path, record });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing: StartLockRecordV1 =
                        read_json(&path).wrap_err("read existing LocalNet start lock")?;
                    if process_identity(existing.pid).as_deref() == Some(&existing.process_identity)
                    {
                        bail!(
                            "LocalNet start is already in progress (pid {})",
                            existing.pid
                        );
                    }
                    fs::remove_file(&path).wrap_err("remove stale LocalNet start lock")?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not acquire LocalNet start lock");
    }
}

impl Drop for StartLock {
    fn drop(&mut self) {
        let still_ours =
            read_json::<StartLockRecordV1>(&self.path).is_ok_and(|record| record == self.record);
        if still_ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(feature = "ocomp-integration")]
struct StateGuard(PathBuf);

#[cfg(feature = "ocomp-integration")]
impl Drop for StateGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_operator_commands_parse_for_the_dev_lane() {
        for command in ["bootstrap", "start", "status", "stop"] {
            let cli = LocalnetCli::try_parse_from([
                "outbe-e2e localnet",
                command,
                "--data-dir",
                "/tmp/localnet-a",
                "--validators",
                "5",
            ])
            .unwrap();
            assert_eq!(cli.data_dir, PathBuf::from("/tmp/localnet-a"));
            assert_eq!(cli.validators, 5);
        }
    }

    #[tokio::test]
    async fn real_sgx_command_is_explicit_and_fail_closed() {
        let error = run_from(
            LocalnetLane::RealSgx,
            ["outbe-e2e localnet-sgx", "bootstrap"],
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("outbe-chain-8lp"));
        assert!(message.contains("no mock or GramineDirectDev fallback"));
    }

    #[test]
    fn state_is_atomic_and_bound_to_the_exact_owner_process() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let identity = process_identity(pid).expect("current process identity");
        let state = LocalnetStateV1 {
            version: STATE_VERSION,
            lane: "dev_mock".to_owned(),
            phase: DriverPhase::Running,
            owner_pid: pid,
            owner_process_identity: identity,
            repo: PathBuf::from("/repo"),
            data_dir: dir.path().to_path_buf(),
            validators: 4,
            port_blocks: vec![18545, 18558, 18571, 18584],
            rpc_ports: vec![18545, 18558, 18571, 18584],
            heights: vec![7, 7, 7, 7],
            ocomp_supervisors: 4,
            ocomp_snapshot_exporters: 4,
            ocomp_workers: 4,
            ocomp_registered_workers: 4,
            ocomp_connected_workers: 4,
            ocomp_processes: Vec::new(),
        };
        write_json_atomic(&state_path(dir.path()), &state).unwrap();
        let decoded = read_state(dir.path()).unwrap().unwrap();
        assert_eq!(decoded, state);
        assert!(state_is_live(&decoded));

        let mut stale = decoded;
        stale.owner_process_identity.push_str("-stale");
        assert!(!state_is_live(&stale));
    }

    #[test]
    fn destructive_bootstrap_targets_reject_broad_paths() {
        assert!(validate_data_dir(Path::new("/repo"), Path::new("/")).is_err());
        assert!(validate_data_dir(Path::new("/repo"), Path::new("/repo")).is_err());
        assert!(validate_data_dir(Path::new("/repo/project"), Path::new("/repo")).is_err());
        validate_data_dir(Path::new("/repo"), Path::new("/tmp/outbe-testnet")).unwrap();
    }

    #[test]
    fn destructive_targets_are_resolved_before_validation() {
        let repo = tempfile::tempdir().unwrap();
        let resolved_repo = fs::canonicalize(repo.path()).unwrap();

        let root_alias = resolve_maybe_missing_path(Path::new("/tmp/..")).unwrap();
        assert_eq!(root_alias, Path::new("/"));
        assert!(validate_data_dir(&resolved_repo, &root_alias).is_err());

        let repo_alias = repo.path().join("target/..");
        let resolved_alias = resolve_maybe_missing_path(&repo_alias).unwrap();
        assert_eq!(resolved_alias, resolved_repo);
        assert!(validate_data_dir(&resolved_repo, &resolved_alias).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_destructive_target_is_resolved_before_validation() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let repo = parent.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let alias = parent.path().join("alias");
        symlink(&repo, &alias).unwrap();

        let resolved_repo = resolve_existing_path(&repo).unwrap();
        let resolved_alias = resolve_maybe_missing_path(&alias).unwrap();
        assert_eq!(resolved_alias, resolved_repo);
        assert!(validate_data_dir(&resolved_repo, &resolved_alias).is_err());
    }
}
