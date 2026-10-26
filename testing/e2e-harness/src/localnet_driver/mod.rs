//! Persistent operator commands for the canonical Rust LocalNet harness.
//!
//! The ordinary `outbe-e2e` entry point owns short-lived Cucumber scenarios.
//! This module exposes the same LocalNet implementation as an operator-owned
//! process for `mise run localnet-*`. The hardware SGX lane deliberately has a
//! command surface but remains fail-closed until its separate implementation.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use eyre::{bail, ensure, eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::env::{EnvCli, Environment, TeeMode};
use crate::internal::{config::Config, eth, ports::Ports, proc};
use crate::world::localnet::Localnet;

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

    /// Optional genesis seed; defaults to testing/fixtures/seed-testnet-lowstake.json.
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
            tee: localnet_tee_mode(),
            no_sudo: self.no_sudo,
            force_sudo: false,
            all: false,
            debug: self.debug,
            repo: Some(self.repo.clone()),
            data_dir: Some(self.data_dir.clone()),
            evidence_dir: None,
            artifact_manifest: None,
            scenario_timeout_secs: None,
            chain_bin: Some(self.repo.join("target/release/outbe-chain")),
            feeder_bin: Some(self.repo.join("target/release/outbe-feeder")),
            upgraded_chain_bin: None,
            cli_bin: Some(self.repo.join("target/release/outbe-cli")),
            keygen_bin: Some(self.repo.join("target/release/outbe-keygen")),
            enclave_bin: Some(self.repo.join("target/release/outbe-tee-enclave")),
            mock_bin: Some(self.repo.join("target/release/outbe-tee-enclave-mock")),
            seed: Some(
                self.seed
                    .clone()
                    .unwrap_or_else(|| self.repo.join("testing/fixtures/seed-testnet-lowstake.json")),
            ),
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
    println!(
        "LocalNet bootstrapped at {} ({})",
        cli.data_dir.display(),
        enclave_profile_banner()
    );
    Ok(())
}

fn start(cli: &LocalnetCli) -> Result<()> {
    println!("LocalNet enclave profile: {}", enclave_profile_banner());
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
                verify_running_state(&state)?;
                println!(
                    "LocalNet running (pid {}, heights {:?})",
                    state.owner_pid, state.heights
                );
                print_funded_accounts(cli, &state.rpc_ports);
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
    let state = read_state(&cli.data_dir)?
        .ok_or_else(|| eyre!("LocalNet is not running at {}", cli.data_dir.display()))?;
    ensure_matches(cli, &state)?;
    ensure!(
        state_is_live(&state),
        "LocalNet state is stale (former pid {})",
        state.owner_pid
    );
    let heights = verify_running_state(&state)?;
    println!(
        "LocalNet {:?} (pid {}): RPC ports {:?}, heights {:?}",
        state.phase, state.owner_pid, state.rpc_ports, heights
    );
    Ok(())
}

fn verify_running_state(state: &LocalnetStateV1) -> Result<Vec<u64>> {
    ensure!(
        state.phase == DriverPhase::Running,
        "LocalNet owner has not published running readiness"
    );
    observe_advancing_heights(&state.rpc_ports, Duration::from_secs(5))
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
            verify_running_state(&state)?;
            println!("LocalNet running (pid {})", state.owner_pid);
            print_funded_accounts(cli, &state.rpc_ports);
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

fn observe_heights(ports: &[u16]) -> Option<Vec<u64>> {
    ports
        .iter()
        .map(|port| eth::block_number(&format!("http://127.0.0.1:{port}")))
        .collect()
}

#[cfg(test)]
mod tests;

mod serve;
mod state;
mod stop;
use serve::serve;

use state::state_path;

use state::StateGuard;
use state::{
    bootstrap_path, enclave_profile_banner, ensure_matches, ensure_not_running, localnet_tee_mode,
    print_funded_accounts, process_identity, read_bootstrap, read_state, remove_state,
    resolve_existing_path, resolve_maybe_missing_path, rpc_ports, state_is_live, validate_data_dir,
    write_json_atomic, StartLock,
};
use stop::{cleanup_run_scoped, stop, terminate_startup_owner};
