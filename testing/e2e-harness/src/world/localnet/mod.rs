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
//! - [`probes`] - datadir, compressed-entity, and timing observations.
//! - [`enclave`] - enclave image identity, execution profile, and startup budgets.
//! - [`teardown`] - owned-process shutdown and scoped data cleanup.
//! - [`timing`] - start options and scenario clock calculations.

mod bootstrap;
mod committee;
mod enclave;
mod follower;
mod joiner;
mod log_audit;
mod probes;
mod radicle;
#[cfg(feature = "ocomp-integration")]
mod snapshot;
mod teardown;
mod timing;

pub use bootstrap::BootstrapProfile;
pub(crate) use log_audit::{ExpectedTeeFailures, LogAudit};
pub use probes::{CeStartupReplayObservationV1, OcompRuntimeTraceMarkerV1};
#[cfg(feature = "ocomp-integration")]
pub(crate) use radicle::RadicleRepositoryFixtureV1;
#[cfg(feature = "ocomp-integration")]
use snapshot::{checked_node_interrupt, process_observed_millis, NodeLaunchRecipe};
#[cfg(feature = "ocomp-integration")]
pub(crate) use snapshot::{NodeLaunchObservation, StoppedSnapshotNode};
pub(crate) use timing::ymd_utc;
pub use timing::{worldwide_day, StartOpts};

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{bail, Result, WrapErr};

use crate::internal::config::Config;
use crate::internal::launch_log::LaunchLog;
#[cfg(test)]
use crate::internal::proc;
use crate::internal::proc::{args, redact_args_for_log, ChildGuard, DockerImageId, EnclaveGuard};
use crate::internal::shell::Sh;
#[cfg(test)]
use enclave::extend_real_sgx_process_environment;
#[cfg(test)]
use teardown::sealed_dirs;

/// Per-node execution cache for validators co-located by the devnet harness.
/// The upstream 4 GiB default is a single-node deployment default. Applying it
/// to every local validator would consume the process budget before OCOMP begins.
const CO_LOCATED_DEVNET_CROSS_BLOCK_CACHE_MIB: u64 = 512;

#[derive(Clone, Debug)]
#[cfg(feature = "ocomp-integration")]
pub(crate) struct NodeStopObservation {
    pub(crate) launch: NodeLaunchObservation,
    pub(crate) stop_requested_at_millis: u64,
    pub(crate) reaped_at_millis: u64,
    pub(crate) code: Option<i32>,
    pub(crate) signal: Option<i32>,
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
    #[cfg(feature = "ocomp-integration")]
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
            #[cfg(feature = "ocomp-integration")]
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
        args.extend(args![
            "--tee-session-mode",
            if self.cfg.uses_production_node_host_session() {
                "production-node-host"
            } else {
                "development"
            }
        ]);
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
        enclave::extend_real_sgx_process_environment(self.cfg.tee_mode, &mut cmd);
        if self.byzantine_preannounce {
            cmd.env("OUTBE_TEST_BYZANTINE_PREANNOUNCE", "1");
        } else {
            cmd.env_remove("OUTBE_TEST_BYZANTINE_PREANNOUNCE");
        }
        crate::world::projection::configure_node_command(&self.cfg, index, &mut cmd)?;
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
        #[cfg(feature = "ocomp-integration")]
        self.node_launch_recipes.remove(&index);
        let launch_log = LaunchLog::arm(&node_dir.join("node.log"))
            .wrap_err_with(|| format!("capture node-{index} launch log before spawn"))?;
        #[cfg(feature = "ocomp-integration")]
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
            environment: cmd
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
                .collect(),
            current_dir: cmd.get_current_dir().map(Path::to_path_buf),
        };
        let guard = ChildGuard::spawn(label, cmd)?;
        #[cfg(feature = "ocomp-integration")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Environment;
    use std::ffi::OsStr;

    #[test]
    fn node_ipc_preflight_rejects_dynamic_node_before_spawning() {
        let env = Environment::default();
        let mut localnet = Localnet::new(Config::resolve(&env));
        let node_dir = PathBuf::from("/tmp").join("long-node-directory-".repeat(10));
        let error = localnet
            .spawn_node(
                "unstarted-follower",
                15,
                &node_dir,
                Command::new("/nonexistent/e2e-node"),
            )
            .expect_err("invalid IPC path must precede executable lookup");
        assert!(error.to_string().contains("invalid node IPC socket path"));
        assert!(error.to_string().contains(&node_dir.display().to_string()));
    }

    fn launch_log_fixture(localnet: &mut Localnet, index: usize, marker: &str) -> u32 {
        use std::time::{Duration, Instant};

        let node_dir = localnet.cfg.validator_dir(index);
        fs::create_dir_all(&node_dir).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s\\n' \"$1\"", "node-log-fixture", marker]);
        proc::attach_log(&mut command, &node_dir).unwrap();
        let mut child = localnet
            .spawn_node("node-log-fixture", index, &node_dir, command)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.exit_status().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "log fixture did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = child.pid();
        localnet.validators.insert(index, child);
        pid
    }

    #[test]
    fn node_launch_log_excludes_prior_incarnation_and_rejects_stale_pid() {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        let node_dir = localnet.cfg.validator_dir(0);
        fs::create_dir_all(&node_dir).unwrap();
        fs::write(node_dir.join("node.log"), "earlier matching marker\n").unwrap();
        let first = launch_log_fixture(&mut localnet, 0, "first launch");
        assert_eq!(
            localnet.node_launch_log(0, first).unwrap(),
            "first launch\n"
        );
        localnet.validators.remove(&0);
        let second = launch_log_fixture(&mut localnet, 0, "second launch");
        assert_eq!(
            localnet.node_launch_log(0, second).unwrap(),
            "second launch\n"
        );
        assert!(localnet.node_launch_log(0, first).is_err());
        assert!(localnet.node_launch_log(1, second).is_err());
    }

    #[test]
    fn node_launch_log_rejects_missing_replaced_and_truncated_log() {
        for damage in ["missing", "replaced", "truncated"] {
            let dir = tempfile::tempdir().unwrap();
            let env = Environment {
                data_dir: dir.path().canonicalize().unwrap(),
                ..Environment::default()
            };
            let mut localnet = Localnet::new(Config::resolve(&env));
            let pid = launch_log_fixture(&mut localnet, 0, "current matching marker");
            assert_eq!(
                localnet.node_launch_log(0, pid).unwrap(),
                "current matching marker\n"
            );
            let path = localnet.cfg.validator_dir(0).join("node.log");
            match damage {
                "missing" => fs::remove_file(&path).unwrap(),
                "replaced" => {
                    fs::rename(&path, path.with_extension("previous")).unwrap();
                    fs::write(&path, "current matching marker\n").unwrap();
                }
                "truncated" => fs::write(&path, "").unwrap(),
                _ => unreachable!(),
            }
            assert!(localnet.node_launch_log(0, pid).is_err(), "{damage}");
        }
    }

    #[test]
    fn node_launch_log_failed_spawn_discards_previous_capture() {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        let pid = launch_log_fixture(&mut localnet, 0, "previous launch");
        localnet.validators.remove(&0);
        let node_dir = localnet.cfg.validator_dir(0);
        assert!(localnet
            .spawn_node(
                "missing-node",
                0,
                &node_dir,
                Command::new(dir.path().join("nonexistent-node"))
            )
            .is_err());
        assert!(localnet.node_launch_log(0, pid).is_err());
        assert!(localnet.node_launch_logs.is_empty());
    }

    #[test]
    fn node_launch_log_cleanup_drops_capture_but_retains_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        let pid = launch_log_fixture(&mut localnet, 0, "retained diagnostics");
        localnet.clear_owned_nodes();
        assert!(localnet.node_launch_logs.is_empty());
        assert!(localnet.validators.is_empty());
        assert!(localnet.node_launch_log(0, pid).is_err());
        assert_eq!(
            fs::read_to_string(localnet.cfg.validator_dir(0).join("node.log")).unwrap(),
            "retained diagnostics\n"
        );
    }

    #[test]
    fn node_ipc_preflight_rejects_bootstrap_before_files_or_subprocesses() {
        let directory = tempfile::tempdir().expect("test directory");
        let env = Environment {
            data_dir: directory.path().join("long-scenario-directory-".repeat(6)),
            seed: directory.path().join("nonexistent-seed.json"),
            chain_bin: directory.path().join("nonexistent-node"),
            ..Environment::default()
        };
        let localnet = Localnet::new(Config::resolve(&env));
        let error = localnet
            .bootstrap_with_profile(4, &BootstrapProfile::default())
            .expect_err("path validation must precede seed reads and bootstrap");
        assert!(error.to_string().contains("invalid node IPC socket path"));
        assert!(
            !env.data_dir.exists(),
            "failed preflight must not create scenario state"
        );
    }

    fn configured_timeout(mode: crate::env::TeeMode) -> Option<String> {
        let mut cmd = Command::new("outbe-chain");
        extend_real_sgx_process_environment(mode, &mut cmd);
        cmd.get_envs()
            .find(|(key, _)| *key == OsStr::new("OUTBE_TEE_IO_TIMEOUT_SECS"))
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
    }

    #[test]
    fn co_located_hardware_lane_alone_widens_enclave_io_timeout() {
        use crate::env::TeeMode;

        let expected = Some(crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS.to_string());
        assert_eq!(configured_timeout(TeeMode::Real), expected);
        assert_eq!(configured_timeout(TeeMode::SgxNoAttest), expected);
        assert_eq!(configured_timeout(TeeMode::Mock), None);
        assert_eq!(configured_timeout(TeeMode::GramineDirect), None);
    }

    #[test]
    fn enclave_startup_budget_is_profile_aware_and_never_extends_scenario() {
        use crate::env::TeeMode;
        use std::time::{Duration, Instant};
        for mode in [
            TeeMode::Real,
            TeeMode::SgxNoAttest,
            TeeMode::GramineDirect,
            TeeMode::Mock,
            TeeMode::MockNative,
        ] {
            let env = Environment {
                tee_mode: mode,
                ..Environment::default()
            };
            let mut localnet = Localnet::new(Config::resolve(&env));
            for seconds in [10, 20] {
                let before = Instant::now();
                let deadline = localnet.enclave_startup_deadline(seconds);
                let budget = if mode.passes_sgx_devices() {
                    crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS
                } else {
                    seconds
                };
                assert!(deadline >= before + Duration::from_secs(budget));
                assert!(deadline <= Instant::now() + Duration::from_secs(budget));
            }
            let outer = Instant::now() + Duration::from_secs(21);
            localnet.set_scenario_deadline(outer);
            assert_eq!(
                localnet.enclave_startup_deadline(20),
                outer - Duration::from_secs(20)
            );
        }
    }

    #[test]
    fn sgx_no_attest_selects_production_node_host_session() {
        let env = Environment {
            tee_mode: crate::env::TeeMode::SgxNoAttest,
            ..Environment::default()
        };
        env.ports
            .start_scenario(env.validators)
            .expect("allocate deterministic scenario ports");
        let localnet = Localnet::new(Config::for_scenario(&env, 1));
        let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);

        assert!(args
            .windows(2)
            .any(|pair| { pair[0] == "--tee-session-mode" && pair[1] == "production-node-host" }));
    }

    #[test]
    fn sgx_no_attest_uses_the_hardware_bootstrap_allowance() {
        let env = Environment {
            tee_mode: crate::env::TeeMode::SgxNoAttest,
            ..Environment::default()
        };
        env.ports
            .start_scenario(env.validators)
            .expect("allocate deterministic scenario ports");
        let localnet = Localnet::new(Config::for_scenario(&env, 1));

        assert_eq!(localnet.tee_bootstrap_wait_attempts(), 372);
        let mut args = Vec::new();
        localnet.extend_real_sgx_startup_timeout(&mut args);
        let bootstrap_timeout = args[1]
            .parse::<u64>()
            .expect("numeric hardware-SGX bootstrap timeout");
        let io_timeout = configured_timeout(crate::env::TeeMode::SgxNoAttest)
            .expect("hardware-SGX enclave I/O override")
            .parse::<u64>()
            .expect("numeric hardware-SGX enclave I/O timeout");
        assert_eq!(
            args,
            vec![
                "--tee-bootstrap-timeout-secs".to_owned(),
                crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS.to_string()
            ],
            "the node deadline must stay inside the harness observation envelope"
        );
        assert!(
            io_timeout >= bootstrap_timeout,
            "an individual SGX request must not expire before the enclosing bootstrap budget"
        );
    }

    #[test]
    fn co_located_devnet_bounds_each_nodes_cross_block_cache() {
        let env = Environment::default();
        env.ports
            .start_scenario(env.validators)
            .expect("allocate deterministic scenario ports");
        let localnet = Localnet::new(Config::for_scenario(&env, 1));
        let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);
        let cache_size = args
            .windows(2)
            .find(|pair| pair[0] == "--engine.cross-block-cache-size")
            .map(|pair| pair[1].as_str());

        assert_eq!(cache_size, Some("512"));
    }

    #[test]
    fn every_localnet_node_retains_the_ocomp_exact_state_proof_window() {
        let env = Environment::default();
        env.ports
            .start_scenario(env.validators)
            .expect("allocate deterministic scenario ports");
        let localnet = Localnet::new(Config::for_scenario(&env, 1));
        let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);
        let proof_window = args
            .windows(2)
            .find(|pair| pair[0] == "--rpc.eth-proof-window")
            .map(|pair| pair[1].as_str());

        assert_eq!(proof_window, Some("1868"));
    }

    #[test]
    fn runtime_audit_stdout_is_free_of_ansi_formatting() {
        let env = Environment::default();
        env.ports
            .start_scenario(env.validators)
            .expect("allocate deterministic scenario ports");
        let localnet = Localnet::new(Config::for_scenario(&env, 1));
        let args = localnet.reth_base_args(Path::new("/tmp/outbe-e2e-node"), 0);

        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--color" && pair[1] == "never"));
    }

    #[test]
    fn generated_node_commands_reject_removed_tee_renewal_key_options() {
        let ordinary = vec!["node".to_owned(), "--validator".to_owned()];
        assert!(ensure_manual_tee_lease_node_args(&ordinary).is_ok());

        for removed in [
            "--node-evm-key",
            "--node-evm-key=/tmp/evm-key.hex",
            "--tee-renewal.relay-key",
            "--tee-renewal.rpc-url=http://127.0.0.1:8545",
            "--tee-renewal.poll-secs",
            "--tee-renewal.warning-blocks",
            "--tee-renewal.critical-blocks",
        ] {
            assert!(
                ensure_manual_tee_lease_node_args(&[removed.to_owned()]).is_err(),
                "removed option still accepted: {removed}"
            );
        }
    }

    /// Both layouts, and nothing else - in particular not `validator-*/data`.
    #[test]
    fn sealed_dirs_finds_only_enclave_mounts() {
        let root = std::env::temp_dir().join(format!("outbe-sealed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // run-dir shape
        fs::create_dir_all(root.join("scenario-1/validator-0/tee")).expect("mk");
        fs::create_dir_all(root.join("scenario-1/validator-0/data")).expect("mk");
        // scenario-dir shape (a Localnet wiping its own dir)
        fs::create_dir_all(root.join("validator-7/tee")).expect("mk");
        fs::create_dir_all(root.join("validator-7/logs")).expect("mk");

        let mut found = sealed_dirs(&root);
        found.sort();
        assert_eq!(
            found,
            vec![
                root.join("scenario-1/validator-0/tee"),
                root.join("validator-7/tee"),
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn worldwide_day_is_eight_digits() {
        assert_eq!(ymd_utc(0), "19700101");
        let wd = worldwide_day();
        assert_eq!(wd.len(), 8);
        assert!(wd.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn custom_day_boundary_lead_is_reflected_exactly_in_the_node_clock_offset() {
        const NOW: u64 = 1_700_000_000;
        const LEAD: u64 = 240;
        use outbe_primitives::time::SECONDS_PER_DAY;

        let opts = StartOpts::near_next_utc_day_with_lead(6, NOW, LEAD);
        let next_day = NOW - (NOW % SECONDS_PER_DAY) + SECONDS_PER_DAY;

        assert_eq!(opts.voting_window, Some(6));
        assert_eq!(
            opts.unix_time_offset_secs,
            Some((next_day - LEAD) as i64 - NOW as i64)
        );
        assert!(!opts.genesis_timestamp_pre_shifted);
        assert!(!opts.is_txpool_eviction_profile);
    }

    #[test]
    fn shortened_txpool_policy_is_an_explicit_scenario_profile() {
        let ordinary = StartOpts::with_voting_window(6);
        let eviction = StartOpts::with_txpool_eviction_profile(6);

        assert!(!ordinary.is_txpool_eviction_profile);
        assert!(eviction.is_txpool_eviction_profile);
        assert_eq!(eviction.voting_window, Some(6));
        assert_eq!(eviction.unix_time_offset_secs, None);
    }
}
