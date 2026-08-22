//! Full-execution follower nodes (`--upstream`). Ported `launch_follower`.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use eyre::{bail, eyre, Result};

use crate::env::TeeMode;
use crate::internal::{
    eth,
    proc::{self, args, attach_log, random_hex_32, read_evm_key},
    shell::Sh,
};

use super::Localnet;

impl Localnet {
    /// Provision a production full-node enclave with its own persistent Reth
    /// identity and a distinct funded relay signer. The relay submits the
    /// Registry transaction but never authorizes the NodeHost manifest.
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
        let node_dir = self.cfg.validator_dir(index);
        let data_dir = node_dir.join("data");
        fs::create_dir_all(&data_dir)?;
        self.ensure_node_key_material(index)?;

        let reth_secret_path = node_dir.join("reth-p2p-secret.hex");
        if !reth_secret_path.is_file() {
            fs::write(&reth_secret_path, random_hex_32()?)?;
        }
        let relay_key = random_hex_32()?;
        let relay_address = eth::address_of(&relay_key)
            .ok_or_else(|| eyre!("generated full-node relay key is invalid"))?;
        let relay_key_path = node_dir.join("relay-evm-key.hex");
        fs::write(&relay_key_path, &relay_key)?;
        fs::set_permissions(&relay_key_path, fs::Permissions::from_mode(0o600))?;

        let funder = read_evm_key(&self.cfg.validator_dir(0))?;
        eth::send_value(&self.cfg.rpc0, relay_address, &funder, eth::coen(100))?;

        self.start_node_enclave(index)?;
        let valid_until = eth::latest_block_timestamp(&self.cfg.rpc0)
            .ok_or_else(|| eyre!("cannot read canonical timestamp for full-node tee join"))?
            .checked_add(7_200)
            .ok_or_else(|| eyre!("full-node tee join lease deadline overflow"))?;
        let mut join = args![
            "tee",
            "join",
            "--enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--reth-p2p-secret-key",
            reth_secret_path.display(),
            "--node-evm-key",
            node_dir.join("evm-key.hex").display(),
            "--binding-id",
            random_hex_32()?,
            "--valid-until",
            valid_until,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            relay_key,
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
    /// upstream before opening networking or execution. Renewal uses that same
    /// reachable upstream as its transaction relay; the harness follower has no
    /// independent P2P route into the committee mempool.
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
            "--tee-renewal.relay-key",
            node_dir.join("relay-evm-key.hex").display(),
            "--tee-renewal.rpc-url",
            format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            "--tee-renewal.poll-secs",
            "2",
            "--upstream",
            format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
        ]);
        self.extend_real_sgx_startup_timeout(&mut args);

        let mut command = Command::new(&self.cfg.bin_chain);
        command
            .env("RUST_MIN_STACK", "16777216")
            .env("RUST_LOG", "info,outbe_consensus::follow=debug")
            .args(&args);
        attach_log(&mut command, &node_dir)?;
        let guard = self.spawn_node(name, &node_dir, command)?;
        self.followers.insert(name.to_owned(), guard);
        Ok(())
    }

    /// Stop all follower nodes (drop owned handles → kill + reap).
    pub fn stop_followers(&mut self) -> Result<()> {
        self.followers.clear();
        Ok(())
    }

    /// Stop one follower while preserving its durable datadir for restart/catch-up tests.
    pub fn stop_follower(&mut self, name: &str) -> Result<()> {
        self.followers.remove(name);
        Ok(())
    }
}
