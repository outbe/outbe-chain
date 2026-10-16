//! Localnet: the whole network in one handle - bootstrap plus every owned node
//! (committee validators, joiner, followers) and their enclaves.
//!
//! A localnet *is* its set of nodes. Adding/removing a validator, attaching a
//! joiner, or launching a follower are all node operations on this one handle
//! rather than a separate object. Every launched process is **owned** via the
//! guards in [`crate::internal::proc`] (nodes killed on drop, enclave containers
//! `docker rm -f`ed on drop). A dropped `World` tears everything down, with a
//! stateless datadir/run-tag sweep as the SIGINT backstop. The distinct
//! lifecycles live in submodules over this one struct:
//!
//! - [`bootstrap`] - genesis/key generation glue (`dkg bootstrap` + `seed_genesis.py`).
//! - [`committee`] - the bootstrapped validator set (start/stop/restart/kill).
//! - [`joiner`] - a validator that joins a running localnet (index = committee size).
//! - [`follower`] - full-execution follower nodes (`--upstream`).
//! - [`log_audit`] - runtime-log normalization, policy, and evidence.
//! - [`probes`] - datadir and timing observations.

mod bootstrap;
mod committee;
mod follower;
mod joiner;
mod log_audit;
mod probes;
mod radicle;
mod teardown;

pub use bootstrap::BootstrapProfile;
pub(crate) use log_audit::{ExpectedTeeFailures, LogAudit};

pub(crate) use radicle::RadicleRepositoryFixtureV1;
#[cfg(test)]
use teardown::sealed_dirs;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{bail, Result, WrapErr};

use crate::internal::config::Config;
use crate::internal::launch_log::LaunchLog;
use crate::internal::proc::{
    self, args, redact_args_for_log, ChildGuard, DockerImageId, EnclaveGuard,
};
use crate::internal::shell::Sh;

/// Per-node execution cache for validators co-located by the devnet harness.
/// The upstream 4 GiB default is a single-node deployment default. Applying it
/// to every local validator would consume the process budget before native execution begins.
const CO_LOCATED_DEVNET_CROSS_BLOCK_CACHE_MIB: u64 = 512;

/// Test-provided knobs for a localnet start. The **enclave mode** is NOT here.
/// It is an environment decision read from [`Config::tee_mode`]. Only per-scenario
/// parameters live on this struct.
#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    /// Expected governance voting window in the already materialized genesis.
    /// Startup rejects a mismatch. This option never changes chain parameters.
    pub voting_window: Option<u64>,
    /// Signed wall-clock offset used only by debug-node day-boundary E2E.
    pub unix_time_offset_secs: Option<i64>,
    /// The scenario already shifted `genesis.json` before deriving another
    /// immutable manifest binding from it. Nodes still receive the clock
    /// offset, but the common start path must not shift genesis a second time.
    pub genesis_timestamp_pre_shifted: bool,
    /// Short-lived pool policy used only by the dedicated eviction scenario.
    pub is_txpool_eviction_profile: bool,
}

impl StartOpts {
    /// A start expecting the given genesis governance voting window.
    pub fn with_voting_window(window: u64) -> Self {
        Self {
            voting_window: Some(window),
            unix_time_offset_secs: None,
            genesis_timestamp_pre_shifted: false,
            is_txpool_eviction_profile: false,
        }
    }

    pub fn with_txpool_eviction_profile(window: u64) -> Self {
        Self {
            voting_window: Some(window),
            is_txpool_eviction_profile: true,
            ..Self::default()
        }
    }

    pub fn near_next_utc_day(window: u64, now_secs: u64) -> Self {
        const BOUNDARY_LEAD_SECS: u64 = 120;
        Self::near_next_utc_day_with_lead(window, now_secs, BOUNDARY_LEAD_SECS)
    }

    /// Position the debug-node clock an exact number of seconds before the next
    /// UTC day boundary. Capacity scenarios need a wider lead than the ordinary
    /// one-item lifecycle because every input must first be finalized through
    /// the public transaction path.
    pub fn near_next_utc_day_with_lead(
        window: u64,
        now_secs: u64,
        boundary_lead_secs: u64,
    ) -> Self {
        use outbe_primitives::time::SECONDS_PER_DAY;
        let next_day = now_secs - (now_secs % SECONDS_PER_DAY) + SECONDS_PER_DAY;
        let target = next_day.saturating_sub(boundary_lead_secs);
        Self {
            voting_window: Some(window),
            unix_time_offset_secs: Some(target as i64 - now_secs as i64),
            genesis_timestamp_pre_shifted: false,
            is_txpool_eviction_profile: false,
        }
    }
}

/// Facts captured from the configured command and this owned launch.
#[derive(Clone, Debug, Eq, PartialEq)]

pub(crate) struct NodeLaunchObservation {
    pub(crate) index: usize,
    pub(crate) pid: u32,
    pub(crate) program: PathBuf,
    pub(crate) argv: Vec<String>,
    pub(crate) started_at_millis: u64,
    pub(crate) log_path: PathBuf,
    pub(crate) log_start: u64,
}

#[derive(Clone, Debug)]

struct NodeLaunchRecipe {
    observation: NodeLaunchObservation,
    node_dir: PathBuf,
    #[cfg(test)]
    environment: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
    #[cfg(test)]
    current_dir: Option<PathBuf>,
}

impl NodeLaunchRecipe {
    #[cfg(test)]
    fn command(&self) -> Command {
        let mut command = Command::new(&self.observation.program);
        command.args(&self.observation.argv);
        for (key, value) in &self.environment {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        if let Some(path) = &self.current_dir {
            command.current_dir(path);
        }
        command
    }
}

#[derive(Clone, Debug)]

pub(crate) struct NodeStopObservation {
    pub(crate) launch: NodeLaunchObservation,
    pub(crate) stop_requested_at_millis: u64,
    pub(crate) reaped_at_millis: u64,
    pub(crate) code: Option<i32>,
    pub(crate) signal: Option<i32>,
}

/// One stopped owned incarnation, consumed when replaying its own launch.
/// Private recipe/owner fields prevent constructing a donor recipe for a recipient.
#[derive(Debug)]

pub(crate) struct StoppedSnapshotNode {
    pub(crate) observation: NodeStopObservation,
    #[cfg(test)]
    recipe: NodeLaunchRecipe,
    #[cfg(test)]
    follower_name: Option<String>,
}

fn process_observed_millis() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

fn checked_node_interrupt(
    child: &mut ChildGuard,
    launch: NodeLaunchObservation,
) -> Result<NodeStopObservation> {
    use std::os::unix::process::ExitStatusExt as _;
    eyre::ensure!(child.pid() == launch.pid, "owned node incarnation changed");
    eyre::ensure!(
        child.exit_status()?.is_none(),
        "owned node exited before requested snapshot stop"
    );
    let requested = process_observed_millis()?;
    child.interrupt();
    let status = child
        .exit_status()?
        .ok_or_else(|| eyre::eyre!("owned node remained live after SIGINT stop"))?;
    // A default signal exit or fallback SIGKILL is not a successfully closed node.
    eyre::ensure!(
        status.success(),
        "owned node PID {} failed during SIGINT stop: {status}",
        child.pid()
    );
    Ok(NodeStopObservation {
        launch,
        stop_requested_at_millis: requested,
        reaped_at_millis: process_observed_millis()?,
        code: status.code(),
        signal: status.signal(),
    })
}

#[derive(Debug)]
pub struct Localnet {
    cfg: Config,
    /// Owned validator-indexed nodes - the committee (`0..n`) and, when attached,
    /// the joiner (index = committee size).
    validators: HashMap<usize, ChildGuard>,
    /// Only the latest common-spawn incarnation for each node index. Callers
    /// supply an independently checked owned PID before using its log evidence.
    node_launch_logs: HashMap<usize, (u32, LaunchLog)>,

    node_launch_recipes: HashMap<usize, NodeLaunchRecipe>,
    /// Operator-owned validator-indexed Radicle sidecars.
    radicle_sidecars: HashMap<usize, ChildGuard>,
    /// Independent non-validator source node used only by the Radicle E2E.
    user_radicle: Option<ChildGuard>,
    /// Owned follower nodes, keyed by name (`follower`, `follower2`).
    followers: HashMap<String, ChildGuard>,
    follower_startup_probes: HashMap<
        String,
        (
            usize,
            crate::internal::startup_rejection::StartupRejectionProbe,
        ),
    >,
    /// Owned validator-indexed enclave containers (committee + joiner).
    enclaves: HashMap<usize, EnclaveGuard>,
    /// Exact Gramine image used by every enclave in this scenario.
    enclave_image_id: Option<DockerImageId>,
    /// Scenario-only chain-manifest overrides used to prove that a validator
    /// with a different immutable fork install cannot join the canonical
    /// consensus namespace. All ordinary validators use `genesis.json`.
    validator_chain_manifests: HashMap<usize, PathBuf>,
    /// Exact last-launched argv for each validator. Recovery derives its
    /// authority-free follower argv from this snapshot and later restores it.
    validator_argv: HashMap<usize, Vec<String>>,
    /// Validator argv retained across one or more interrupted recovery-follower
    /// runs until the original validator role is relaunched.
    validator_recovery_original_argv: HashMap<usize, Vec<String>>,
    /// The options the last committee `start` ran with, replayed by `restart`.
    start_opts: StartOpts,
    scenario_deadline: Option<std::time::Instant>,
    byzantine_preannounce: bool,
}

impl Localnet {
    pub(crate) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            validators: HashMap::new(),
            node_launch_logs: HashMap::new(),

            node_launch_recipes: HashMap::new(),
            radicle_sidecars: HashMap::new(),
            user_radicle: None,
            followers: HashMap::new(),
            follower_startup_probes: HashMap::new(),
            enclaves: HashMap::new(),
            enclave_image_id: None,
            validator_chain_manifests: HashMap::new(),
            validator_argv: HashMap::new(),
            validator_recovery_original_argv: HashMap::new(),
            start_opts: StartOpts::default(),
            scenario_deadline: None,
            byzantine_preannounce: false,
        }
    }

    pub(crate) fn set_scenario_deadline(&mut self, deadline: std::time::Instant) {
        self.scenario_deadline = Some(deadline);
    }

    /// Enable forged pre-announces only for nodes in this scenario.
    pub(crate) fn arm_byzantine_preannounces(&mut self) {
        self.byzantine_preannounce = true;
    }

    fn enclave_startup_deadline(&self, nonhardware_seconds: u64) -> std::time::Instant {
        let now = std::time::Instant::now();
        let seconds = if self.cfg.tee_mode.passes_sgx_devices() {
            crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS
        } else {
            nonhardware_seconds
        };
        let deadline = now + std::time::Duration::from_secs(seconds);
        // Leave the existing watchdog time for diagnostics and owned teardown.
        self.scenario_deadline.map_or(deadline, |outer| {
            deadline.min(
                outer
                    .checked_sub(std::time::Duration::from_secs(20))
                    .unwrap_or(now),
            )
        })
    }

    fn retain_enclave_image_id(&mut self, image_id: DockerImageId) -> Result<()> {
        match &self.enclave_image_id {
            Some(established) if established != &image_id => {
                bail!("Gramine Docker image identity changed during the scenario");
            }
            Some(_) => Ok(()),
            None => {
                self.enclave_image_id = Some(image_id);
                Ok(())
            }
        }
    }

    fn ensure_enclave_image_once_with<F>(&mut self, resolve: F) -> Result<()>
    where
        F: FnOnce() -> Result<DockerImageId>,
    {
        if self.enclave_image_id.is_some() {
            return Ok(());
        }
        self.retain_enclave_image_id(resolve()?)
    }

    fn ensure_enclave_image_once(&mut self) -> Result<()> {
        // The native profile runs no container, so it must not build, resolve or
        // retain an image identity. `enclave_image_id()` stays `None`, and the
        // evidence records no Gramine image for a run that never used one.
        if self.cfg.tee_mode.runs_native_host_enclave() {
            return Ok(());
        }
        let repo = self.cfg.repo.clone();
        let sudo = self.cfg.sudo;
        let signing_key = self.cfg.dir.join("test-sgx-signing-key.pem");
        self.ensure_enclave_image_once_with(|| {
            proc::ensure_enclave_image(&repo, sudo, &signing_key, None)
        })
    }

    /// The execution profile every enclave in this scenario runs under.
    fn enclave_launch(&self) -> Result<proc::EnclaveLaunch> {
        if self.cfg.tee_mode.runs_native_host_enclave() {
            return Ok(proc::EnclaveLaunch::NativeHost);
        }
        Ok(proc::EnclaveLaunch::Gramine {
            image_id: self
                .enclave_image_id
                .clone()
                .ok_or_else(|| eyre::eyre!("Gramine Docker image identity was not resolved"))?,
        })
    }

    pub(crate) fn enclave_image_id(&self) -> Option<&str> {
        self.enclave_image_id.as_ref().map(DockerImageId::as_str)
    }

    fn sh(&self) -> Sh<'_> {
        Sh::new(&self.cfg)
    }

    fn dir(&self) -> String {
        self.cfg.dir.display().to_string()
    }

    /// Committee size (`--validators`). Not derivable from the port map: the
    /// joiner and followers own blocks past the committee's.
    pub(crate) fn committee_size(&self) -> usize {
        self.cfg.validators
    }

    /// Every reachable harness mode runs an enclave.
    pub fn tee_enabled(&self) -> bool {
        self.cfg.tee_mode.enabled()
    }

    /// Canonical ValidatorSet epoch length authored into this scenario's
    /// ChainSpec. Admission scheduling must derive its boundary window from
    /// this value rather than duplicating a devnet literal.
    pub fn epoch_length_blocks(&self) -> Result<u64> {
        let genesis: serde_json::Value =
            serde_json::from_slice(&fs::read(self.cfg.dir.join("genesis.json"))?)?;
        genesis
            .pointer("/config/epochLengthBlocks")
            .and_then(serde_json::Value::as_u64)
            .filter(|epoch| *epoch > 0)
            .ok_or_else(|| eyre::eyre!("genesis config has no positive epochLengthBlocks"))
    }

    /// Five-second RPC polls allowed for block-1 TEE bootstrap. Consecutive
    /// four-enclave real-SGX evidence exceeded the production-oriented node and
    /// per-request deadlines. A host with 187.5 MiB EPC needed more than ten
    /// minutes while all enclave calls still made progress. So keep the harness
    /// outside its thirty-minute co-located-EPC allowance and let it observe the
    /// node's verdict.
    pub fn tee_bootstrap_wait_attempts(&self) -> u32 {
        if self.cfg.tee_mode.passes_sgx_devices() {
            372
        } else {
            18
        }
    }

    /// OS pid of one owned committee validator. Used only for runtime process
    /// boundary evidence. Callers cannot mutate the process through this API.
    pub fn validator_pid(&self, validator_index: usize) -> Result<u32> {
        self.validators
            .get(&validator_index)
            .map(ChildGuard::pid)
            .ok_or_else(|| eyre::eyre!("validator-{validator_index} is not running"))
    }

    /// Exact owned validator-role nodes, including admitted joiners, not FullNodes.
    pub(crate) fn owned_validator_indices(&self) -> Vec<usize> {
        let mut indices = self.validators.keys().copied().collect::<Vec<_>>();
        indices.sort_unstable();
        indices
    }

    /// Observe both owned children, propagating wait errors instead of treating
    /// an unobservable process as live.
    pub(crate) fn live_validator_and_enclave_pids(&mut self, index: usize) -> Result<(u32, u32)> {
        let node = self
            .validators
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned node"))?;
        eyre::ensure!(
            node.exit_status()?.is_none(),
            "validator-{index} node exited"
        );
        let node_pid = node.pid();
        Ok((node_pid, self.live_enclave_pid(index)?))
    }

    /// The owned foreground enclave launcher must remain observable and live.
    pub(crate) fn live_enclave_pid(&mut self, index: usize) -> Result<u32> {
        let enclave = self
            .enclaves
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned enclave"))?;
        eyre::ensure!(
            enclave.exit_status()?.is_none(),
            "validator-{index} enclave exited"
        );
        Ok(enclave.pid())
    }

    /// Co-located hardware enclaves have an E2E-only startup allowance. The
    /// node's production/testnet default remains unchanged. The node's operator
    /// must choose that value for the deployment topology.
    fn extend_real_sgx_startup_timeout(&self, args: &mut Vec<String>) {
        if self.cfg.tee_mode.passes_sgx_devices() {
            args.extend(args![
                "--tee-bootstrap-timeout-secs",
                crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS
            ]);
        }
    }

    /// The flags common to every node process (committee, joiner, follower):
    /// reth http/p2p/discovery/authrpc/ipc/log for port index `i`, rooted at
    /// `node_dir`. Callers `.extend(args![...])` with their role-specific tail.
    fn reth_base_args(&self, node_dir: &Path, i: usize) -> Vec<String> {
        let data = node_dir.join("data");
        let chain_manifest = self
            .validator_chain_manifests
            .get(&i)
            .cloned()
            .unwrap_or_else(|| self.cfg.dir.join("genesis.json"));
        let mut args = args![
            "node",
            "--chain",
            chain_manifest.display(),
            "--datadir",
            data.display(),
            "--http",
            "--http.addr",
            "0.0.0.0",
            "--http.port",
            self.cfg.http_port(i),
            // `txpool` is required by the pool-eviction scenarios: the pool's
            // real contents are only observable through `txpool_content` /
            // `txpool_status` (`eth_pendingTransactions` does not reflect them).
            "--http.api",
            "eth,net,web3,outbe,txpool",
            "--rpc.eth-proof-window",
            1868,
            "--port",
            self.cfg.p2p_port(i),
            "--discovery.port",
            self.cfg.p2p_port(i),
            "--discovery.v5.addr",
            "127.0.0.1",
            "--discovery.v5.port",
            self.cfg.discv5_port(i),
            "--authrpc.port",
            self.cfg.authrpc_port(i),
            "--ipcpath",
            crate::internal::config::node_ipc_path(node_dir).display(),
            "--engine.persistence-threshold",
            0,
            "--engine.cross-block-cache-size",
            CO_LOCATED_DEVNET_CROSS_BLOCK_CACHE_MIB,
            "--log.file.directory",
            node_dir.join("logs").display(),
            "--color",
            "never",
        ];
        if matches!(self.cfg.tee_mode, crate::env::TeeMode::SgxNoAttest) {
            args.extend(args!["--tee-session-mode", "production-node-host"]);
        }
        args
    }

    /// Comma-joined reth bootnodes from `reth-bootnodes.txt` (comments stripped).
    fn bootnodes(&self) -> Option<String> {
        let raw = fs::read_to_string(self.cfg.dir.join("reth-bootnodes.txt")).ok()?;
        let joined = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join(",");
        (!joined.is_empty()).then_some(joined)
    }

    /// Run a one-shot setup subprocess (`dkg bootstrap`, `seed_genesis.py`).
    /// Quiet by default: this method captures stdout/stderr and surfaces them only
    /// when the command fails. Under `--debug` it streams live and shows the full
    /// DKG/seed progress (`balance: ... entries`, `Total storage entries: ...`, ...).
    fn run_setup(&self, cmd: &mut Command, label: &str) -> Result<()> {
        if self.cfg.debug {
            let status = cmd.status().wrap_err_with(|| format!("run {label}"))?;
            if !status.success() {
                bail!("{label} failed");
            }
        } else {
            let out = cmd.output().wrap_err_with(|| format!("run {label}"))?;
            if !out.status.success() {
                bail!("{label} failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        Ok(())
    }

    /// Spawn an owned node process, logging its launch **metadata** (command,
    /// PID, log path) under `--debug`. The caller already attaches the node's own
    /// runtime stdout/stderr to `<node_dir>/node.log` (via
    /// [`attach_log`](crate::internal::proc::attach_log)). This method does not
    /// stream those live, since interleaving several running nodes would be
    /// unreadable.
    fn spawn_node(
        &mut self,
        label: &str,
        index: usize,
        node_dir: &Path,
        mut cmd: Command,
    ) -> Result<ChildGuard> {
        crate::internal::config::validate_node_ipc_path(node_dir)?;
        let node_args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        ensure_manual_tee_lease_node_args(&node_args)?;
        extend_real_sgx_process_environment(self.cfg.tee_mode, &mut cmd);
        if self.byzantine_preannounce {
            cmd.env("OUTBE_TEST_BYZANTINE_PREANNOUNCE", "1");
        } else {
            cmd.env_remove("OUTBE_TEST_BYZANTINE_PREANNOUNCE");
        }

        if self.cfg.debug {
            let prog = cmd.get_program().to_string_lossy().into_owned();
            let rest: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let rest = redact_args_for_log(&rest);
            eprintln!("[localnet] launch {label}: {prog} {}", rest.join(" "));
            eprintln!("           log: {}", node_dir.join("node.log").display());
        }
        // A failed replacement must not leave the previous incarnation usable
        // as evidence for this launch attempt. Capture before any child writes.
        self.node_launch_logs.remove(&index);

        self.node_launch_recipes.remove(&index);
        let launch_log = LaunchLog::arm(&node_dir.join("node.log"))
            .wrap_err_with(|| format!("capture node-{index} launch log before spawn"))?;

        let mut recipe = NodeLaunchRecipe {
            observation: NodeLaunchObservation {
                index,
                pid: 0,
                program: PathBuf::from(cmd.get_program()),
                argv: cmd
                    .get_args()
                    .map(|arg| {
                        arg.to_str()
                            .map(str::to_owned)
                            .ok_or_else(|| eyre::eyre!("node argv is not UTF-8"))
                    })
                    .collect::<Result<Vec<_>>>()?,
                started_at_millis: process_observed_millis()?,
                log_path: node_dir.join("node.log"),
                log_start: launch_log.start_offset(),
            },
            node_dir: node_dir.to_path_buf(),
            #[cfg(test)]
            environment: cmd
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
                .collect(),
            #[cfg(test)]
            current_dir: cmd.get_current_dir().map(Path::to_path_buf),
        };
        let guard = ChildGuard::spawn(label, cmd)?;

        {
            recipe.observation.pid = guard.pid();
            self.node_launch_recipes.insert(index, recipe);
        }
        self.node_launch_logs
            .insert(index, (guard.pid(), launch_log));
        if self.cfg.debug {
            eprintln!("[localnet] {label} pid {}", guard.pid());
        }
        Ok(guard)
    }

    /// Read only this incarnation's output, including messages emitted before
    /// the caller began polling. This binds the launch, not liveness: callers
    /// must separately verify the expected PID is their current owned process.
    pub(crate) fn node_launch_log(&mut self, index: usize, expected_pid: u32) -> Result<String> {
        let (pid, log) = self
            .node_launch_logs
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("node-{index} has no captured launch log"))?;
        eyre::ensure!(
            *pid == expected_pid,
            "node-{index} launch PID mismatch: expected {expected_pid}, captured {pid}"
        );
        log.read()
            .wrap_err_with(|| format!("read node-{index} launch log for PID {expected_pid}"))
    }

    fn captured_node_recipe(&self, index: usize, expected_pid: u32) -> Result<NodeLaunchRecipe> {
        let recipe = self
            .node_launch_recipes
            .get(&index)
            .ok_or_else(|| eyre::eyre!("node-{index} has no captured launch recipe"))?;
        eyre::ensure!(
            recipe.observation.pid == expected_pid,
            "node-{index} captured launch PID mismatch"
        );
        Ok(recipe.clone())
    }

    pub(crate) fn validator_launch_observation(
        &mut self,
        index: usize,
    ) -> Result<NodeLaunchObservation> {
        let child = self
            .validators
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned node"))?;
        eyre::ensure!(
            child.exit_status()?.is_none(),
            "validator-{index} node exited"
        );
        let pid = child.pid();
        Ok(self.captured_node_recipe(index, pid)?.observation)
    }

    pub(crate) fn stop_validator_for_snapshot(
        &mut self,
        index: usize,
        expected_pid: u32,
    ) -> Result<StoppedSnapshotNode> {
        let observed = self.validator_launch_observation(index)?;
        eyre::ensure!(
            observed.pid == expected_pid,
            "validator-{index} stop PID mismatch"
        );
        #[cfg(test)]
        let recipe = self.captured_node_recipe(index, expected_pid)?;
        let observation = checked_node_interrupt(
            self.validators.get_mut(&index).expect("checked owned node"),
            observed,
        )?;
        self.validators.remove(&index);
        if let Some((pid, log)) = self.node_launch_logs.get_mut(&index) {
            eyre::ensure!(*pid == expected_pid, "node stop log incarnation changed");
            log.seal()?;
        }
        Ok(StoppedSnapshotNode {
            observation,
            #[cfg(test)]
            recipe,
            #[cfg(test)]
            follower_name: None,
        })
    }

    /// Replay this stopped process's actual command. The replay reuses native
    /// stores, keys, enclave, arguments and explicit environment. No provisioning
    /// runs here.
    #[cfg(test)]
    pub(crate) fn resume_snapshot_node(
        &mut self,
        stopped: StoppedSnapshotNode,
    ) -> Result<NodeLaunchObservation> {
        let index = stopped.observation.launch.index;
        let node_dir = self.cfg.validator_dir(index);
        eyre::ensure!(
            stopped.recipe.node_dir == node_dir,
            "stopped recipe belongs to another node directory"
        );
        eyre::ensure!(
            !self.validators.contains_key(&index),
            "node index still has an owned validator"
        );
        eyre::ensure!(
            !self
                .validators
                .values()
                .chain(self.followers.values())
                .any(|child| child.owns_node_data_dir(&node_dir.join("data"))),
            "node datadir still has an owned writer"
        );
        if let Some(name) = &stopped.follower_name {
            eyre::ensure!(
                !self.followers.contains_key(name),
                "follower name still has an owned node"
            );
            follower::ensure_validator_recovery_follower_args(&stopped.recipe.observation.argv)?;
        }
        let label = stopped
            .follower_name
            .clone()
            .unwrap_or_else(|| format!("validator-{index}"));
        let mut command = stopped.recipe.command();
        proc::attach_log(&mut command, &node_dir)?;
        let guard = self.spawn_node(&label, index, &node_dir, command)?;
        let pid = guard.pid();
        if let Some(name) = stopped.follower_name {
            self.followers.insert(name, guard);
        } else {
            self.validators.insert(index, guard);
        }
        // Capture actual final argv again. Projection option handling is idempotent.
        let observed = self.captured_node_recipe(index, pid)?.observation;
        eyre::ensure!(
            observed.argv == stopped.recipe.observation.argv
                && observed.program == stopped.recipe.observation.program,
            "ordinary restart changed actual launch options"
        );
        Ok(observed)
    }
}

fn ensure_manual_tee_lease_node_args(args: &[String]) -> Result<()> {
    if let Some(option) = args.iter().find(|arg| {
        arg.as_str() == "--node-evm-key"
            || arg.starts_with("--node-evm-key=")
            || arg.starts_with("--tee-renewal.")
    }) {
        bail!("generated node command contains removed TEE renewal option {option}");
    }
    Ok(())
}

/// Co-located real enclaves share one physical EPC. A request can therefore
/// complete in the enclave after the production-oriented 30-second host timeout.
/// The enclave then observes a broken pipe even though it produced and sealed the
/// result. Widen only the hardware E2E lane. Production/testnet retain their
/// explicit operator-selected/default deadline.
fn extend_real_sgx_process_environment(mode: crate::env::TeeMode, cmd: &mut Command) {
    if mode.passes_sgx_devices() {
        cmd.env(
            "OUTBE_TEE_IO_TIMEOUT_SECS",
            crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS.to_string(),
        );
    }
}

/// The chain's current worldwide-day key (`YYYYMMDD`), matching how
/// `bootstrap-testnet.sh` seeds genesis: `date_key(now + UTC_PLUS_14_OFFSET)`
/// (lib.sh:33-34).
pub fn worldwide_day() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    outbe_primitives::time::worldwide_day_from_timestamp(now).to_string()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod snapshot_process_tests;
