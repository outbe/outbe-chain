//! Full-execution follower nodes (`--upstream`). Ported `launch_follower`.

use std::fs;
use std::process::Command;
use std::time::Duration;

use eyre::{bail, ensure, eyre, Result};

use crate::env::TeeMode;
use crate::internal::{
    eth,
    proc::{self, args, attach_log, random_hex_32, read_evm_key},
    shell::Sh,
};

use super::{committee::validator_protocol_environment, Localnet};

const VALIDATOR_RECOVERY_FOLLOWER_FORBIDDEN_ARGS: &[&str] = &[
    "--validator",
    "--consensus.signing-key",
    "--validator.evm-key",
    "--radicle.control-socket",
    "--radicle.status-address",
    "--upstream.nocertify",
];

const VALIDATOR_RECOVERY_FOLLOWER_STRIPPED_ARGS: &[(&str, bool)] = &[
    ("--validator", false),
    ("--consensus.signing-key", true),
    ("--validator.evm-key", true),
    ("--radicle.control-socket", true),
    ("--radicle.status-address", true),
];

fn ensure_validator_recovery_follower_args(args: &[String]) -> Result<()> {
    ensure!(
        args.iter()
            .any(|arg| arg == "--upstream" || arg.starts_with("--upstream=")),
        "validator recovery follower requires a certified --upstream"
    );
    if let Some(option) = args.iter().find(|arg| {
        VALIDATOR_RECOVERY_FOLLOWER_FORBIDDEN_ARGS
            .iter()
            .any(|forbidden| {
                arg.as_str() == *forbidden
                    || arg
                        .strip_prefix(forbidden)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
    }) {
        bail!("validator recovery follower command contains forbidden authority/bypass option {option}");
    }
    Ok(())
}

fn derive_validator_recovery_follower_args(
    original: &[String],
    certified_upstream: &str,
) -> Result<Vec<String>> {
    let mut follower = Vec::with_capacity(original.len() + 2);
    let mut index = 0;
    while index < original.len() {
        let arg = &original[index];
        if arg == "--upstream"
            || arg.starts_with("--upstream=")
            || arg == "--upstream.nocertify"
            || arg.starts_with("--upstream.nocertify=")
        {
            bail!("validator argv unexpectedly contains follower option {arg}");
        }

        let mut stripped = false;
        for (option, takes_value) in VALIDATOR_RECOVERY_FOLLOWER_STRIPPED_ARGS {
            if arg == option {
                if *takes_value {
                    ensure!(
                        index + 1 < original.len(),
                        "validator option {option} is missing its value"
                    );
                    index += 1;
                }
                stripped = true;
                break;
            }
            if arg
                .strip_prefix(option)
                .is_some_and(|suffix| suffix.starts_with('='))
            {
                stripped = true;
                break;
            }
        }
        if !stripped {
            follower.push(arg.clone());
        }
        index += 1;
    }
    follower.extend(args!["--upstream", certified_upstream]);
    ensure_validator_recovery_follower_args(&follower)?;
    Ok(follower)
}

impl Localnet {
    /// Provision a production full-node enclave with its persistent Reth and
    /// EVM identities. The global EVM key owns both the Registry association
    /// and the transaction envelope.
    pub fn provision_dcap_full_node(&mut self, index: usize) -> Result<()> {
        if !matches!(self.cfg.tee_mode, TeeMode::Real) {
            bail!("DcapRequired full-node provisioning requires --tee real");
        }
        self.provision_full_node_node_host(index)
    }

    /// Provision the production FullNode NodeHost path for any enabled TEE
    /// policy. DCAP and GramineDirectDev differ only in attestation authority;
    /// both require the same resident offer-key delivery before node startup.
    pub fn provision_full_node_node_host(&mut self, index: usize) -> Result<()> {
        let valid_until = eth::latest_block_timestamp(&self.cfg.rpc0)
            .ok_or_else(|| eyre!("cannot read canonical timestamp for full-node tee join"))?
            .checked_add(outbe_primitives::tee_genesis_v1::PRODUCTION_TEE_LEASE_SECONDS_V1)
            .ok_or_else(|| eyre!("full-node tee join lease deadline overflow"))?;
        self.provision_full_node_node_host_until(index, valid_until)
    }

    /// Provision a FullNode with an exact lease deadline. The lease E2E aligns
    /// it with the founding committee so one canonical timestamp exercises
    /// validator and FullNode expiry together.
    pub(crate) fn provision_full_node_node_host_until(
        &mut self,
        index: usize,
        valid_until: u64,
    ) -> Result<()> {
        let node_dir = self.cfg.validator_dir(index);
        let data_dir = node_dir.join("data");
        fs::create_dir_all(&data_dir)?;
        self.ensure_node_key_material(index)?;

        let reth_secret_path = node_dir.join("reth-p2p-secret.hex");
        if !reth_secret_path.is_file() {
            fs::write(&reth_secret_path, random_hex_32()?)?;
        }
        let evm_key = read_evm_key(&node_dir)?;
        let evm_address = eth::address_of(&evm_key)
            .ok_or_else(|| eyre!("provisioned full-node EVM key is invalid"))?;

        let funder = read_evm_key(&self.cfg.validator_dir(0))?;
        eth::send_value(&self.cfg.rpc0, evm_address, &funder, eth::coen(100))?;

        self.start_node_enclave(index)?;
        let mut join = args![
            "tee",
            "join",
            "--enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--reth-p2p-secret-key",
            reth_secret_path.display(),
            "--genesis",
            self.cfg.dir.join("genesis.json").display(),
            "--binding-id",
            random_hex_32()?,
            "--valid-until",
            valid_until,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            evm_key,
            "--timeout-secs",
            "180",
        ];
        if matches!(self.cfg.tee_mode, TeeMode::Real | TeeMode::SgxNoAttest) {
            join.extend(args!["--node-data-dir", data_dir.display()]);
        }
        Sh::new(&self.cfg).cli_required(join)?;
        Ok(())
    }

    /// Launch a DcapRequired full node against its own initialized enclave.
    /// Startup itself verifies the resident offer key against the selected
    /// upstream before opening networking or execution.
    pub fn launch_dcap_full_node(
        &mut self,
        name: &str,
        index: usize,
        upstream_slot: usize,
    ) -> Result<()> {
        let node_dir = self.cfg.validator_dir(index);
        fs::create_dir_all(node_dir.join("logs"))?;
        // File-based flag: the key must never appear in argv (`ps` leak).
        let p2p_secret_file = proc::normalized_secret_file(&node_dir.join("reth-p2p-secret.hex"))?;
        let mut args = self.reth_base_args(&node_dir, index);
        args.extend(args![
            "--p2p-secret-key",
            p2p_secret_file.display(),
            "--tee-enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--upstream",
            format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
        ]);
        self.extend_real_sgx_startup_timeout(&mut args);

        self.launch_certified_follower_with_args(name, index, args, Vec::new())
    }

    /// Launch the real certified-follower command while deliberately omitting
    /// the mandatory enclave endpoint. This is a negative acceptance probe:
    /// production startup must fail before RPC/network service is available.
    pub fn launch_enclave_less_follower(
        &mut self,
        name: &str,
        index: usize,
        upstream_slot: usize,
    ) -> Result<()> {
        ensure!(
            !self.followers.contains_key(name) && !self.follower_startup_probes.contains_key(name),
            "negative follower {name} is already owned"
        );
        let node_dir = self.cfg.validator_dir(index);
        fs::create_dir_all(node_dir.join("logs"))?;
        self.ensure_node_key_material(index)?;
        let reth_secret_path = node_dir.join("reth-p2p-secret.hex");
        if !reth_secret_path.is_file() {
            fs::write(&reth_secret_path, random_hex_32()?)?;
        }
        let p2p_secret_file = proc::normalized_secret_file(&reth_secret_path)?;
        let mut args = self.reth_base_args(&node_dir, index);
        args.extend(args![
            "--p2p-secret-key",
            p2p_secret_file.display(),
            "--upstream",
            format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
        ]);
        let probe = crate::internal::startup_rejection::StartupRejectionProbe::arm(
            &node_dir.join("node.log"),
            ([127, 0, 0, 1], self.cfg.http_port(index)).into(),
        )?;
        self.launch_certified_follower_with_args(name, index, args, Vec::new())?;
        self.follower_startup_probes
            .insert(name.to_owned(), (index, probe));
        Ok(())
    }

    /// Launch a production follower whose execution head advances only from its
    /// certified HTTP upstream and whose transaction pool has no P2P gossip
    /// path. This isolates the real pending-staleness policy from block
    /// production without mocking the node or its canonical-state stream.
    pub fn launch_isolated_txpool_follower(
        &mut self,
        name: &str,
        index: usize,
        upstream_slot: usize,
    ) -> Result<()> {
        ensure!(
            self.start_opts.is_txpool_eviction_profile,
            "isolated txpool follower requires the explicit eviction profile"
        );
        let node_dir = self.cfg.validator_dir(index);
        fs::create_dir_all(node_dir.join("logs"))?;
        let p2p_secret_file = proc::normalized_secret_file(&node_dir.join("reth-p2p-secret.hex"))?;
        let mut args = self.reth_base_args(&node_dir, index);
        args.extend(args![
            "--p2p-secret-key",
            p2p_secret_file.display(),
            "--disable-discovery",
            "--tee-enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--upstream",
            format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
            "--txpool.outbe.pending-staleness-secs",
            "20",
            "--txpool.lifetime",
            "30s",
        ]);
        self.extend_real_sgx_startup_timeout(&mut args);
        self.launch_certified_follower_with_args(name, index, args, Vec::new())
    }

    /// Require an owned negative-probe follower to terminate with the exact
    /// startup guardrail. Missing process ownership or a missing log is an
    /// error, never evidence that the guardrail worked.
    pub fn wait_for_follower_guardrail(
        &mut self,
        name: &str,
        index: usize,
        expected: &str,
        timeout: Duration,
    ) -> Result<()> {
        let (observed_index, probe) = self
            .follower_startup_probes
            .remove(name)
            .ok_or_else(|| eyre!("negative follower {name} has no pre-launch observer"))?;
        ensure!(
            observed_index == index,
            "negative follower observer belongs to a different node"
        );
        let child = self
            .followers
            .get_mut(name)
            .ok_or_else(|| eyre!("negative follower {name} is not owned by the scenario"))?;
        probe.wait(child, expected, timeout)?;
        self.followers.remove(name);
        Ok(())
    }

    fn launch_certified_follower_with_args(
        &mut self,
        name: &str,
        index: usize,
        args: Vec<String>,
        protocol_environment: Vec<(&'static str, String)>,
    ) -> Result<()> {
        let node_dir = self.cfg.validator_dir(index);
        ensure_validator_recovery_follower_args(&args)?;
        let mut command = Command::new(&self.cfg.bin_chain);
        command
            .env("RUST_MIN_STACK", "16777216")
            .env("RUST_LOG", "info,outbe_consensus=debug");
        for (name, value) in protocol_environment {
            command.env(name, value);
        }
        command.args(&args);
        attach_log(&mut command, &node_dir)?;
        let guard = self.spawn_node(name, index, &node_dir, command)?;
        self.followers.insert(name.to_owned(), guard);
        Ok(())
    }

    /// Start an excluded validator's existing datadir as the ordinary certified
    /// follower. The validator node and its Radicle signer are removed first so
    /// this phase has one database writer and no validator authority process.
    pub fn launch_validator_recovery_follower(
        &mut self,
        index: usize,
        upstream_slot: usize,
    ) -> Result<String> {
        if index >= self.committee_size() {
            bail!("validator index {index} is outside the committee");
        }
        if self
            .validators
            .get_mut(&index)
            .is_some_and(|guard| !guard.exited())
        {
            bail!("validator-{index} is still running; refusing a second datadir writer");
        }
        self.validators.remove(&index);
        self.radicle_sidecars.remove(&index);

        let name = Self::validator_recovery_follower_name(index);
        if self
            .followers
            .get_mut(&name)
            .is_some_and(|guard| !guard.exited())
        {
            bail!("{name} is already running");
        }
        self.followers.remove(&name);
        let original = self
            .validator_argv
            .get(&index)
            .cloned()
            .ok_or_else(|| eyre!("validator-{index} has no captured original argv"))?;
        let upstream = format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot));
        let follower_args = derive_validator_recovery_follower_args(&original, &upstream)?;
        self.validator_recovery_original_argv
            .entry(index)
            .or_insert(original);
        let protocol_environment = validator_protocol_environment(&self.start_opts);
        self.launch_certified_follower_with_args(
            &name,
            index,
            follower_args,
            protocol_environment,
        )?;
        Ok(name)
    }

    pub fn validator_recovery_follower_name(index: usize) -> String {
        format!("validator-{index}-recovery-follower")
    }

    pub fn follower_running(&mut self, name: &str) -> bool {
        self.followers
            .get_mut(name)
            .is_some_and(|guard| !guard.exited())
    }

    pub fn validator_radicle_sidecar_running(&mut self, index: usize) -> bool {
        self.radicle_sidecars
            .get_mut(&index)
            .is_some_and(|guard| !guard.exited())
    }

    /// Stop all follower nodes (drop owned handles -> kill + reap).
    pub fn stop_followers(&mut self) -> Result<()> {
        self.followers.clear();
        Ok(())
    }

    /// Stop one follower while preserving its durable datadir for restart/catch-up tests.
    pub fn stop_follower(&mut self, name: &str) -> Result<()> {
        if let Some(mut follower) = self.followers.remove(name) {
            follower.interrupt();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn validator_recovery_follower_requires_certified_non_authority_arguments() {
        let safe = vec![
            "node".to_owned(),
            "--datadir".to_owned(),
            "/srv/outbe/validator-3/data".to_owned(),
            "--upstream".to_owned(),
            "http://validator-0:8545".to_owned(),
            "--tee-enclave-socket".to_owned(),
            "127.0.0.1:7003".to_owned(),
        ];
        super::ensure_validator_recovery_follower_args(&safe)
            .expect("a certified authority-free follower command is valid");

        for forbidden in [
            "--validator",
            "--consensus.signing-key",
            "--validator.evm-key",
            "--radicle.control-socket",
            "--radicle.status-address",
            "--upstream.nocertify",
        ] {
            let mut unsafe_args = safe.clone();
            unsafe_args.push(forbidden.to_owned());
            assert!(
                super::ensure_validator_recovery_follower_args(&unsafe_args).is_err(),
                "recovery follower accepted authority/bypass option {forbidden}"
            );

            let mut inline_unsafe_args = safe.clone();
            inline_unsafe_args.push(format!("{forbidden}=value"));
            assert!(
                super::ensure_validator_recovery_follower_args(&inline_unsafe_args).is_err(),
                "recovery follower accepted inline authority/bypass option {forbidden}"
            );
        }

        let no_upstream = vec!["node".to_owned(), "--datadir".to_owned(), "data".to_owned()];
        assert!(super::ensure_validator_recovery_follower_args(&no_upstream).is_err());
    }

    #[test]
    fn validator_recovery_follower_is_derived_from_and_preserves_exact_validator_argv() {
        let original = vec![
            "node".to_owned(),
            "--chain".to_owned(),
            "/srv/outbe/genesis.json".to_owned(),
            "--datadir".to_owned(),
            "/srv/outbe/validator-3/data".to_owned(),
            "--bootnodes=enode://canonical".to_owned(),
            "--validator".to_owned(),
            "--consensus.signing-key".to_owned(),
            "/srv/outbe/validator-3/signing-key.hex".to_owned(),
            "--validator.evm-key=/srv/outbe/validator-3/evm-key.hex".to_owned(),
            "--consensus.listen-addr".to_owned(),
            "127.0.0.1:6103".to_owned(),
            "--consensus.use-local-defaults".to_owned(),
            "--radicle.control-socket".to_owned(),
            "/srv/outbe/validator-3/radicle.sock".to_owned(),
            "--radicle.status-address=127.0.0.1:6203".to_owned(),
            "--tee-canary.interval-secs".to_owned(),
            "5".to_owned(),
        ];
        let exact_restore = original.clone();

        let follower =
            super::derive_validator_recovery_follower_args(&original, "http://validator-0:8545")
                .expect("derive authority-free certified follower argv");

        assert_eq!(original, exact_restore, "derivation mutated original argv");
        assert!(follower.contains(&"--bootnodes=enode://canonical".to_owned()));
        assert!(follower.contains(&"--tee-canary.interval-secs".to_owned()));
        assert!(follower.contains(&"--consensus.use-local-defaults".to_owned()));
        assert!(follower.windows(2).any(|pair| {
            pair == [
                "--upstream".to_owned(),
                "http://validator-0:8545".to_owned(),
            ]
        }));
        super::ensure_validator_recovery_follower_args(&follower)
            .expect("derived follower argv must stay certified and authority-free");
        assert_eq!(
            exact_restore, original,
            "validator restart must retain byte-for-byte original argv"
        );
        assert_eq!(
            super::super::node_slot_projection_identity(3),
            "validator-3",
            "recovery follower must reuse the validator's durable projection identity"
        );
    }
}
