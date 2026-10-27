//! The bootstrapped committee: start/stop/restart the validator set and its
//! enclaves, owned as Rust child processes.

use std::collections::HashMap;
use std::fs;

use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eyre::ensure;
use eyre::{bail, Result, WrapErr};

use crate::internal::proc::{self, args, attach_log};

use super::{Localnet, StartOpts};

mod enclave;
mod upgrade;

#[cfg(test)]
use upgrade::{cargo_package_version, replacement_chain_build_args, rewrite_workspace_version};

fn unix_time_offset_arg(offset: i64) -> String {
    format!("--testnet.unix-time-offset-secs={offset}")
}

fn verify_genesis_governance_window(genesis: &serde_json::Value, expected: u64) -> Result<()> {
    let parameters = outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(genesis)?;
    ensure!(
        parameters.governance_voting_window_blocks == expected,
        "genesis governance voting window {} differs from requested {expected}; configure the bootstrap profile before binding identities",
        parameters.governance_voting_window_blocks
    );
    Ok(())
}

fn restore_validator_argv(
    rebuilt: Vec<String>,
    recovery_original: Option<Vec<String>>,
) -> Vec<String> {
    recovery_original.unwrap_or(rebuilt)
}

fn committee_signal_args(pids: &[u32], signal: &str) -> Vec<String> {
    std::iter::once(format!("-{signal}"))
        .chain(std::iter::once("--".to_owned()))
        .chain(pids.iter().map(u32::to_string))
        .collect()
}

fn signal_committee(pids: &[u32], signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args(committee_signal_args(pids, signal))
        .status()
        .wrap_err_with(|| format!("signal validator committee with SIG{signal}"))?;
    if !status.success() {
        // A partially delivered STOP would leave an owned test process frozen.
        // Best-effort resume the complete cohort before surfacing the failure.
        let _ = Command::new("kill")
            .args(committee_signal_args(pids, "CONT"))
            .status();
        bail!("failed to signal validator committee with SIG{signal}");
    }
    Ok(())
}

fn quiesce_and_terminate_committee_with(
    pids: &[u32],
    mut signal: impl FnMut(&[u32], &str) -> Result<()>,
) -> Result<()> {
    signal(pids, "STOP")?;
    if let Err(error) = signal(pids, "TERM") {
        let _ = signal(pids, "CONT");
        return Err(error);
    }
    signal(pids, "CONT")
}

fn reap_stopped_committee(validators: &mut HashMap<usize, proc::ChildGuard>) -> Result<()> {
    let deadline = Instant::now() + proc::GRACEFUL_STOP_TIMEOUT;
    let mut failures = Vec::new();
    for (index, child) in validators {
        let pid = child.pid();
        let remaining = deadline.saturating_duration_since(Instant::now());
        match child.wait_for_exit(remaining) {
            Ok(status) if status.success() => {}
            Ok(status) => {
                failures.push(format!("validator-{index} PID {pid} exited with {status}"))
            }
            Err(error) => failures.push(format!("validator-{index} PID {pid}: {error:#}")),
        }
    }
    if !failures.is_empty() {
        failures.sort();
        bail!("committee stop failed: {}", failures.join("; "));
    }
    Ok(())
}

/// Preparation may block on external services. No node gets a head start while
/// another member is still preparing. Keep launch ownership with the caller.
fn start_prepared_cohort<C>(
    context: &mut C,
    indices: &[usize],
    mut prepare: impl FnMut(&mut C, usize) -> Result<()>,
    mut launch: impl FnMut(&mut C, usize) -> Result<()>,
) -> Result<()> {
    for &index in indices {
        prepare(context, index)?;
    }
    for &index in indices {
        launch(context, index)?;
    }
    Ok(())
}

fn verify_pre_dkg_public_identity(
    response: outbe_tee::protocol::EnclaveResponse,
    manifest: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
) -> Result<()> {
    match response {
        outbe_tee::protocol::EnclaveResponse::PublicKeys {
            offer_key_ready: false,
            recipient_x25519_pub,
            attestation_pub,
            noise_static_pub,
            ..
        } => {
            ensure!(
                recipient_x25519_pub == manifest.recipient_x25519
                    && attestation_pub == manifest.attestation_ed25519
                    && noise_static_pub == manifest.noise_responder_x25519,
                "prepared founder public identity differs from its committed manifest"
            );
            Ok(())
        }
        _ => {
            bail!("fresh founder preparation requires authenticated keyless PublicKeys");
        }
    }
}

impl Localnet {
    /// Start the committee (and, when TEE is enabled, its enclaves). Idempotent:
    /// this method skips indices whose owned node is still alive, so [`restart`]
    /// only relaunches the ones that died.
    pub fn start(&mut self, opts: &StartOpts) -> Result<()> {
        if let Some(window) = opts.voting_window {
            let genesis = serde_json::from_slice(&fs::read(self.cfg.dir.join("genesis.json"))?)?;
            verify_genesis_governance_window(&genesis, window)?;
        }
        self.start_opts = opts.clone();
        let n = self.committee_size();
        if self.tee_enabled() {
            self.ensure_enclave_image_once()?;
        }
        let bootnodes = self.bootnodes();
        let chain_id_hex = if self.tee_enabled() {
            Some(self.chain_id_hex()?)
        } else {
            None
        };

        let mut launched = Vec::new();
        for i in 0..n {
            if self.validators.get_mut(&i).is_some_and(|g| !g.exited()) {
                continue; // already running
            }
            self.validators.remove(&i); // clear a dead handle if present
            launched.push(i);
        }

        // TCP listening alone does not prove authenticated NodeHost readiness.
        // Complete cold initialization before any node starts its independent
        // genesis-formation timer. Actual formation and DKG remain node-owned.
        start_prepared_cohort(
            self,
            &launched,
            |localnet, i| {
                if localnet.tee_enabled() && !localnet.enclaves.contains_key(&i) {
                    localnet.start_enclave(i, chain_id_hex.as_deref().unwrap_or_default())?;
                }
                localnet.prepare_fresh_founder_node_host(i)?;
                localnet.start_radicle(i)
            },
            |localnet, i| localnet.launch_validator(i, opts, bootnodes.as_deref()),
        )?;

        // Survival check: a node that dies in the first couple seconds is a
        // config error. Surface it with its log tail.
        sleep(Duration::from_secs(2));
        for &i in &launched {
            if self.validators.get_mut(&i).is_some_and(|g| g.exited()) {
                let tail = self.tail_log(i, 20);
                self.validators.remove(&i);
                bail!("validator-{i} exited during startup:\n{tail}");
            }
        }
        Ok(())
    }

    /// Require that every process owned as a committee validator is still alive.
    ///
    /// RPC reachability alone cannot establish ownership: another local run may
    /// be listening on the same static port. The persistent LocalNet driver calls
    /// this throughout readiness and only accepts heights from this live cohort.
    pub fn ensure_committee_alive(&mut self) -> Result<()> {
        for index in 0..self.committee_size() {
            let Some(guard) = self.validators.get_mut(&index) else {
                bail!("validator-{index} has no owned process");
            };
            if guard.exited() {
                let tail = self.tail_log(index, 20);
                bail!("validator-{index} exited during readiness:\n{tail}");
            }
        }
        Ok(())
    }

    /// Relaunch any committee validator whose node died (e.g. one killed in the
    /// DKG-failure recovery), reusing the last start's options. Live nodes and
    /// still-running enclaves are left in place.
    pub fn restart(&mut self) -> Result<()> {
        let opts = self.start_opts.clone();
        self.start(&opts)
    }

    /// Stop the complete validator committee before changing the shared
    /// testnet-only logical clock, then relaunch every validator with preserved
    /// datadirs and enclaves. Freeze the whole cohort with one signal before
    /// dropping any child guard. Stopping validators one by one leaves a live
    /// quorum long enough to certify a new block between process exits. A
    /// one-by-one stop is not a valid quiescent barrier for a test-only clock
    /// change.
    pub fn restart_committee_at_unix_time_offset(&mut self, offset_secs: i64) -> Result<()> {
        self.stop_committee_preserving_enclaves()?;
        let mut opts = self.start_opts.clone();
        opts.unix_time_offset_secs = Some(offset_secs);
        opts.genesis_timestamp_pre_shifted = true;
        self.start(&opts)
    }

    /// Point the test-only committee clock at a target consensus timestamp.
    /// Canonical time remains subject to the production drift band and may
    /// ratchet toward a distant target across multiple finalized blocks.
    pub(crate) fn restart_committee_at_consensus_timestamp(
        &mut self,
        target_timestamp: u64,
    ) -> Result<()> {
        let host_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| eyre::eyre!("host clock predates Unix epoch: {error}"))?
            .as_secs();
        let offset = i128::from(target_timestamp) - i128::from(host_timestamp);
        let offset = i64::try_from(offset)
            .map_err(|_| eyre::eyre!("test consensus clock offset exceeds i64"))?;
        self.restart_committee_at_unix_time_offset(offset)
    }

    /// Restart the complete validator cohort at one quiescent barrier while
    /// preserving datadirs, running enclaves and the current logical clock.
    pub fn restart_committee_preserving_enclaves(&mut self) -> Result<()> {
        self.stop_committee_preserving_enclaves()?;
        let opts = self.start_opts.clone();
        self.start(&opts)
    }

    fn stop_committee_preserving_enclaves(&mut self) -> Result<()> {
        let validator_pids = self
            .validators
            .values()
            .map(proc::ChildGuard::pid)
            .collect::<Vec<_>>();
        if validator_pids.len() != self.committee_size() {
            bail!(
                "committee clock restart requires {} live validators, found {}",
                self.committee_size(),
                validator_pids.len()
            );
        }
        quiesce_and_terminate_committee_with(&validator_pids, signal_committee)?;
        reap_stopped_committee(&mut self.validators)?;
        self.validators.clear();
        Ok(())
    }

    /// Relaunch one exact stopped committee validator while leaving every other
    /// stopped validator down. This is the bounded network-partition primitive
    /// used by the real tentative-candidate orphan scenario.
    pub fn restart_validator(&mut self, i: usize) -> Result<()> {
        if i >= self.committee_size() {
            bail!("validator index {i} is outside the committee");
        }
        if self
            .validators
            .get_mut(&i)
            .is_some_and(|guard| !guard.exited())
        {
            bail!("validator-{i} is already running");
        }
        self.validators.remove(&i);
        let opts = self.start_opts.clone();
        let bootnodes = self.bootnodes();
        if self.tee_enabled() && !self.enclaves.contains_key(&i) {
            let chain_id_hex = self.chain_id_hex()?;
            self.start_enclave(i, &chain_id_hex)?;
        }
        self.start_radicle(i)?;
        self.launch_validator(i, &opts, bootnodes.as_deref())?;
        sleep(Duration::from_secs(2));
        if self
            .validators
            .get_mut(&i)
            .is_some_and(|guard| guard.exited())
        {
            let tail = self.tail_log(i, 20);
            self.validators.remove(&i);
            bail!("validator-{i} exited during isolated restart:\n{tail}");
        }
        Ok(())
    }

    /// Stop and relaunch one committee validator while preserving its running
    /// enclave and persisted node data.
    pub fn restart_validator_preserving_enclave(&mut self, i: usize) -> Result<()> {
        ensure!(
            i < self.committee_size(),
            "restart target is outside the committee"
        );
        ensure!(
            !self.tee_enabled() || self.enclaves.contains_key(&i),
            "validator-{i} restart must preserve its owned enclave"
        );
        let argv = self
            .validator_argv
            .get(&i)
            .cloned()
            .ok_or_else(|| eyre::eyre!("validator-{i} has no captured launch argv"))?;
        let child = self
            .validators
            .get_mut(&i)
            .ok_or_else(|| eyre::eyre!("validator-{i} restart requires an owned child"))?;
        ensure!(
            child.exit_status()?.is_none(),
            "validator-{i} exited before restart fault"
        );
        // Stop only this owned node. A process-name backstop can kill a node
        // from another concurrently running scenario using the same slot.
        child.interrupt();
        let status = child
            .exit_status()?
            .ok_or_else(|| eyre::eyre!("validator-{i} did not stop"))?;
        ensure!(
            status.success(),
            "validator-{i} failed during planned restart: {status}"
        );
        self.validators.remove(&i);
        self.spawn_validator_with_argv(i, argv)?;
        sleep(Duration::from_secs(2));
        let child = self
            .validators
            .get_mut(&i)
            .ok_or_else(|| eyre::eyre!("validator-{i} restart lost its child"))?;
        ensure!(
            child.exit_status()?.is_none(),
            "validator-{i} exited during restart"
        );
        Ok(())
    }

    /// Stop and relaunch the entire committee, including every enclave, while
    /// preserving validator datadirs and sealed enclave state. This models an
    /// operator-level localnet stop/start rather than a single node restart.
    pub fn restart_committee_and_enclaves(&mut self) -> Result<()> {
        self.restart_committee_and_enclaves_observed(|_| Ok(()))
    }

    /// Arm restart observations after owned launchers exit and cleanup runs,
    /// before any replacement can append to the same logs.
    pub(crate) fn restart_committee_and_enclaves_observed(
        &mut self,
        before_launch: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        for validator in self.validators.values_mut() {
            let status = validator.stop_and_reap()?;
            ensure!(
                status.success(),
                "validator PID {} failed during committee restart: {status}",
                validator.pid()
            );
        }
        for enclave in self.enclaves.values_mut() {
            enclave.stop_and_reap()?;
        }
        self.validators.clear();
        self.enclaves.clear();
        before_launch(self)?;
        let opts = self.start_opts.clone();
        self.start(&opts)
    }

    /// Stop and relaunch one committee validator together with its enclave,
    /// preserving the node datadir and enclave seal while leaving every other
    /// committee member running.
    pub fn restart_validator_and_enclave(&mut self, i: usize) -> Result<()> {
        self.kill_validator(i)?;
        self.enclaves.remove(&i);
        self.restart()?;
        Ok(())
    }

    /// Fault the exact observed node and reap both owned children before arming
    /// replacement observations. Never use a process-name fallback for this fault.
    pub(crate) fn restart_validator_and_enclave_owned_observed(
        &mut self,
        index: usize,
        expected_node_pid: u32,
        expected_enclave_pid: u32,
        before_launch: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            self.live_validator_and_enclave_pids(index)?
                == (expected_node_pid, expected_enclave_pid),
            "validator-{index} restart target incarnation changed"
        );
        self.kill_validator_owned(index, expected_node_pid)?;
        self.enclaves
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned enclave"))?
            .stop_and_reap()?;
        self.enclaves.remove(&index);
        before_launch(self)?;
        self.restart()
    }

    /// Restart ONLY validator `i`'s enclave sidecar, preserving its sealed TEE
    /// state. The node keeps running. Its enclave session must reconnect (with
    /// identity re-validation) on the next request. A node restart is not
    /// required, and this verb is the first that proves it.
    pub(crate) fn restart_enclave_only(
        &mut self,
        i: usize,
    ) -> Result<crate::internal::launch_log::LaunchLog> {
        let log = self.stop_enclave_for_observation(i)?;
        let chain_id_hex = self.chain_id_hex()?;
        let seed = self.default_dkg_seed(i);
        self.start_enclave_with_seed(i, &chain_id_hex, seed)?;
        Ok(log)
    }

    /// Replace only the enclave, retaining its original seal for recovery. The
    /// replacement uses a separate, publicly initialized NodeHost fixture. The
    /// running node's persistent manifest and authorization are never changed.
    pub(crate) fn restart_enclave_with_fresh_identity(
        &mut self,
        i: usize,
    ) -> Result<crate::internal::launch_log::LaunchLog> {
        use alloy_signer::SignerSync as _;
        use alloy_signer_local::PrivateKeySigner;

        ensure!(
            self.cfg.tee_mode.passes_sgx_devices(),
            "identity substitution requires the production SGX NodeHost session"
        );
        let directory = self.cfg.validator_dir(i);
        let original = outbe_tee::load_committed_enclave_manifest_v1(&directory.join("data"))?;
        let backup = directory.join("tee-observability-original");
        let replacement_host = directory.join("tee-observability-replacement-host");
        ensure!(
            !backup.try_exists()? && !replacement_host.try_exists()?,
            "identity fault already armed"
        );
        let log = self.stop_enclave_for_observation(i)?;
        fs::rename(directory.join("tee"), &backup).wrap_err("preserve original enclave seal")?;
        let chain_id_hex = self.chain_id_hex()?;
        self.start_enclave_with_seed(i, &chain_id_hex, None)?;
        fs::create_dir(&replacement_host)?;
        let signer: PrivateKeySigner = fs::read_to_string(directory.join("reth-p2p-secret.hex"))?
            .trim()
            .parse()
            .map_err(|_| eyre::eyre!("invalid fixture P2P signer"))?;
        let endpoint = format!("127.0.0.1:{}", self.cfg.tee_port(i));
        let mut client = outbe_tee::connect_or_initialize_node_host_enclave(
            &endpoint,
            &replacement_host,
            outbe_tee::NodeHostIdentityV1 {
                network_binding: original.network_binding(),
                reth_p2p_public: original.node_id.reth_p2p_public,
            },
            |hash| {
                signer.sign_hash_sync(&hash).map(|signature| {
                    let mut bytes = [0_u8; 65];
                    bytes[..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
                    bytes[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
                    bytes[64] = u8::from(signature.v());
                    bytes
                })
            },
        )?;
        let replacement = outbe_tee::load_committed_enclave_manifest_v1(&replacement_host)?;
        ensure!(
            replacement.noise_responder_x25519 != original.noise_responder_x25519
                && replacement.attestation_ed25519 != original.attestation_ed25519
                && replacement.recipient_x25519 != original.recipient_x25519,
            "replacement enclave did not obtain a fresh public identity"
        );
        verify_pre_dkg_public_identity(
            client.request(&outbe_tee::protocol::EnclaveRequest::GetPublicKeys)?,
            &replacement,
        )?;
        Ok(log)
    }

    /// Restore the exact original sealed identity without restarting the node.
    pub(crate) fn restore_enclave_identity(
        &mut self,
        i: usize,
    ) -> Result<crate::internal::launch_log::LaunchLog> {
        let directory = self.cfg.validator_dir(i);
        ensure!(
            directory.join("tee-observability-original").is_dir(),
            "missing original enclave seal"
        );
        let log = self.stop_enclave_for_observation(i)?;
        fs::rename(
            directory.join("tee"),
            directory.join("tee-observability-rejected"),
        )?;
        fs::rename(
            directory.join("tee-observability-original"),
            directory.join("tee"),
        )?;
        let chain_id_hex = self.chain_id_hex()?;
        self.start_enclave_with_seed(i, &chain_id_hex, None)?;
        Ok(log)
    }

    /// Reap the exact owner before arming a log cursor for its replacement.
    fn stop_enclave_for_observation(
        &mut self,
        i: usize,
    ) -> Result<crate::internal::launch_log::LaunchLog> {
        self.live_validator_and_enclave_pids(i)?;
        self.enclaves
            .get_mut(&i)
            .ok_or_else(|| eyre::eyre!("missing enclave owner"))?
            .stop_and_reap()?;
        self.enclaves.remove(&i);
        crate::internal::launch_log::LaunchLog::checkpoint(
            &self.cfg.validator_dir(i).join("enclave.log"),
        )
    }

    /// Keep the validator process owned while its enclave is absent during a
    /// real encrypted-offer fault window.
    pub(crate) fn stop_validator_enclave_for_fault(&mut self, index: usize) -> Result<()> {
        self.stop_enclave_for_observation(index).map(|_| ())
    }

    pub(crate) fn restore_validator_enclave_after_fault(&mut self, index: usize) -> Result<()> {
        ensure!(
            !self.enclaves.contains_key(&index),
            "validator enclave is already owned"
        );
        let chain_id_hex = self.chain_id_hex()?;
        let seed = self.default_dkg_seed(index);
        self.start_enclave_with_seed(index, &chain_id_hex, seed)?;
        if !self.validator_running(index) {
            self.restart_validator(index)?;
        }
        Ok(())
    }

    /// Whether committee validator `i`'s node process is still running (its
    /// owned child guard exists and has not exited).
    pub fn validator_running(&mut self, i: usize) -> bool {
        self.validators
            .get_mut(&i)
            .is_some_and(|guard| !guard.exited())
    }

    /// Relaunch one validator against an alternate immutable chain manifest.
    ///
    /// The manifest must describe the same genesis header; this hook exists only
    /// for fork-install isolation evidence and does not inject state or alter the
    /// manifests used by the rest of the committee.
    pub fn restart_validator_with_chain_manifest(
        &mut self,
        i: usize,
        chain_manifest: PathBuf,
    ) -> Result<()> {
        if !chain_manifest.is_file() {
            bail!(
                "validator-{i} chain manifest does not exist: {}",
                chain_manifest.display()
            );
        }
        self.validator_chain_manifests.insert(i, chain_manifest);
        self.restart_validator_and_enclave(i)
    }

    /// Kill committee validator `i` so it stays down, leaving its enclave up (a
    /// later [`restart`] reconnects to it). Port of `e2e_kill_validator`.
    pub fn kill_validator(&mut self, i: usize) -> Result<()> {
        // Preserve graceful restart semantics and reap only this run's child.
        // Process-name matching can also kill the same slot in another network.
        self.validators
            .get_mut(&i)
            .ok_or_else(|| eyre::eyre!("validator-{i} has no owned stop target"))?
            .stop_and_reap()?;
        self.validators.remove(&i);
        Ok(())
    }

    /// Fault only the previously observed owned validator, leaving its enclave up.
    pub(crate) fn kill_validator_owned(
        &mut self,
        index: usize,
        expected_pid: u32,
    ) -> Result<std::process::ExitStatus> {
        let child = self
            .validators
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned fault target"))?;
        ensure!(
            child.pid() == expected_pid,
            "validator-{index} fault target incarnation changed"
        );
        let status = child.fault_and_reap()?;
        self.validators.remove(&index);
        Ok(status)
    }

    /// Launch one committee validator as an owned child:
    /// `--consensus.use-local-defaults`, signing-key + evm-key only (peers come
    /// from the on-chain ValidatorSet), no `run-supervised` wrapper.
    fn launch_validator(
        &mut self,
        i: usize,
        opts: &StartOpts,
        bootnodes: Option<&str>,
    ) -> Result<()> {
        let vd = self.cfg.validator_dir(i);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        // A prior SIGKILL can leave the MDBX lock behind. Clear it before relaunch.
        let _ = fs::remove_file(vd.join("data/db/lock"));

        let mut a = self.reth_base_args(&vd, i);
        if let Some(bn) = bootnodes.filter(|b| !b.is_empty()) {
            a.extend(args!["--bootnodes", bn]);
        }
        let secret = vd.join("reth-p2p-secret.hex");
        if secret.exists() {
            // File-based flag: the key must never appear in argv (`ps` leak).
            a.extend(args![
                "--p2p-secret-key",
                proc::normalized_secret_file(&secret)?.display()
            ]);
        }
        a.extend(args![
            "--validator",
            "--metrics",
            format!("0.0.0.0:{}", self.cfg.metrics_port(i)),
            "--consensus.signing-key",
            vd.join("signing-key.hex").display(),
            "--validator.evm-key",
            vd.join("evm-key.hex").display(),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(i)),
            "--consensus.use-local-defaults",
            "--radicle.control-socket",
            self.radicle_control_socket(i).display(),
            "--radicle.status-address",
            format!("127.0.0.1:{}", self.cfg.radicle_status_port(i)),
        ]);
        if opts.is_txpool_eviction_profile {
            // The dedicated pool-eviction scenario must converge in seconds,
            // while every ordinary localnet retains production pool policy.
            a.extend(args![
                "--txpool.outbe.pending-staleness-secs",
                "20",
                "--txpool.lifetime",
                "30s",
            ]);
        }
        if self.tee_enabled() {
            a.extend(args![
                "--tee-enclave-socket",
                format!("127.0.0.1:{}", self.cfg.tee_port(i)),
                // Fast canary cadence so observability scenarios converge in
                // seconds instead of the 30s production default.
                "--tee-canary.interval-secs",
                "5"
            ]);
            self.extend_real_sgx_startup_timeout(&mut a);
        }

        if let Some(offset) = opts.unix_time_offset_secs {
            a.push(unix_time_offset_arg(offset));
        }
        // The recovery follower is derived from an exact validator argv
        // snapshot. Its first subsequent validator launch consumes that
        // snapshot verbatim instead of reconstructing a merely equivalent
        // command from mutable harness inputs.
        a = restore_validator_argv(a, self.validator_recovery_original_argv.remove(&i));
        self.spawn_validator_with_argv(i, a)
    }

    fn spawn_validator_with_argv(&mut self, i: usize, argv: Vec<String>) -> Result<()> {
        let vd = self.cfg.validator_dir(i);
        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.env("RUST_MIN_STACK", "16777216");
        cmd.args(&argv);
        attach_log(&mut cmd, &vd)?;
        let guard = self.spawn_node(&format!("validator-{i}"), i, &vd, cmd)?;
        self.validator_argv.insert(i, argv);
        self.validators.insert(i, guard);
        Ok(())
    }

    /// Last `n` lines of validator `i`'s `node.log`.
    fn tail_log(&self, i: usize, n: usize) -> String {
        let s = fs::read_to_string(self.cfg.validator_dir(i).join("node.log")).unwrap_or_default();
        let lines: Vec<&str> = s.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

#[cfg(test)]
mod owned_committee_tests;

#[cfg(test)]
mod tests;
