//! The bootstrapped committee: start/stop/restart the validator set and its
//! enclaves (ported `run-testnet.sh` start), owned as Rust child processes.

use std::fs;
#[cfg(any(test, feature = "ocomp-integration"))]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

#[cfg(any(test, feature = "ocomp-integration"))]
use eyre::ensure;
use eyre::{bail, Result, WrapErr};

use crate::internal::proc::{self, args, attach_log, read_trimmed, wait_tcp, SealSpec};

use super::{Localnet, StartOpts};

fn unix_time_offset_arg(offset: i64) -> String {
    format!("--testnet.unix-time-offset-secs={offset}")
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

impl Localnet {
    /// Start the committee (and, when TEE is enabled, its enclaves). Idempotent:
    /// indices whose owned node is still alive are skipped, so [`restart`] only
    /// relaunches the ones that died.
    pub fn start(&mut self, opts: &StartOpts) -> Result<()> {
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

        // Bring the complete enclave cohort up before the first validator can
        // enter genesis DKG. Starting enclave→validator per index gave the first
        // nodes several seconds' head start and made them finalize before the
        // last dealings existed (`MissingPlayerDealing`).
        for &i in &launched {
            if self.tee_enabled() && !self.enclaves.contains_key(&i) {
                self.start_enclave(i, chain_id_hex.as_deref().unwrap_or_default())?;
            }
        }
        for &i in &launched {
            self.launch_validator(i, opts, bootnodes.as_deref())?;
        }

        // Survival check: a node that dies in the first couple seconds is a
        // config error — surface it with its log tail (`run-testnet.sh:386-407`).
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
    /// dropping any child guard: stopping validators one by one leaves a live
    /// quorum long enough to certify a new block between process exits, which
    /// is not a valid quiescent barrier for a test-only clock change.
    pub fn restart_committee_at_unix_time_offset(&mut self, offset_secs: i64) -> Result<()> {
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
        signal_committee(&validator_pids, "STOP")?;
        self.validators.clear();
        if !self.validators.is_empty() {
            bail!("committee stop barrier retained a validator process");
        }
        let mut opts = self.start_opts.clone();
        opts.unix_time_offset_secs = Some(offset_secs);
        opts.genesis_timestamp_pre_shifted = true;
        self.start(&opts)
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
        self.kill_validator(i)?;
        self.restart_validator(i)?;
        Ok(())
    }

    /// Stop and relaunch the entire committee, including every enclave, while
    /// preserving validator datadirs and sealed enclave state. This models an
    /// operator-level localnet stop/start rather than a single node restart.
    pub fn restart_committee_and_enclaves(&mut self) -> Result<()> {
        self.validators.clear();
        self.enclaves.clear();
        let opts = self.start_opts.clone();
        self.start(&opts)
    }

    /// Replace the node executable and relaunch the whole committee with the
    /// same datadirs, keys, ports, and enclave seals. This models the normal
    /// operator recovery after an old binary stops at a protocol activation.
    pub fn restart_committee_with_upgraded_binary(&mut self, version: &str) -> Result<()> {
        let upgraded = match self.cfg.bin_chain_upgraded.clone() {
            Some(path) => path,
            None => self.build_upgraded_binary(version)?,
        };
        if !upgraded.is_file() {
            bail!(
                "upgraded chain binary does not exist: {}",
                upgraded.display()
            );
        }
        self.cfg.bin_chain = upgraded;
        self.restart_committee_and_enclaves()
    }

    /// Build the replacement executable from the exact source revision under
    /// test, without modifying the developer's checkout. The temporary detached
    /// worktree receives only the workspace package-version bump; Cargo writes to
    /// a stable version-specific target so repeated update scenarios reuse heavy
    /// third-party artifacts.
    fn build_upgraded_binary(&self, version: &str) -> Result<PathBuf> {
        let cargo_version = cargo_package_version(version)?;
        let worktree = self.cfg.dir.join("upgrade-worktree");
        let target = self
            .cfg
            .repo
            .join("target/e2e-upgrades")
            .join(version.replace('.', "-"));
        let binary = target.join("release/outbe-chain");

        if worktree.exists() {
            let _ = Command::new("git")
                .current_dir(&self.cfg.repo)
                .args(["worktree", "remove", "--force"])
                .arg(&worktree)
                .output();
        }
        self.run_setup(
            Command::new("git")
                .current_dir(&self.cfg.repo)
                .args(["worktree", "add", "--detach"])
                .arg(&worktree)
                .arg("HEAD"),
            "create protocol-upgrade worktree",
        )?;

        let build_result = (|| -> Result<()> {
            let manifest = worktree.join("Cargo.toml");
            let source = fs::read_to_string(&manifest)?;
            fs::write(
                &manifest,
                rewrite_workspace_version(&source, &cargo_version)?,
            )?;

            self.run_setup(
                Command::new("cargo")
                    .current_dir(&worktree)
                    .args([
                        "build",
                        "--offline",
                        "--release",
                        "-p",
                        "outbe-chain",
                        "--bin",
                        "outbe-chain",
                    ])
                    .arg("--target-dir")
                    .arg(&target),
                &format!("build replacement outbe-chain v{version}"),
            )
        })();

        let cleanup = Command::new("git")
            .current_dir(&self.cfg.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&worktree)
            .output();
        build_result?;
        let cleanup = cleanup?;
        if !cleanup.status.success() {
            bail!(
                "remove protocol-upgrade worktree failed: {}",
                String::from_utf8_lossy(&cleanup.stderr)
            );
        }
        Ok(binary)
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
        // Dropping the owned guard sends SIGKILL and synchronously reaps the node.
        self.validators.remove(&i);
        // Backstop in case the owned handle was ever lost.
        let pat = format!("outbe-chain node.*validator-{i}/data");
        self.sh().sudo_best_effort("pkill", &["-9", "-f", &pat]);
        Ok(())
    }

    /// Rebuild one validator's derived CE database from its preserved canonical
    /// Reth history. Only the scenario-owned CE directory is removed, after the
    /// owned node has stopped; chain DB, keys, consensus state and OCOMP state
    /// remain untouched.
    #[cfg(feature = "ocomp-integration")]
    pub(super) fn restart_validator_after_ce_reset(&mut self, i: usize) -> Result<()> {
        verified_compressed_entities_reconstruction_path(&self.cfg.dir, self.committee_size(), i)?;
        self.kill_validator(i)?;
        let ce_dir = verified_compressed_entities_reconstruction_path(
            &self.cfg.dir,
            self.committee_size(),
            i,
        )?;
        fs::remove_dir_all(&ce_dir)
            .wrap_err_with(|| format!("remove test-owned CE database {}", ce_dir.display()))?;
        ensure!(
            !ce_dir.exists(),
            "test-owned CE database remains after removal: {}",
            ce_dir.display()
        );
        self.restart()
    }

    /// Launch one committee validator as an owned child (`run-testnet.sh:317-349`):
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
        // A prior SIGKILL can leave the MDBX lock behind; clear it before relaunch.
        let _ = fs::remove_file(vd.join("data/db/lock"));

        let mut a = self.reth_base_args(&vd, i);
        if let Some(bn) = bootnodes.filter(|b| !b.is_empty()) {
            a.extend(args!["--bootnodes", bn]);
        }
        let secret = vd.join("reth-p2p-secret.hex");
        if secret.exists() {
            a.extend(args!["--p2p-secret-key-hex", read_trimmed(&secret)?]);
        }
        a.extend(args![
            "--validator",
            "--metrics",
            format!("0.0.0.0:{}", self.cfg.metrics_port(i)),
            "--consensus.signing-key",
            vd.join("signing-key.hex").display(),
            "--validator.evm-key",
            vd.join("evm-key.hex").display(),
            "--tee-renewal.relay-key",
            vd.join("evm-key.hex").display(),
            "--tee-renewal.rpc-url",
            format!("http://localhost:{}", self.cfg.http_port(i)),
            "--tee-renewal.poll-secs",
            "2",
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(i)),
            "--consensus.use-local-defaults",
        ]);
        if self.tee_enabled() {
            a.extend(args![
                "--tee-enclave-socket",
                format!("127.0.0.1:{}", self.cfg.tee_port(i))
            ]);
            self.extend_real_sgx_startup_timeout(&mut a);
        }

        let binary = self
            .validator_binary_overrides
            .get(&i)
            .unwrap_or(&self.cfg.bin_chain);
        let mut cmd = Command::new(binary);
        cmd.env("RUST_MIN_STACK", "16777216");
        if let Some(environment) = self.validator_environment_overrides.get(&i) {
            cmd.envs(environment.iter().map(|(key, value)| (key, value)));
        }
        if let Some(w) = opts.voting_window {
            cmd.env("OUTBE_TEST_VOTING_WINDOW_BLOCKS", w.to_string());
        }
        if let Some(offset) = opts.unix_time_offset_secs {
            a.push(unix_time_offset_arg(offset));
        }
        cmd.args(&a);
        attach_log(&mut cmd, &vd)?;
        let guard = self.spawn_node(&format!("validator-{i}"), &vd, cmd)?;
        self.validators.insert(i, guard);
        Ok(())
    }

    /// Launch validator `i`'s enclave container (`run-testnet.sh:215-293`), owned
    /// and foreground (no `-d`), and wait for its socket.
    fn start_enclave(&mut self, i: usize, chain_id_hex: &str) -> Result<()> {
        let vd = self.cfg.validator_dir(i);
        fs::create_dir_all(&vd)?;
        let port = self.cfg.tee_port(i);
        let enclave_bin = if self.cfg.tee_mode.uses_mock_binary() {
            self.cfg.bin_mock.clone()
        } else {
            self.real_enclave_bin()?
        };
        // The harness always seals (localnet start sets OUTBE_TEE_SEAL); the host
        // dkg-seed is passed except for real+seal, where the enclave self-seals.
        let seal = Some(SealSpec {
            tee_dir: vd.join("tee"),
            chain_id_hex: chain_id_hex.to_string(),
        });
        let dkg_seed = self
            .cfg
            .tee_mode
            .uses_deterministic_dkg_seed()
            .then(|| format!("{:064x}", i + 1));

        let guard = proc::spawn_enclave(proc::EnclaveSpec {
            name: self.cfg.tee_container(i),
            tee_port: port,
            enclave_bin,
            signing_key: self.cfg.dir.join("test-sgx-signing-key.pem"),
            image_id: self
                .enclave_image_id
                .clone()
                .ok_or_else(|| eyre::eyre!("Gramine Docker image identity was not resolved"))?,
            sudo: self.cfg.sudo,
            pass_sgx_devices: self.cfg.tee_mode.passes_sgx_devices(),
            remote_attestation: match self.cfg.tee_mode {
                crate::env::TeeMode::Real => proc::TestRemoteAttestation::Dcap,
                crate::env::TeeMode::SgxNoAttest
                | crate::env::TeeMode::GramineDirect
                | crate::env::TeeMode::Mock => proc::TestRemoteAttestation::None,
            },
            dkg_seed,
            seal,
            log_path: vd.join("enclave.log"),
            debug: self.cfg.debug,
        })?;
        self.enclaves.insert(i, guard);
        if !wait_tcp(port, 200) {
            self.enclaves.remove(&i);
            bail!("enclave socket 127.0.0.1:{port} never came up for validator-{i}");
        }
        Ok(())
    }

    /// The genesis chain id as `0x`-padded 64-hex (enclave seal `--chain-id`).
    pub(super) fn chain_id_hex(&self) -> Result<String> {
        let g: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.cfg.dir.join("genesis.json"))?)?;
        let id = g
            .get("config")
            .and_then(|c| c.get("chainId"))
            .and_then(|x| x.as_u64())
            .ok_or_else(|| eyre::eyre!("no chainId in genesis.json"))?;
        Ok(format!("0x{id:064x}"))
    }

    /// Resolve the real (non-mock) enclave binary from the build tree.
    pub(super) fn real_enclave_bin(&self) -> Result<PathBuf> {
        if self.cfg.bin_enclave.exists() {
            return Ok(self.cfg.bin_enclave.clone());
        }
        Err(eyre::eyre!(
            "production enclave binary `outbe-tee-enclave` not found at {}",
            self.cfg.bin_enclave.display()
        ))
    }

    /// Last `n` lines of validator `i`'s `node.log`.
    fn tail_log(&self, i: usize, n: usize) -> String {
        let s = fs::read_to_string(self.cfg.validator_dir(i).join("node.log")).unwrap_or_default();
        let lines: Vec<&str> = s.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
    }
}

#[cfg(test)]
mod owned_committee_tests {
    use std::process::Command;
    use std::thread::sleep;
    use std::time::Duration;

    use crate::env::Environment;
    use crate::internal::config::Config;
    use crate::internal::proc::{ChildGuard, DockerImageId};

    use super::Localnet;

    #[test]
    fn readiness_rejects_an_owned_validator_that_exited() {
        let env = Environment {
            validators: 1,
            data_dir: tempfile::tempdir().unwrap().keep(),
            ..Environment::default()
        };
        env.ports.start_scenario(1).unwrap();
        let mut localnet = Localnet::new(Config::resolve(&env));

        let mut command = Command::new("sh");
        command.args(["-c", "exit 23"]);
        let guard = ChildGuard::spawn("validator-0-test", command).unwrap();
        localnet.validators.insert(0, guard);
        sleep(Duration::from_millis(50));

        let error = localnet
            .ensure_committee_alive()
            .expect_err("an exited owned validator must fail readiness");
        assert!(error.to_string().contains("validator-0 exited"));
    }

    #[test]
    fn pinned_enclave_image_is_not_resolved_again_on_committee_restart() {
        let env = Environment {
            validators: 1,
            data_dir: tempfile::tempdir().unwrap().keep(),
            ..Environment::default()
        };
        env.ports.start_scenario(1).unwrap();
        let mut localnet = Localnet::new(Config::resolve(&env));
        let mut resolutions = 0;
        let expected = format!("sha256:{}", "1".repeat(64));

        localnet
            .ensure_enclave_image_once_with(|| {
                resolutions += 1;
                DockerImageId::from_inspect_output(&expected)
            })
            .unwrap();
        localnet
            .ensure_enclave_image_once_with(|| {
                resolutions += 1;
                DockerImageId::from_inspect_output(&format!("sha256:{}", "2".repeat(64)))
            })
            .unwrap();

        assert_eq!(resolutions, 1);
        assert_eq!(localnet.enclave_image_id(), Some(expected.as_str()));
    }
}

fn rewrite_workspace_version(manifest: &str, version: &str) -> Result<String> {
    let section = manifest
        .find("[workspace.package]")
        .ok_or_else(|| eyre::eyre!("root Cargo.toml has no [workspace.package] section"))?;
    let section_end = manifest[section + 1..]
        .find("\n[")
        .map_or(manifest.len(), |offset| section + 1 + offset);
    let version_start = manifest[section..section_end]
        .find("\nversion = \"")
        .map(|offset| section + offset + "\nversion = \"".len())
        .ok_or_else(|| eyre::eyre!("[workspace.package] has no version field"))?;
    let version_end = manifest[version_start..]
        .find('"')
        .map(|offset| version_start + offset)
        .ok_or_else(|| eyre::eyre!("workspace package version is not quoted"))?;

    let mut rewritten = manifest.to_owned();
    rewritten.replace_range(version_start..version_end, version);
    Ok(rewritten)
}

fn cargo_package_version(protocol_version: &str) -> Result<String> {
    let mut components = protocol_version.split('.');
    let major = components.next().unwrap_or_default();
    let minor = components.next().unwrap_or_default();
    if major.is_empty() || minor.is_empty() || components.next().is_some() {
        bail!("protocol version must have major.minor form, got {protocol_version}");
    }
    Ok(format!("{major}.{minor}.0"))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn compressed_entities_reconstruction_path(
    scenario_dir: &Path,
    committee_size: usize,
    validator_index: usize,
) -> Result<PathBuf> {
    if !scenario_dir.is_absolute()
        || scenario_dir == Path::new("/")
        || validator_index >= committee_size
    {
        bail!(
            "refuse compressed-entity reconstruction outside one exact committee validator: \
             scenario={}, validator={validator_index}, committee={committee_size}",
            scenario_dir.display()
        );
    }
    let path = scenario_dir
        .join(format!("validator-{validator_index}"))
        .join("data")
        .join("compressed_entities");
    if !path.starts_with(scenario_dir)
        || path.file_name().and_then(|name| name.to_str()) != Some("compressed_entities")
    {
        bail!("compressed-entity reconstruction path escaped its scenario owner");
    }
    Ok(path)
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn verified_compressed_entities_reconstruction_path(
    scenario_dir: &Path,
    committee_size: usize,
    validator_index: usize,
) -> Result<PathBuf> {
    let path =
        compressed_entities_reconstruction_path(scenario_dir, committee_size, validator_index)?;
    let validator_dir = scenario_dir.join(format!("validator-{validator_index}"));
    let data_dir = validator_dir.join("data");
    for component in [
        scenario_dir,
        validator_dir.as_path(),
        data_dir.as_path(),
        path.as_path(),
    ] {
        let metadata = fs::symlink_metadata(component)
            .wrap_err_with(|| format!("inspect CE reconstruction owner {}", component.display()))?;
        ensure!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "CE reconstruction owner is not one real directory: {}",
            component.display()
        );
    }
    let canonical_scenario = fs::canonicalize(scenario_dir)
        .wrap_err_with(|| format!("canonicalize scenario owner {}", scenario_dir.display()))?;
    let canonical_target = fs::canonicalize(&path)
        .wrap_err_with(|| format!("canonicalize CE reconstruction target {}", path.display()))?;
    let expected = canonical_scenario
        .join(format!("validator-{validator_index}"))
        .join("data")
        .join("compressed_entities");
    ensure!(
        canonical_target == expected && canonical_target.starts_with(&canonical_scenario),
        "CE reconstruction target escaped its canonical scenario owner: target={}, owner={}",
        canonical_target.display(),
        canonical_scenario.display()
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{
        cargo_package_version, committee_signal_args, compressed_entities_reconstruction_path,
        rewrite_workspace_version, unix_time_offset_arg,
        verified_compressed_entities_reconstruction_path,
    };
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct ClockOffsetArgs {
        #[arg(long = "testnet.unix-time-offset-secs")]
        unix_time_offset_secs: Option<i64>,
    }

    #[test]
    fn negative_clock_offset_is_one_clap_parseable_argument() {
        let offset_arg = unix_time_offset_arg(-3);
        let parsed = ClockOffsetArgs::try_parse_from(["outbe-chain", offset_arg.as_str()])
            .expect("negative clock offset must remain the option value");

        assert_eq!(parsed.unix_time_offset_secs, Some(-3));
    }

    #[test]
    fn committee_freeze_is_one_noninteractive_signal_for_the_complete_cohort() {
        assert_eq!(
            committee_signal_args(&[101, 202, 303, 404], "STOP"),
            ["-STOP", "--", "101", "202", "303", "404"]
        );
    }

    #[test]
    fn expands_protocol_version_to_cargo_semver() {
        assert_eq!(cargo_package_version("3.0").unwrap(), "3.0.0");
        assert!(cargo_package_version("3").is_err());
        assert!(cargo_package_version("3.0.1").is_err());
    }

    #[test]
    fn rewrites_only_the_workspace_package_version() {
        let manifest = r#"[workspace]
members = []

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
example = { version = "9.9.9" }
"#;

        let rewritten = rewrite_workspace_version(manifest, "3.0.0").unwrap();
        assert!(rewritten.contains("[workspace.package]\nversion = \"3.0.0\""));
        assert!(rewritten.contains("example = { version = \"9.9.9\" }"));
    }

    #[test]
    fn ce_reconstruction_path_is_exact_and_committee_bounded() {
        let scenario = std::path::Path::new("/tmp/outbe-e2e/scenario-1");
        assert_eq!(
            compressed_entities_reconstruction_path(scenario, 4, 2).unwrap(),
            scenario.join("validator-2/data/compressed_entities")
        );
        assert!(compressed_entities_reconstruction_path(scenario, 4, 4).is_err());
        assert!(compressed_entities_reconstruction_path(std::path::Path::new("/"), 4, 0).is_err());
    }

    #[test]
    fn ce_reconstruction_target_rejects_an_intermediate_symlink() {
        let root = tempfile::tempdir().unwrap();
        let scenario = root.path().join("scenario-1");
        let outside = root.path().join("outside-validator");
        std::fs::create_dir_all(outside.join("data/compressed_entities")).unwrap();
        std::fs::create_dir_all(&scenario).unwrap();
        std::os::unix::fs::symlink(&outside, scenario.join("validator-0")).unwrap();

        assert!(
            verified_compressed_entities_reconstruction_path(&scenario, 4, 0).is_err(),
            "an intermediate symlink must never authorize recursive CE deletion"
        );

        std::fs::remove_file(scenario.join("validator-0")).unwrap();
        std::fs::create_dir_all(scenario.join("validator-0/data/compressed_entities")).unwrap();
        assert_eq!(
            verified_compressed_entities_reconstruction_path(&scenario, 4, 0).unwrap(),
            scenario.join("validator-0/data/compressed_entities")
        );
    }
}
