//! Localnet: the whole network in one handle — bootstrap plus every owned node
//! (committee validators, joiner, followers) and their enclaves.
//!
//! A localnet *is* its set of nodes, so adding/removing a validator, attaching a
//! joiner, or launching a follower are all node operations on this one handle
//! rather than a separate object. Every launched process is **owned** via the
//! guards in [`crate::internal::proc`] (nodes killed on drop, enclave containers
//! `docker rm -f`ed on drop); a dropped `World` tears everything down, with a
//! stateless datadir/run-tag sweep as the SIGINT backstop. The distinct
//! lifecycles live in submodules over this one struct:
//!
//! - [`bootstrap`] — genesis/key generation glue (`dkg bootstrap` + `seed_genesis.py`).
//! - [`committee`] — the bootstrapped validator set (start/stop/restart/kill).
//! - [`joiner`] — a validator that joins a running localnet (index = committee size).
//! - [`follower`] — full-execution follower nodes (`--upstream`).
//! - [`probes`] — datadir moves + node-log inspection.

mod adversarial;
mod bootstrap;
mod committee;
mod follower;
mod joiner;
mod probes;

pub use bootstrap::BootstrapProfile;
pub(crate) use probes::LogAudit;
pub use probes::{CeStartupReplayObservationV1, OcompRuntimeTraceMarkerV1};

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{bail, Result, WrapErr};

use crate::internal::config::Config;
use crate::internal::proc::{args, redact_args_for_log, ChildGuard, DockerImageId, EnclaveGuard};
use crate::internal::shell::Sh;

/// Per-node execution cache for the four validators co-located by the PoC
/// devnet harness. The upstream 4 GiB default is a single-node deployment
/// default; applying it four times would consume the declared 12 GiB process
/// budget before OCOMP begins.
const CO_LOCATED_DEVNET_CROSS_BLOCK_CACHE_MIB: u64 = 512;

/// Test-provided knobs for a localnet start. The **enclave mode** is NOT here —
/// it's an environment decision read from [`Config::tee_mode`]. Only per-scenario
/// parameters live on this struct.
#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    /// Shorten the governance voting window to N blocks (test hook,
    /// `OUTBE_TEST_VOTING_WINDOW_BLOCKS`).
    pub voting_window: Option<u64>,
    /// Signed wall-clock offset used only by debug-node day-boundary E2E.
    pub unix_time_offset_secs: Option<i64>,
    /// The scenario already shifted `genesis.json` before deriving another
    /// immutable manifest binding from it. Nodes still receive the clock
    /// offset, but the common start path must not shift genesis a second time.
    pub genesis_timestamp_pre_shifted: bool,
    /// Bundle identity already pinned by the measurement chain manifest; used
    /// only for the node-local OCOMP UDS handshake.
    pub ocomp_protocol_bundle_hash: Option<String>,
}

impl StartOpts {
    /// A start with a shortened voting window.
    pub fn with_voting_window(window: u64) -> Self {
        Self {
            voting_window: Some(window),
            unix_time_offset_secs: None,
            genesis_timestamp_pre_shifted: false,
            ocomp_protocol_bundle_hash: None,
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
        const SECONDS_PER_DAY: u64 = 86_400;
        let next_day = now_secs - (now_secs % SECONDS_PER_DAY) + SECONDS_PER_DAY;
        let target = next_day.saturating_sub(boundary_lead_secs);
        Self {
            voting_window: Some(window),
            unix_time_offset_secs: Some(target as i64 - now_secs as i64),
            genesis_timestamp_pre_shifted: false,
            ocomp_protocol_bundle_hash: None,
        }
    }

    /// Enable node-local OCOMP UDS for an already-armed measurement manifest.
    pub fn with_ocomp_measurement_bundle(protocol_bundle_hash: String) -> Self {
        Self {
            ocomp_protocol_bundle_hash: Some(protocol_bundle_hash),
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct Localnet {
    cfg: Config,
    /// Owned validator-indexed nodes — the committee (`0..n`) and, when attached,
    /// the joiner (index = committee size).
    validators: HashMap<usize, ChildGuard>,
    /// Owned follower nodes, keyed by name (`follower`, `follower2`).
    followers: HashMap<String, ChildGuard>,
    /// Owned validator-indexed enclave containers (committee + joiner).
    enclaves: HashMap<usize, EnclaveGuard>,
    /// Exact Gramine image used by every enclave in this scenario.
    enclave_image_id: Option<DockerImageId>,
    /// Scenario-only chain-manifest overrides used to prove that a validator
    /// with a different immutable fork install cannot join the canonical
    /// consensus namespace. All ordinary validators use `genesis.json`.
    validator_chain_manifests: HashMap<usize, PathBuf>,
    /// The options the last committee `start` ran with, replayed by `restart`.
    start_opts: StartOpts,
    /// Scenario-scoped executable overrides used only by explicit adversarial
    /// tests. Normal validators always use `cfg.bin_chain`.
    validator_binary_overrides: HashMap<usize, PathBuf>,
    /// Scenario-scoped environment passed only to the corresponding overridden
    /// validator process.
    validator_environment_overrides: HashMap<usize, Vec<(String, String)>>,
}

impl Localnet {
    pub(crate) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            validators: HashMap::new(),
            followers: HashMap::new(),
            enclaves: HashMap::new(),
            enclave_image_id: None,
            validator_chain_manifests: HashMap::new(),
            start_opts: StartOpts::default(),
            validator_binary_overrides: HashMap::new(),
            validator_environment_overrides: HashMap::new(),
        }
    }

    fn retain_enclave_image_id(&mut self, image_id: DockerImageId) -> Result<()> {
        match &self.enclave_image_id {
            Some(established) if established != &image_id => {
                bail!("Gramine Docker image identity changed during the scenario")
            }
            Some(_) => Ok(()),
            None => {
                self.enclave_image_id = Some(image_id);
                Ok(())
            }
        }
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
    fn committee_size(&self) -> usize {
        self.cfg.validators
    }

    /// Every reachable harness mode runs an enclave.
    pub fn tee_enabled(&self) -> bool {
        self.cfg.tee_mode.enabled()
    }

    /// Five-second RPC polls allowed for block-1 TEE bootstrap. Consecutive
    /// four-enclave real-SGX evidence exceeded the production-oriented node and
    /// per-request deadlines; keep the harness outside its five-minute
    /// co-located-EPC allowance so it observes the node's verdict.
    pub fn tee_bootstrap_wait_attempts(&self) -> u32 {
        if matches!(self.cfg.tee_mode, crate::env::TeeMode::Real) {
            72
        } else {
            18
        }
    }

    /// OS pid of one owned committee validator. Used only for runtime process
    /// boundary evidence; callers cannot mutate the process through this API.
    pub fn validator_pid(&self, validator_index: usize) -> Result<u32> {
        self.validators
            .get(&validator_index)
            .map(ChildGuard::pid)
            .ok_or_else(|| eyre::eyre!("validator-{validator_index} is not running"))
    }

    /// Co-located hardware enclaves have an E2E-only startup allowance. The
    /// node's production/testnet default remains unchanged and must be chosen
    /// for the deployment topology by its operator.
    fn extend_real_sgx_startup_timeout(&self, args: &mut Vec<String>) {
        if matches!(self.cfg.tee_mode, crate::env::TeeMode::Real) {
            args.extend(args!["--tee-bootstrap-timeout-secs", "180"]);
        }
    }

    /// The flags common to every node process (committee, joiner, follower):
    /// reth http/p2p/discovery/authrpc/ipc/log for port index `i`, rooted at
    /// `node_dir`. Callers `.extend(args![…])` with their role-specific tail.
    fn reth_base_args(&self, node_dir: &Path, i: usize) -> Vec<String> {
        let data = node_dir.join("data");
        let chain_manifest = self
            .validator_chain_manifests
            .get(&i)
            .cloned()
            .unwrap_or_else(|| self.cfg.dir.join("genesis.json"));
        args![
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
            "--http.api",
            "eth,net,web3,outbe,debug",
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
            data.join("reth.ipc").display(),
            "--engine.persistence-threshold",
            0,
            "--engine.cross-block-cache-size",
            CO_LOCATED_DEVNET_CROSS_BLOCK_CACHE_MIB,
            "--log.file.directory",
            node_dir.join("logs").display(),
        ]
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
    /// Quiet by default — stdout/stderr are captured and only surfaced when the
    /// command fails; under `--debug` it streams live so the full DKG/seed
    /// progress (`balance: … entries`, `Total storage entries: …`, …) is shown.
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
    /// PID, log path) under `--debug`. The node's own runtime stdout/stderr are
    /// already attached to `<node_dir>/node.log` by the caller (via
    /// [`attach_log`](crate::internal::proc::attach_log)) — we don't stream those
    /// live, since interleaving several running nodes would be unreadable.
    fn spawn_node(&self, label: &str, node_dir: &Path, mut cmd: Command) -> Result<ChildGuard> {
        extend_real_sgx_process_environment(self.cfg.tee_mode, &mut cmd);
        cmd.env(
            "OUTBE_PROJECTION_MONGODB_URI",
            &self.cfg.projection_mongodb_uri,
        )
        .env(
            "OUTBE_PROJECTION_MONGODB_DATABASE",
            format!(
                "{}_scenario_{}_{}",
                self.cfg.projection_database_prefix, self.cfg.scenario, label
            ),
        );
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
        let guard = ChildGuard::spawn(label, cmd)?;
        if self.cfg.debug {
            eprintln!("[localnet] {label} pid {}", guard.pid());
        }
        Ok(guard)
    }

    // ---- teardown ------------------------------------------------------------

    /// Drop the owned node handles (killing nodes + `docker rm -f`ing enclaves),
    /// then run a stateless backstop sweep. Its primary role is the SIGINT path,
    /// where the `World` is never dropped so the guards never fire (plus
    /// intra-run belt-and-suspenders between scenarios). It is scoped to this
    /// run's unique data subdir + enclave run tag, so it never touches another
    /// run's nodes/containers.
    fn shutdown(&mut self) {
        // Nodes first (release MDBX locks), then their enclaves — matching the
        // stop-nodes-then-teardown-enclaves ordering `run-testnet.sh` used.
        self.validators.clear();
        self.followers.clear();
        self.enclaves.clear();
        // No settle needed here: clearing the maps dropped every guard, which
        // synchronously `kill()`s + `wait()`s the owned nodes/enclaves, and the
        // sweep below is a fire-and-forget backstop.

        let nodes = format!("outbe-chain node.*{}", self.dir());
        self.sh().sudo_best_effort("pkill", &["-9", "-f", &nodes]);
        let tee_sweep = format!(
            "docker ps -aq --filter name=outbe-tee-gramine-{}- | xargs -r docker rm -f",
            self.cfg.run_tag
        );
        self.sh().sudo_best_effort("bash", &["-c", &tee_sweep]);
    }

    /// Remove `cfg.dir` (this localnet's scenario dir, or the whole run dir when
    /// built from the run-level [`Config`]). Already-gone is success.
    ///
    /// `validator-<i>/tee` is the enclave container's only writable mount
    /// (`proc::spawn_enclave`), so under `--sudo` it is the only thing this user
    /// can't unlink — drop those with `sudo rm` first. Everything else the
    /// harness created itself, and a failure to remove it is a real error.
    pub fn wipe(&self) -> Result<()> {
        if !self.cfg.dir.exists() {
            return Ok(());
        }
        if self.cfg.sudo {
            for tee in sealed_dirs(&self.cfg.dir) {
                self.sh()
                    .sudo_best_effort("rm", &["-rf", &tee.display().to_string()]);
            }
        }
        fs::remove_dir_all(&self.cfg.dir)
            .wrap_err_with(|| format!("wiping data dir {}", self.dir()))
    }

    /// Post-run teardown: shut down all nodes + enclave containers, leaving the
    /// data dir (logs/chain state) intact for inspection. Invoked from the
    /// cucumber `after` hook (and the SIGINT handler).
    pub fn teardown(&mut self) {
        self.shutdown();
    }

    /// Stop the localnet (alias for [`teardown`](Self::teardown)).
    pub fn stop(&mut self) -> Result<()> {
        self.shutdown();
        Ok(())
    }
}

/// Co-located real enclaves share one physical EPC. A request can therefore
/// complete in the enclave after the production-oriented 30-second host timeout:
/// the enclave then observes a broken pipe even though it produced and sealed the
/// result. Widen only the hardware E2E lane; production/testnet retain their
/// explicit operator-selected/default deadline.
fn extend_real_sgx_process_environment(mode: crate::env::TeeMode, cmd: &mut Command) {
    if matches!(mode, crate::env::TeeMode::Real) {
        cmd.env("OUTBE_TEE_IO_TIMEOUT_SECS", "300");
    }
}

/// The sealed-state dirs under `root` — the only paths the (root) enclave
/// container writes. `root` is either a scenario dir (`validator-<i>/tee`) or a
/// run dir (`scenario-<n>/validator-<i>/tee`); both shapes are checked.
///
/// Deliberately not a recursive walk: `validator-<i>/data` holds the reth MDBX
/// store, and descending into it would cost far more than the two `read_dir`s
/// this needs.
fn sealed_dirs(root: &Path) -> Vec<PathBuf> {
    fn push_validator_tee(base: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(base) else {
            return;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("validator-") {
                let tee = e.path().join("tee");
                if tee.is_dir() {
                    out.push(tee);
                }
            }
        }
    }

    let mut out = Vec::new();
    push_validator_tee(root, &mut out);
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("scenario-") {
                push_validator_tee(&e.path(), &mut out);
            }
        }
    }
    out
}

/// The chain's current worldwide-day key (`YYYYMMDD`), matching how
/// `bootstrap-testnet.sh` seeds genesis: `date_key(now + UTC_PLUS_14_OFFSET)`
/// (lib.sh:33-34). Pure-Rust civil-date conversion so no `date(1)` shell-out.
pub fn worldwide_day() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ymd_utc(now + 50_400)
}

/// `YYYYMMDD` for a UTC epoch second (Howard Hinnant's `civil_from_days`).
pub(crate) fn ymd_utc(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Environment;
    use std::ffi::OsStr;

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

        assert_eq!(configured_timeout(TeeMode::Real).as_deref(), Some("300"));
        assert_eq!(configured_timeout(TeeMode::Mock), None);
        assert_eq!(configured_timeout(TeeMode::GramineDirect), None);
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

    /// Both layouts, and nothing else — in particular not `validator-*/data`.
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
        const SECONDS_PER_DAY: u64 = 86_400;

        let opts = StartOpts::near_next_utc_day_with_lead(6, NOW, LEAD);
        let next_day = NOW - (NOW % SECONDS_PER_DAY) + SECONDS_PER_DAY;

        assert_eq!(opts.voting_window, Some(6));
        assert_eq!(
            opts.unix_time_offset_secs,
            Some((next_day - LEAD) as i64 - NOW as i64)
        );
        assert!(!opts.genesis_timestamp_pre_shifted);
    }
}
