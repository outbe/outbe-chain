//! A validator that joins a running localnet: provision (keygen, fund, register,
//! enclave, `tee join`) and launch at index = committee size. Ported
//! `e2e_provision_joiner` / `e2e_launch_joiner`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::{hex, Address, Bytes, B256};
use eyre::{eyre, Result};
use outbe_tee::TransportError;
use serde::Deserialize;

use crate::internal::{
    addresses,
    eth::{self, IValidatorSet},
    proc::{self, args, attach_log, first_hex, random_hex_32, read_evm_key, read_trimmed},
};
use crate::world::rpc::TxOutcome;
use crate::world::validators::{RadicleKeys, RegistrationIdentity, RegistrationKeys};

use super::Localnet;

mod cold_catchup;
mod enclave;

const REAL_SGX_OFFER_READ_ATTEMPTS: usize = 3;

fn registration_outcome(url: &str, transaction_hash: &str) -> Result<TxOutcome> {
    let receipt = eth::raw_json_result(
        url,
        "eth_getTransactionReceipt",
        serde_json::json!([transaction_hash]),
    )?;
    registration_receipt(transaction_hash, receipt)
}

fn registration_receipt(transaction_hash: &str, receipt: serde_json::Value) -> Result<TxOutcome> {
    let expected: B256 = transaction_hash.parse()?;
    let actual: B256 = serde_json::from_value(receipt["transactionHash"].clone())?;
    eyre::ensure!(
        actual == expected,
        "registration receipt belongs to a different transaction"
    );
    eyre::ensure!(
        receipt["status"].as_str() == Some("0x1"),
        "registration receipt is not successful"
    );
    let outcome = TxOutcome {
        transaction_hash: transaction_hash.to_owned(),
        success: true,
        receipt,
    };
    eyre::ensure!(
        outcome.block_number().is_some(),
        "registration receipt omitted block number"
    );
    outcome.block_hash()?;
    Ok(outcome)
}

const PRODUCTION_NO_ATTEST_REJECTION: &str = "production DCAP release refuses runtime attestation \
none (gramine-sgx; remote attestation disabled - EGETKEY sealing available)";

fn has_exact_production_no_attest_rejection(log: &str) -> bool {
    log.contains(PRODUCTION_NO_ATTEST_REJECTION)
}

fn retry_node_offer_read(real_sgx: bool, attempt: usize, error: &TransportError) -> bool {
    real_sgx
        && attempt.saturating_add(1) < REAL_SGX_OFFER_READ_ATTEMPTS
        && matches!(error, TransportError::IoTimeout { .. })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ManualRenewalObservationV1 {
    pub renewal_nonce: u64,
    pub valid_until: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualRenewalStatusObservationV1 {
    pub finalized_height: u64,
    pub finalized_timestamp: u64,
    pub valid_until: u64,
    pub journal_state: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManualRenewalCliOutcomeV1 {
    Finalized,
    NotDue,
    Unexpected,
}

fn classify_manual_renewal_cli_output(output: &str) -> ManualRenewalCliOutcomeV1 {
    let output = output.trim_start();
    if output.starts_with("Finalized {") {
        ManualRenewalCliOutcomeV1::Finalized
    } else if output.starts_with("NotDue {") {
        ManualRenewalCliOutcomeV1::NotDue
    } else {
        ManualRenewalCliOutcomeV1::Unexpected
    }
}

fn verifier_material_paths(run_dir: &Path) -> (PathBuf, PathBuf) {
    let active_keys = run_dir.join("validator-0/data/keys");
    (
        active_keys.join("dkg_polynomial.hex"),
        active_keys.join("dkg_output.hex"),
    )
}

fn full_node_joiner_role_args(
    p2p_secret_file: &str,
    tee_enclave_socket: &str,
    upstream: &str,
    consensus_listen_address: &str,
) -> Vec<String> {
    args![
        "--p2p-secret-key",
        p2p_secret_file,
        "--tee-enclave-socket",
        tee_enclave_socket,
        "--upstream",
        upstream,
        "--consensus.listen-addr",
        consensus_listen_address,
    ]
}

impl Localnet {
    /// Canonical ownership key for the role-neutral FullNode process. Launch,
    /// stop, and exit probes must all use this exact identity.
    pub fn joiner_full_node_name(index: usize) -> String {
        format!("joiner-full-node-{index}")
    }

    /// Launch a node in non-voting full-execution mode while preserving its
    /// durable Reth datadir and role-neutral NodeHost identity.
    /// The FullNode owns an enclave and NodeHost identity in its own slot. The
    /// process receives no validator BLS key and no validator signing credentials.
    pub fn launch_joiner_full_node(&mut self, index: usize, upstream_slot: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        let secret_path = vd.join("reth-p2p-secret.hex");
        self.provision_full_node_node_host(index)?;
        self.wait_selected_upstream_admission(index, upstream_slot)?;
        // File-based flag: the key must never appear in argv (`ps` leak).
        let secret_file = proc::normalized_secret_file(&secret_path)?;
        let mut process_args = self.reth_base_args(&vd, index);
        process_args.extend(full_node_joiner_role_args(
            &secret_file.display().to_string(),
            &format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            &format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            &format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
        ));
        self.extend_real_sgx_startup_timeout(&mut process_args);

        let name = Self::joiner_full_node_name(index);
        let mut command = Command::new(&self.cfg.bin_chain);
        command
            .env("RUST_MIN_STACK", "16777216")
            .env("RUST_LOG", "info,outbe_consensus::follow=debug")
            .args(&process_args);
        attach_log(&mut command, &vd)?;
        let guard = self.spawn_node(&name, index, &vd, command)?;
        self.followers.insert(name, guard);
        Ok(())
    }

    /// Stop the non-voting phase without deleting its synchronized Reth data.
    pub fn stop_joiner_full_node(&mut self, index: usize) {
        self.followers.remove(&Self::joiner_full_node_name(index));
    }

    /// Stop only the live FullNode incarnation observed by the scenario, and
    /// retain its actual exit status before permitting a replacement role.
    pub(crate) fn stop_joiner_full_node_owned(
        &mut self,
        index: usize,
        expected_pid: u32,
    ) -> Result<std::process::ExitStatus> {
        let (pid, status) = self.owned_full_node_process(index)?;
        eyre::ensure!(pid == expected_pid, "FullNode {index} incarnation changed");
        eyre::ensure!(
            status.is_none(),
            "FullNode {index} already exited: {status:?}"
        );
        let name = Self::joiner_full_node_name(index);
        let status = self
            .followers
            .get_mut(&name)
            .ok_or_else(|| eyre!("FullNode {index} owner disappeared"))?
            .stop_and_reap()?;
        self.followers.remove(&name);
        eyre::ensure!(
            status.success(),
            "FullNode {index} PID {expected_pid} failed during planned stop: {status}"
        );
        Ok(status)
    }

    /// Exit state for an owned role-neutral FullNode. `None` means the process
    /// was never registered under the canonical key and is not exit evidence.
    pub fn joiner_full_node_exit_status(&mut self, index: usize) -> Option<bool> {
        self.followers
            .get_mut(&Self::joiner_full_node_name(index))
            .map(crate::internal::proc::ChildGuard::exited)
    }

    /// Observe the exact owned FullNode before replacement or teardown. A
    /// missing owner and an observation error are not evidence of an exit.
    pub(crate) fn owned_full_node_process(
        &mut self,
        index: usize,
    ) -> Result<(u32, Option<std::process::ExitStatus>)> {
        let child = self
            .followers
            .get_mut(&Self::joiner_full_node_name(index))
            .ok_or_else(|| eyre!("FullNode {index} is not owned"))?;
        Ok((child.pid(), child.exit_status()?))
    }

    /// Whether the owned non-voting FullNode process has exited.
    pub fn joiner_full_node_exited(&mut self, index: usize) -> bool {
        self.joiner_full_node_exit_status(index).unwrap_or(true)
    }

    /// Generate a reusable EOA + individual MinPk BLS identity and its exact
    /// chain-and-address-bound registration PoP.
    ///
    /// Generation happens in an ephemeral staging directory. The returned
    /// value owns the key/proof bytes, so it remains usable after
    /// [`Localnet::rebootstrap_with_profile`] removes the first chain's files.
    pub fn prepare_registration_identity(
        &self,
        suggested_index: usize,
    ) -> Result<RegistrationIdentity> {
        self.prepare_registration_identity_parts(suggested_index, None, None)
    }

    /// Generate a fresh BLS key and PoP while retaining an existing identity's
    /// EOA. Re-registration tests use this for "same validator, new BLS key".
    pub fn rotate_registration_bls(
        &self,
        suggested_index: usize,
        eoa_source: &RegistrationIdentity,
    ) -> Result<RegistrationIdentity> {
        self.prepare_registration_identity_parts(suggested_index, Some(eoa_source), None)
    }

    /// Bind an existing BLS private key to another prepared EOA and generate
    /// the corresponding chain-and-address-bound PoP. Duplicate-key and old-key reuse
    /// tests use this without exposing the BLS secret through the public API.
    pub fn rebind_registration_bls(
        &self,
        suggested_index: usize,
        eoa_source: &RegistrationIdentity,
        bls_source: &RegistrationIdentity,
    ) -> Result<RegistrationIdentity> {
        self.prepare_registration_identity_parts(
            suggested_index,
            Some(eoa_source),
            Some(bls_source),
        )
    }

    fn prepare_registration_identity_parts(
        &self,
        suggested_index: usize,
        eoa_source: Option<&RegistrationIdentity>,
        bls_source: Option<&RegistrationIdentity>,
    ) -> Result<RegistrationIdentity> {
        fs::create_dir_all(&self.cfg.dir)?;
        let nonce = random_hex_32()?;
        let staging = self.cfg.dir.join(format!(
            ".identity-stage-{suggested_index}-{}",
            &nonce[..16]
        ));
        if staging.exists() {
            return Err(eyre!(
                "identity staging directory already exists: {}",
                staging.display()
            ));
        }

        let result = (|| -> Result<RegistrationIdentity> {
            match (eoa_source, bls_source) {
                (None, None) => {
                    self.keygen(&["hybrid", "--output-dir", &staging.display().to_string()])?;
                }
                (_, None) => {
                    self.keygen(&["generate", "--output-dir", &staging.display().to_string()])?;
                }
                (_, Some(source)) => {
                    source.install_at(&staging)?;
                }
            }
            let signing_key = staging.join("signing-key.hex");
            let signing_key_arg = signing_key.display().to_string();
            let bls = first_hex(
                &self.keygen(&["show-pubkey", "--key", &signing_key_arg])?,
                96,
            )
            .ok_or_else(|| eyre!("no BLS pubkey from keygen"))?;
            let evm_key = match eoa_source {
                Some(source) => source.evm_key().to_owned(),
                None => read_evm_key(&staging)?,
            };
            let address =
                eth::address_of(&evm_key).ok_or_else(|| eyre!("bad generated EVM key"))?;
            let (radicle_node_id, radicle_secret_key, radicle_public_key) =
                if let Some(source) = eoa_source {
                    (
                        source.radicle_node_id(),
                        source.radicle_secret_key().to_owned(),
                        source.radicle_public_key().to_owned(),
                    )
                } else {
                    let radicle_home = staging.join("radicle");
                    let output = self.keygen(&[
                        "radicle",
                        "--output-dir",
                        &radicle_home.display().to_string(),
                    ])?;
                    let node_id = first_hex(&output, 64)
                        .ok_or_else(|| eyre!("no Radicle NodeId from keygen"))?;
                    (
                        B256::from_slice(&hex::decode(node_id)?),
                        fs::read_to_string(radicle_home.join("keys/radicle"))?,
                        fs::read_to_string(radicle_home.join("keys/radicle.pub"))?,
                    )
                };
            let signature = self.sign_registration(&signing_key_arg, address, radicle_node_id)?;
            let bls_private_key = read_trimmed(&signing_key)?;

            let bls_public_key = Bytes::from(hex::decode(bls)?);
            let registration_signature = Bytes::from(hex::decode(signature)?);
            Ok(RegistrationIdentity::new(
                address,
                RegistrationKeys {
                    evm_private_key: evm_key,
                    bls_private_key,
                    bls_public_key,
                    reth_p2p_secret: random_hex_32()?,
                },
                RadicleKeys {
                    node_id: radicle_node_id,
                    secret_key: radicle_secret_key,
                    public_key: radicle_public_key,
                },
                registration_signature,
            ))
        })();
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// Materialize a prepared identity for a joiner node. This method never
    /// overwrites existing key files.
    pub fn install_registration_identity(
        &self,
        index: usize,
        identity: &RegistrationIdentity,
    ) -> Result<()> {
        identity.install_at(&self.cfg.validator_dir(index))
    }

    pub fn installed_registration_eoa_source(
        &self,
        index: usize,
        address: Address,
        evm_key: String,
        radicle_node_id: B256,
    ) -> Result<RegistrationIdentity> {
        let validator_dir = self.cfg.validator_dir(index);
        let radicle = RadicleKeys {
            node_id: radicle_node_id,
            secret_key: fs::read_to_string(validator_dir.join("radicle/keys/radicle"))?,
            public_key: fs::read_to_string(validator_dir.join("radicle/keys/radicle.pub"))?,
        };
        let keys = RegistrationKeys {
            evm_private_key: evm_key,
            bls_private_key: String::new(),
            bls_public_key: Bytes::new(),
            reth_p2p_secret: read_trimmed(&validator_dir.join("reth-p2p-secret.hex"))?,
        };
        Ok(RegistrationIdentity::new(
            address,
            keys,
            radicle,
            Bytes::new(),
        ))
    }

    /// Provision a joiner: keygen, fund, register, p2p, enclave, `tee join`
    /// (port of `e2e_provision_joiner`). Leaves keys under `validator-<index>/`.
    pub fn provision_joiner(&mut self, index: usize) -> Result<()> {
        self.provision_existing_node_as_joiner(index)?;
        self.join_node_enclave(index)
    }

    /// Add ValidatorSet identity to an already joined role-neutral
    /// NodeHost. This deliberately does not perform a second TEE join.
    pub fn provision_existing_node_as_joiner(&mut self, index: usize) -> Result<()> {
        self.provision_existing_node_as_joiner_observed(index)
            .map(drop)
    }

    /// Preserve the real registration and P2P receipts for finalized assertions.
    pub(crate) fn provision_existing_node_as_joiner_observed(
        &mut self,
        index: usize,
    ) -> Result<[TxOutcome; 2]> {
        let outcomes = self.provision_joiner_registration_observed(index)?;
        Ok(outcomes)
    }

    /// Generate and register validator identity without changing the
    /// currently running node or enclave profile.
    pub fn provision_joiner_registration(&mut self, index: usize) -> Result<()> {
        self.provision_joiner_registration_observed(index).map(drop)
    }

    fn provision_joiner_registration_observed(&mut self, index: usize) -> Result<[TxOutcome; 2]> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(&vd)?;
        let signing_key = vd.join("signing-key.hex").display().to_string();

        self.ensure_node_key_material(index)?;
        let bls = first_hex(&self.keygen(&["show-pubkey", "--key", &signing_key])?, 96)
            .ok_or_else(|| eyre!("no BLS pubkey from keygen"))?;
        let key = read_evm_key(&vd)?;
        let addr = eth::address_of(&key).ok_or_else(|| eyre!("bad joiner evm key"))?;
        let radicle_output = self.keygen(&[
            "radicle",
            "--output-dir",
            &vd.join("radicle").display().to_string(),
        ])?;
        let radicle_node_id = first_hex(&radicle_output, 64)
            .and_then(|value| hex::decode(value).ok())
            .map(|value| B256::from_slice(&value))
            .ok_or_else(|| eyre!("no Radicle NodeId from keygen"))?;
        let sig = self.sign_registration(&signing_key, addr, radicle_node_id)?;
        let p2p_secret = vd.join("reth-p2p-secret.hex");
        if !p2p_secret.is_file() {
            fs::write(p2p_secret, random_hex_32()?)?;
        }

        self.register_joiner_identity(
            addr,
            &key,
            Bytes::from(hex::decode(&bls)?),
            radicle_node_id,
            Bytes::from(hex::decode(&sig)?),
        )
    }

    /// Provision a joiner from caller-retained identity material.
    ///
    /// Lifecycle consistency tests retain the identity so they can later prove
    /// old-key release, same-EOA key rotation, and duplicate-key rejection
    /// through public transactions.
    pub fn provision_joiner_with_identity(
        &mut self,
        index: usize,
        identity: &RegistrationIdentity,
    ) -> Result<()> {
        self.install_registration_identity(index, identity)?;
        self.register_joiner_identity(
            identity.address(),
            identity.evm_key(),
            identity.bls_public_key().clone(),
            identity.radicle_node_id(),
            identity.registration_signature().clone(),
        )?;
        self.join_node_enclave(index)
    }

    fn register_joiner_identity(
        &self,
        addr: Address,
        key: &str,
        bls_public_key: Bytes,
        radicle_node_id: B256,
        registration_signature: Bytes,
    ) -> Result<[TxOutcome; 2]> {
        // Steps:
        // 1. Fund from validator-0.
        // 2. Prove that an unrelated EOA cannot register this ValidatorSet identity.
        // 3. Self-register and publish the P2P address.
        // The rejected call uses the joiner's otherwise-valid BLS binding. This
        // isolates caller authorization from proof validation.
        let v0 = read_evm_key(&self.cfg.validator_dir(0))?;
        eth::send_value(&self.cfg.rpc0, addr, &v0, eth::coen(2000))?;
        let registration = IValidatorSet::registerValidatorCall {
            validatorAddress: addr,
            consensusPubkey: bls_public_key,
            radicleNodeId: radicle_node_id,
            blsRegistrationSignature: registration_signature,
        };
        let unrelated = read_evm_key(&self.cfg.validator_dir(1))?;
        let unrelated_address =
            eth::address_of(&unrelated).ok_or_else(|| eyre!("bad unrelated EOA key"))?;
        let height = eth::finalized_number(&self.cfg.rpc0)
            .ok_or_else(|| eyre!("missing finalized registration checkpoint"))?;
        let unauthorized = eth::read_call_revert_data_at(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            unrelated_address,
            &registration,
            eth::RevertAt {
                value: alloy_primitives::U256::ZERO,
                height,
            },
        )?;
        eyre::ensure!(
            unauthorized.as_ref()
                == alloy_sol_types::SolError::abi_encode(
                    &IValidatorSet::UnauthorizedOwnerOrValidator {}
                ),
            "unexpected unrelated-EOA registration revert for {addr:#x}: {unauthorized:#x}"
        );
        let register_tx =
            eth::send_call(&self.cfg.rpc0, addresses::VS_ADDR, key, &registration, None)?;
        let register = registration_outcome(&self.cfg.rpc0, &register_tx)?;
        let p2p_tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            key,
            &IValidatorSet::setP2pAddressCall {
                validatorAddress: addr,
                version: 1,
                encoded: Bytes::from(hex::decode("00047f00000176c4")?),
            },
            None,
        )?;
        let p2p = registration_outcome(&self.cfg.rpc0, &p2p_tx)?;
        Ok([register, p2p])
    }

    /// Launch the joiner node (validator-mode, verifier-join args), passing any
    /// extra node args (e.g. `--consensus.keys-dir ...`). Port of `e2e_launch_joiner`.
    pub fn launch_joiner(&mut self, index: usize, extra: &[&str]) -> Result<()> {
        let argv = self.joiner_validator_args(index, extra)?;
        self.spawn_joiner_with_args(index, argv)
    }

    fn joiner_validator_args(&self, index: usize, extra: &[&str]) -> Result<Vec<String>> {
        eyre::ensure!(
            !extra
                .iter()
                .any(|arg| *arg == "--testnet.unix-time-offset-secs"
                    || arg.starts_with("--testnet.unix-time-offset-secs=")),
            "joiner clock must come from shared StartOpts, not extra arguments"
        );
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        // File-based flag: the key must never appear in argv (`ps` leak).
        let secret_file = proc::normalized_secret_file(&vd.join("reth-p2p-secret.hex"))?;

        let (public_polynomial, dkg_output) = verifier_material_paths(&self.cfg.dir);
        let mut a = self.reth_base_args(&vd, index);
        a.extend(args![
            "--validator",
            "--bootnodes",
            self.bootnodes().unwrap_or_default(),
            "--p2p-secret-key",
            secret_file.display(),
            "--metrics",
            format!("0.0.0.0:{}", self.cfg.metrics_port(index)),
            "--consensus.signing-key",
            vd.join("signing-key.hex").display(),
            "--validator.evm-key",
            vd.join("evm-key.hex").display(),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
            "--consensus.peers",
            self.consensus_peers()?,
            "--consensus.use-local-defaults",
            "--radicle.control-socket",
            self.radicle_control_socket(index).display(),
            "--radicle.status-address",
            format!("127.0.0.1:{}", self.cfg.radicle_status_port(index)),
            "--tee-enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--consensus.public-polynomial",
            public_polynomial.display(),
            "--consensus.dkg-output",
            dkg_output.display(),
        ]);
        self.extend_real_sgx_startup_timeout(&mut a);
        if let Some(offset) = self.start_opts.unix_time_offset_secs {
            a.push(format!("--testnet.unix-time-offset-secs={offset}"));
        }
        a.extend(extra.iter().map(|s| s.to_string()));
        Ok(a)
    }

    fn spawn_joiner_with_args(&mut self, index: usize, a: Vec<String>) -> Result<()> {
        self.start_radicle(index)?;
        let vd = self.cfg.validator_dir(index);
        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.env("RUST_MIN_STACK", "16777216").args(&a);
        attach_log(&mut cmd, &vd)?;
        let guard = self.spawn_node(&format!("validator-{index}"), index, &vd, cmd)?;
        self.validator_argv.insert(index, a);
        self.validators.insert(index, guard);
        Ok(())
    }

    /// Stop the joiner node (drop its owned handle -> kill + reap). Port of
    /// `e2e_stop_joiner`.
    pub fn stop_joiner(&mut self, index: usize) -> Result<()> {
        if let Some(node) = self.validators.get_mut(&index) {
            let status = node.stop_and_reap()?;
            eyre::ensure!(status.success(), "joiner {index} exited with {status}");
        }
        self.validators.remove(&index);
        Ok(())
    }

    /// `--consensus.peers` (`<public_key>@<p2p_address>,...`) from `validators.json`.
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

    fn sign_registration(
        &self,
        signing_key: &str,
        address: Address,
        radicle_node_id: B256,
    ) -> Result<String> {
        let chain_id = self.chain_id()?.to_string();
        first_hex(
            &self.keygen(&[
                "sign-registration",
                "--key",
                signing_key,
                "--validator-address",
                &format!("{address:#x}"),
                "--chain-id",
                &chain_id,
                "--radicle-node-id",
                &format!("{radicle_node_id:#x}"),
            ])?,
            120,
        )
        .ok_or_else(|| eyre!("no registration signature from keygen"))
    }

    /// Run `outbe-keygen <args>` and return stdout.
    fn keygen(&self, args: &[&str]) -> Result<String> {
        proc::run_capture(&self.cfg.bin_keygen, args)
    }

    pub(super) fn ensure_node_key_material(&self, index: usize) -> Result<()> {
        let node_dir = self.cfg.validator_dir(index);
        let bls = node_dir.join("signing-key.hex");
        let evm = node_dir.join("evm-key.hex");
        match (bls.is_file(), evm.is_file()) {
            (true, true) => Ok(()),
            (false, false) => {
                self.keygen(&["hybrid", "--output-dir", &node_dir.display().to_string()])?;
                Ok(())
            }
            _ => Err(eyre!(
                "node key material is partial: both {} and {} must exist or both be absent",
                bls.display(),
                evm.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests;
