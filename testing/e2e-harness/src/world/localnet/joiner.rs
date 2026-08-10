//! A validator that joins a running localnet: provision (keygen, fund, register,
//! enclave, `tee join`) and launch at index = committee size. Ported
//! `e2e_provision_joiner` / `e2e_launch_joiner`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::{hex, Address, Bytes};
use eyre::{eyre, Result};
use outbe_primitives::tee_attestation_v1::NodeIdV1;
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

use crate::internal::{
    addresses,
    eth::{self, IValidatorSet},
    proc::{
        self, args, attach_log, first_hex, random_hex_32, read_evm_key, read_trimmed, wait_tcp,
        SealSpec,
    },
    shell::Sh,
};

use super::Localnet;

fn verifier_material_paths(run_dir: &Path) -> (PathBuf, PathBuf) {
    let active_keys = run_dir.join("validator-0/data/keys");
    (
        active_keys.join("dkg_polynomial.hex"),
        active_keys.join("dkg_output.hex"),
    )
}

fn full_node_joiner_role_args(
    p2p_secret: &str,
    tee_enclave_socket: &str,
    upstream: &str,
    consensus_listen_address: &str,
) -> Vec<String> {
    args![
        "--p2p-secret-key-hex",
        p2p_secret,
        "--tee-enclave-socket",
        tee_enclave_socket,
        "--upstream",
        upstream,
        "--consensus.listen-addr",
        consensus_listen_address,
    ]
}

impl Localnet {
    /// Launch the future joiner as a non-voting full-execution follower while
    /// preserving the validator slot's durable Reth datadir for promotion.
    /// The FullNode owns an enclave and NodeHost identity in its own slot. The
    /// process receives no validator BLS key and no OCOMP voting credentials.
    pub fn launch_joiner_full_node(
        &mut self,
        index: usize,
        upstream_slot: usize,
        ocomp_args: &[String],
    ) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        let secret_path = vd.join("reth-p2p-secret.hex");
        self.provision_full_node_node_host(index)?;
        let secret = read_trimmed(&secret_path)?;
        let mut process_args = self.reth_base_args(&vd, index);
        process_args.extend(full_node_joiner_role_args(
            &secret,
            &format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            &format!("http://localhost:{}", self.cfg.http_port(upstream_slot)),
            &format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
        ));
        process_args.extend_from_slice(ocomp_args);
        self.extend_real_sgx_startup_timeout(&mut process_args);

        let name = format!("joiner-full-node-{index}");
        let mut command = Command::new(&self.cfg.bin_chain);
        command
            .env("RUST_MIN_STACK", "16777216")
            .env("RUST_LOG", "info,outbe_consensus::follow=debug")
            .args(&process_args);
        attach_log(&mut command, &vd)?;
        let guard = self.spawn_node(&name, &vd, command)?;
        self.followers.insert(name, guard);
        Ok(())
    }

    /// Stop the non-voting phase without deleting its synchronized Reth data.
    pub fn stop_joiner_full_node(&mut self, index: usize) {
        self.followers.remove(&format!("joiner-full-node-{index}"));
    }

    /// Provision a joiner: keygen, fund, register, p2p, enclave, `tee join`
    /// (port of `e2e_provision_joiner`). Leaves keys under `validator-<index>/`.
    pub fn provision_joiner(&mut self, index: usize) -> Result<()> {
        self.provision_joiner_registration(index)?;
        self.join_validator_enclave(index)
    }

    /// Generate and register Validator/OCOMP identity without changing the
    /// currently running node or enclave profile.
    pub fn provision_joiner_registration(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(&vd)?;
        let signing_key = vd.join("signing-key.hex").display().to_string();

        // Fresh hybrid key material for the joiner.
        self.keygen(&["hybrid", "--output-dir", &vd.display().to_string()])?;
        let bls = first_hex(&self.keygen(&["show-pubkey", "--key", &signing_key])?, 96)
            .ok_or_else(|| eyre!("no BLS pubkey from keygen"))?;
        let key = read_evm_key(&vd)?;
        let addr = eth::address_of(&key).ok_or_else(|| eyre!("bad joiner evm key"))?;
        let chain_id = eth::raw_json(&self.cfg.rpc0, "eth_chainId")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("cannot read chain id for joiner OCOMP registration"))?;
        let genesis_hash = eth::raw_json_with_params(
            &self.cfg.rpc0,
            "eth_getBlockByNumber",
            serde_json::json!(["0x0", false]),
        )
        .and_then(|block| block.get("hash").cloned())
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or_else(|| eyre!("cannot read genesis hash for joiner OCOMP registration"))?;
        self.keygen(&[
            "ocomp",
            "--output-dir",
            &vd.display().to_string(),
            "--chain-id",
            &chain_id.to_string(),
            "--genesis-hash",
            &genesis_hash,
            "--validator-address",
            &format!("{addr:#x}"),
            "--consensus-bls-min-pk",
            &bls,
        ])?;
        let sig = first_hex(
            &self.keygen(&[
                "sign-registration",
                "--key",
                &signing_key,
                "--validator-address",
                &format!("{addr:#x}"),
            ])?,
            120,
        )
        .ok_or_else(|| eyre!("no registration signature from keygen"))?;
        let p2p_secret = vd.join("reth-p2p-secret.hex");
        if !p2p_secret.is_file() {
            fs::write(p2p_secret, random_hex_32()?)?;
        }

        // Fund from validator-0, prove that an unrelated EOA cannot register
        // this validator identity, then self-register and publish the P2P
        // address. The rejected call uses the joiner's otherwise-valid BLS
        // binding, isolating caller authorization from proof validation.
        let v0 = read_evm_key(&self.cfg.validator_dir(0))?;
        eth::send_value(&self.cfg.rpc0, addr, &v0, eth::coen(2000))?;
        let registration = IValidatorSet::registerValidatorCall {
            v: addr,
            pubkey: Bytes::from(hex::decode(&bls)?),
            sig: Bytes::from(hex::decode(&sig)?),
        };
        let unrelated = read_evm_key(&self.cfg.validator_dir(1))?;
        let unauthorized = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            &unrelated,
            &registration,
            None,
        );
        match unauthorized {
            Err(error) if error.to_string().contains("unauthorized") => {}
            Ok(tx) if eth::receipt_success(&self.cfg.rpc0, &tx) == Some(false) => {}
            Err(error) => {
                return Err(eyre!(
                    "unexpected unrelated-EOA registration error for {addr:#x}: {error}"
                ));
            }
            Ok(tx) => {
                return Err(eyre!(
                    "unrelated EOA unexpectedly registered joiner {addr:#x}: {tx}"
                ));
            }
        }
        let register_tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            &key,
            &registration,
            None,
        )?;
        if eth::receipt_success(&self.cfg.rpc0, &register_tx) != Some(true) {
            return Err(eyre!("joiner registration failed: {register_tx}"));
        }
        let p2p_tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            &key,
            &IValidatorSet::setP2pAddressCall {
                v: addr,
                kind: 1,
                addr: Bytes::from(hex::decode("00047f00000176c4")?),
            },
            None,
        )?;
        if eth::receipt_success(&self.cfg.rpc0, &p2p_tx) != Some(true) {
            return Err(eyre!("joiner P2P registration failed: {p2p_tx}"));
        }

        Ok(())
    }

    /// Start a fresh Validator-profile enclave and complete its on-chain TEE join.
    pub fn join_validator_enclave(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        let signing_key = vd.join("signing-key.hex").display().to_string();
        let bls = first_hex(&self.keygen(&["show-pubkey", "--key", &signing_key])?, 96)
            .ok_or_else(|| eyre!("no BLS pubkey from keygen"))?;
        let key = read_evm_key(&vd)?;
        self.start_node_enclave(index)?;
        let port = self.cfg.tee_port(index);
        let sock = format!("127.0.0.1:{port}");
        let binding_id = random_hex_32()?;
        let valid_until = eth::latest_block_timestamp(&self.cfg.rpc0)
            .ok_or_else(|| eyre!("cannot read canonical head timestamp for V1 tee join"))?
            .checked_add(7_200)
            .ok_or_else(|| eyre!("V1 tee join lease deadline overflow"))?
            .to_string();
        let mut join = args![
            "tee",
            "join",
            "--enclave-socket",
            sock,
            "--profile",
            "validator",
            "--consensus-bls-public",
            bls,
            "--binding-id",
            binding_id,
            "--valid-until",
            valid_until,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            key,
            "--timeout-secs",
            if matches!(self.cfg.tee_mode, crate::env::TeeMode::Real) {
                "180"
            } else {
                "60"
            },
        ];
        if matches!(
            self.cfg.tee_mode,
            crate::env::TeeMode::Real | crate::env::TeeMode::SgxNoAttest
        ) {
            join.extend(args!["--node-data-dir", vd.join("data").display()]);
        }
        Sh::new(&self.cfg).cli_required(join)?;
        Ok(())
    }

    /// Restart a joiner's enclave from its existing writable TEE directory.
    /// On hardware SGX this exercises EGETKEY unsealing rather than provisioning
    /// fresh join material.
    pub fn restart_joiner_enclave(&mut self, index: usize) -> Result<()> {
        self.enclaves.remove(&index);
        self.start_node_enclave(index)
    }

    pub fn stop_node_enclave(&mut self, index: usize) {
        self.enclaves.remove(&index);
    }

    /// Preserve the stopped FullNode's immutable local TEE identity before the
    /// same synchronized node slot is promoted to a Validator. Chain data and
    /// OCOMP results remain in place; only profile-bound identity is moved.
    pub fn archive_full_node_identity_for_validator_promotion(
        &mut self,
        index: usize,
    ) -> Result<()> {
        self.stop_node_enclave(index);

        let validator_dir = self.cfg.validator_dir(index);
        let archive_dir = validator_dir.join("full-node-identity-v1");
        let tee_dir = validator_dir.join("tee");
        let node_host_dir = validator_dir
            .join("data")
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
        let archived_tee_dir = archive_dir.join("tee");
        let archived_node_host_dir = archive_dir.join("tee-node-host-v1");

        if archive_dir.exists() {
            return Err(eyre!(
                "FullNode identity archive already exists: {}",
                archive_dir.display()
            ));
        }
        for path in [&tee_dir, &node_host_dir] {
            if !path.exists() {
                return Err(eyre!(
                    "cannot promote FullNode without its committed identity: {}",
                    path.display()
                ));
            }
        }

        fs::create_dir(&archive_dir)?;
        fs::rename(&tee_dir, &archived_tee_dir)?;
        fs::rename(&node_host_dir, &archived_node_host_dir)?;
        Ok(())
    }

    pub fn restart_full_node_enclave(&mut self, index: usize) -> Result<()> {
        self.enclaves.remove(&index);
        self.start_node_enclave(index)
    }

    /// Reopen the production enclave through its durable NodeHost identity and
    /// read the exact resident offer public key over authenticated Noise. The
    /// preceding `tee join` already required this key to match finalized chain
    /// state; this method proves that the same key remains reachable after the
    /// enclave and node restart without reopening the plaintext dev transport.
    pub fn node_offer_public(&self, index: usize) -> Result<[u8; 32]> {
        let node_data_dir = self.cfg.validator_dir(index).join("data");
        let manifest = outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir)
            .map_err(|error| eyre!("load joiner NodeHost manifest: {error}"))?;
        if manifest.chain_id[..24] != [0_u8; 24] {
            return Err(eyre!("node manifest chain id does not fit u64"));
        }
        let chain_id = u64::from_be_bytes(
            manifest.chain_id[24..]
                .try_into()
                .map_err(|_| eyre!("node manifest chain id has invalid width"))?,
        );
        let endpoint = format!("127.0.0.1:{}", self.cfg.tee_port(index));
        let unexpected_reinitialize =
            |_| Err("committed node unexpectedly required reinitialization".to_owned());
        let mut client = match manifest.node_id {
            NodeIdV1::Validator {
                address,
                bls_minpk_public,
            } => outbe_tee::connect_or_initialize_validator_enclave(
                &endpoint,
                &node_data_dir,
                outbe_tee::ValidatorNodeHostIdentityV1 {
                    chain_id,
                    genesis_hash: manifest.genesis_hash,
                    validator: Address::from(address),
                    consensus_bls_public: bls_minpk_public,
                },
                unexpected_reinitialize,
            ),
            NodeIdV1::FullNode { reth_p2p_public } => {
                outbe_tee::connect_or_initialize_full_node_enclave(
                    &endpoint,
                    &node_data_dir,
                    outbe_tee::FullNodeNodeHostIdentityV1 {
                        chain_id,
                        genesis_hash: manifest.genesis_hash,
                        reth_p2p_public,
                    },
                    unexpected_reinitialize,
                )
            }
        }
        .map_err(|error| eyre!("authenticated node enclave reopen failed: {error}"))?;
        match client.request(&EnclaveRequest::GetPublicKeys)? {
            EnclaveResponse::PublicKeys {
                offer_key_ready: true,
                recipient_x25519_pub,
                ..
            } => Ok(recipient_x25519_pub),
            other => Err(eyre!(
                "authenticated node enclave has no permanent offer key: {other:?}"
            )),
        }
    }

    pub(super) fn start_node_enclave(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        let port = self.cfg.tee_port(index);
        let image_id = proc::ensure_enclave_image(
            &self.cfg.repo,
            self.cfg.sudo,
            &self.cfg.dir.join("test-sgx-signing-key.pem"),
        )?;
        self.retain_enclave_image_id(image_id)?;
        let enclave_bin = if self.cfg.tee_mode.uses_mock_binary() {
            self.cfg.bin_mock.clone()
        } else {
            self.real_enclave_bin()?
        };
        let guard = proc::spawn_enclave(proc::EnclaveSpec {
            name: self.cfg.tee_container(index),
            tee_port: port,
            enclave_bin,
            signing_key: self.cfg.dir.join("test-sgx-signing-key.pem"),
            image_id: self
                .enclave_image_id
                .clone()
                .ok_or_else(|| eyre!("Gramine Docker image identity was not resolved"))?,
            sudo: self.cfg.sudo,
            pass_sgx_devices: self.cfg.tee_mode.passes_sgx_devices(),
            remote_attestation: match self.cfg.tee_mode {
                crate::env::TeeMode::Real => proc::TestRemoteAttestation::Dcap,
                crate::env::TeeMode::SgxNoAttest
                | crate::env::TeeMode::GramineDirect
                | crate::env::TeeMode::Mock => proc::TestRemoteAttestation::None,
            },
            dkg_seed: self
                .cfg
                .tee_mode
                .uses_deterministic_dkg_seed()
                .then(|| format!("{:064x}", index + 1)),
            seal: Some(SealSpec {
                tee_dir: vd.join("tee"),
                chain_id_hex: self.chain_id_hex()?,
            }),
            log_path: vd.join("enclave.log"),
            debug: self.cfg.debug,
        })?;
        self.enclaves.insert(index, guard);
        if !wait_tcp(port, 100) {
            self.enclaves.remove(&index);
            return Err(eyre!("enclave socket 127.0.0.1:{port} never came up"));
        }
        Ok(())
    }

    /// Launch the joiner node (validator-mode, verifier-join args), passing any
    /// extra node args (e.g. `--consensus.keys-dir ...`). Port of `e2e_launch_joiner`.
    pub fn launch_joiner(&mut self, index: usize, extra: &[&str]) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        let secret = read_trimmed(&vd.join("reth-p2p-secret.hex"))?;

        let (public_polynomial, dkg_output) = verifier_material_paths(&self.cfg.dir);
        let mut a = self.reth_base_args(&vd, index);
        a.extend(args![
            "--validator",
            "--bootnodes",
            self.bootnodes().unwrap_or_default(),
            "--p2p-secret-key-hex",
            secret,
            "--metrics",
            format!("0.0.0.0:{}", self.cfg.metrics_port(index)),
            "--consensus.signing-key",
            vd.join("signing-key.hex").display(),
            "--validator.evm-key",
            vd.join("evm-key.hex").display(),
            "--tee-renewal.relay-key",
            vd.join("evm-key.hex").display(),
            "--tee-renewal.rpc-url",
            format!("http://localhost:{}", self.cfg.http_port(index)),
            "--tee-renewal.poll-secs",
            "2",
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
            "--consensus.peers",
            self.consensus_peers()?,
            "--consensus.use-local-defaults",
            "--tee-enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--consensus.public-polynomial",
            public_polynomial.display(),
            "--consensus.dkg-output",
            dkg_output.display(),
        ]);
        self.extend_real_sgx_startup_timeout(&mut a);
        a.extend(extra.iter().map(|s| s.to_string()));

        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.env("RUST_MIN_STACK", "16777216").args(&a);
        attach_log(&mut cmd, &vd)?;
        let guard = self.spawn_node(&format!("validator-{index}"), &vd, cmd)?;
        self.validators.insert(index, guard);
        Ok(())
    }

    /// Stop the joiner node (drop its owned handle → kill + reap). Port of
    /// `e2e_stop_joiner`.
    pub fn stop_joiner(&mut self, index: usize) -> Result<()> {
        self.validators.remove(&index);
        Ok(())
    }

    /// `--consensus.peers` (`<public_key>@<p2p_address>,…`) from `validators.json`.
    fn consensus_peers(&self) -> Result<String> {
        let raw = fs::read_to_string(self.cfg.dir.join("validators.json"))?;
        let v: serde_json::Value = serde_json::from_str(&raw)?;
        let arr = v
            .as_array()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        let peers: Vec<String> = arr
            .iter()
            .filter_map(|e| {
                let pk = e.get("public_key")?.as_str()?;
                let addr = e.get("p2p_address")?.as_str()?;
                Some(format!("{pk}@{addr}"))
            })
            .collect();
        Ok(peers.join(","))
    }

    /// Run `outbe-keygen <args>` and return stdout.
    fn keygen(&self, args: &[&str]) -> Result<String> {
        proc::run_capture(&self.cfg.bin_keygen, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_node_joiner_role_has_no_validator_or_ocomp_vote_credentials() {
        let args = full_node_joiner_role_args(
            "p2p-secret",
            "127.0.0.1:34000",
            "http://127.0.0.1:35000",
            "127.0.0.1:36000",
        );

        for forbidden in [
            "--validator",
            "--consensus.signing-key",
            "--validator.evm-key",
            "--ocomp-key",
            "--ocomp-evm-key",
        ] {
            assert!(!args.iter().any(|arg| arg == forbidden), "{forbidden}");
        }
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--upstream", "http://127.0.0.1:35000"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--tee-enclave-socket", "127.0.0.1:34000"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--consensus.listen-addr", "127.0.0.1:36000"]));
    }

    #[test]
    fn verifier_join_uses_active_committee_material_not_bootstrap_fixture() {
        let root = Path::new("/run/scenario-1");
        let (polynomial, output) = verifier_material_paths(root);

        assert_eq!(
            polynomial,
            root.join("validator-0/data/keys/dkg_polynomial.hex")
        );
        assert_eq!(output, root.join("validator-0/data/keys/dkg_output.hex"));
    }
}
