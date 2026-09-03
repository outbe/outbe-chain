//! A validator that joins a running localnet: provision (keygen, fund, register,
//! enclave, `tee join`) and launch at index = committee size. Ported
//! `e2e_provision_joiner` / `e2e_launch_joiner`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::{hex, Address, Bytes, B256};
use eyre::{bail, eyre, Result, WrapErr as _};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::TransportError;
use serde::Deserialize;

use crate::internal::{
    addresses,
    eth::{self, IValidatorSet},
    proc::{
        self, args, attach_log, first_hex, random_hex_32, read_evm_key, read_trimmed, wait_tcp,
        SealSpec,
    },
    shell::Sh,
};
use crate::world::validators::RegistrationIdentity;

use super::Localnet;

const REAL_SGX_OFFER_READ_ATTEMPTS: usize = 3;
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
        // File-based flag: the key must never appear in argv (`ps` leak).
        let secret_file = proc::normalized_secret_file(&secret_path)?;
        let mut process_args = self.reth_base_args(&vd, index);
        process_args.extend(full_node_joiner_role_args(
            &secret_file.display().to_string(),
            &format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            &format!("http://127.0.0.1:{}", self.cfg.http_port(upstream_slot)),
            &format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
        ));
        process_args.extend_from_slice(ocomp_args);
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

    /// Exit state for an owned role-neutral FullNode. `None` means the process
    /// was never registered under the canonical key and is not exit evidence.
    pub fn joiner_full_node_exit_status(&mut self, index: usize) -> Option<bool> {
        self.followers
            .get_mut(&Self::joiner_full_node_name(index))
            .map(crate::internal::proc::ChildGuard::exited)
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
            let chain_id = self.chain_id()?.to_string();
            let signature = first_hex(
                &self.keygen(&[
                    "sign-registration",
                    "--key",
                    &signing_key_arg,
                    "--validator-address",
                    &format!("{address:#x}"),
                    "--chain-id",
                    &chain_id,
                    "--radicle-node-id",
                    &format!("{radicle_node_id:#x}"),
                ])?,
                120,
            )
            .ok_or_else(|| eyre!("no registration signature from keygen"))?;
            let bls_private_key = read_trimmed(&signing_key)?;

            Ok(RegistrationIdentity::new(
                address,
                evm_key,
                bls_private_key,
                Bytes::from(hex::decode(bls)?),
                radicle_node_id,
                radicle_secret_key,
                radicle_public_key,
                Bytes::from(hex::decode(signature)?),
                random_hex_32()?,
            ))
        })();
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// Materialize a prepared identity for a joiner node. Existing key files
    /// are never overwritten.
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
        Ok(RegistrationIdentity::new(
            address,
            evm_key,
            String::new(),
            Bytes::new(),
            radicle_node_id,
            fs::read_to_string(validator_dir.join("radicle/keys/radicle"))?,
            fs::read_to_string(validator_dir.join("radicle/keys/radicle.pub"))?,
            Bytes::new(),
            read_trimmed(&validator_dir.join("reth-p2p-secret.hex"))?,
        ))
    }

    /// Provision a joiner: keygen, fund, register, p2p, enclave, `tee join`
    /// (port of `e2e_provision_joiner`). Leaves keys under `validator-<index>/`.
    pub fn provision_joiner(&mut self, index: usize) -> Result<()> {
        self.provision_existing_node_as_joiner(index)?;
        self.join_node_enclave(index)
    }

    /// Add ValidatorSet and OCOMP material to an already joined role-neutral
    /// NodeHost. This deliberately does not perform a second TEE join.
    pub fn provision_existing_node_as_joiner(&mut self, index: usize) -> Result<()> {
        self.provision_joiner_registration(index)?;
        #[cfg(feature = "ocomp-integration")]
        crate::world::ocomp::stage_direct_joiner_domain_material(&self.cfg, index)?;
        Ok(())
    }

    /// Generate and register Validator/OCOMP identity without changing the
    /// currently running node or enclave profile.
    pub fn provision_joiner_registration(&mut self, index: usize) -> Result<()> {
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
        self.prepare_joiner_ocomp_identity(index, addr, &bls)?;
        let chain_id = self.chain_id()?.to_string();
        let sig = first_hex(
            &self.keygen(&[
                "sign-registration",
                "--key",
                &signing_key,
                "--validator-address",
                &format!("{addr:#x}"),
                "--chain-id",
                &chain_id,
                "--radicle-node-id",
                &format!("{radicle_node_id:#x}"),
            ])?,
            120,
        )
        .ok_or_else(|| eyre!("no registration signature from keygen"))?;
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
        let bls = hex::encode(identity.bls_public_key());
        self.prepare_joiner_ocomp_identity(index, identity.address(), &bls)?;
        self.register_joiner_identity(
            identity.address(),
            identity.evm_key(),
            identity.bls_public_key().clone(),
            identity.radicle_node_id(),
            identity.registration_signature().clone(),
        )?;
        self.join_node_enclave(index)
    }

    fn prepare_joiner_ocomp_identity(
        &self,
        index: usize,
        address: Address,
        consensus_bls: &str,
    ) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
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
            &format!("{address:#x}"),
            "--consensus-bls-min-pk",
            consensus_bls,
        ])?;
        Ok(())
    }

    fn register_joiner_identity(
        &self,
        addr: Address,
        key: &str,
        bls_public_key: Bytes,
        radicle_node_id: B256,
        registration_signature: Bytes,
    ) -> Result<()> {
        // Fund from validator-0, prove that an unrelated EOA cannot register
        // this ValidatorSet identity, then self-register and publish the P2P
        // address. The rejected call uses the joiner's otherwise-valid BLS
        // binding, isolating caller authorization from proof validation.
        let v0 = read_evm_key(&self.cfg.validator_dir(0))?;
        eth::send_value(&self.cfg.rpc0, addr, &v0, eth::coen(2000))?;
        let registration = IValidatorSet::registerValidatorCall {
            validatorAddress: addr,
            consensusPubkey: bls_public_key,
            radicleNodeId: radicle_node_id,
            blsRegistrationSignature: registration_signature,
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
        let register_tx =
            eth::send_call(&self.cfg.rpc0, addresses::VS_ADDR, key, &registration, None)?;
        if eth::receipt_success(&self.cfg.rpc0, &register_tx) != Some(true) {
            return Err(eyre!("joiner registration failed: {register_tx}"));
        }
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
        if eth::receipt_success(&self.cfg.rpc0, &p2p_tx) != Some(true) {
            return Err(eyre!("joiner P2P registration failed: {p2p_tx}"));
        }

        Ok(())
    }

    /// Start a node's role-neutral enclave and complete its one on-chain TEE join.
    pub fn join_node_enclave(&mut self, index: usize) -> Result<()> {
        let now = eth::latest_block_timestamp(&self.cfg.rpc0)
            .ok_or_else(|| eyre!("cannot read canonical head timestamp for V1 tee join"))?;
        let valid_until = now
            .checked_add(outbe_primitives::tee_genesis_v1::PRODUCTION_TEE_LEASE_SECONDS_V1)
            .ok_or_else(|| eyre!("V1 tee join lease deadline overflow"))?;
        self.join_node_enclave_until(index, valid_until)
    }

    /// Join or recover one role-neutral enclave at an exact finalized-time
    /// deadline. Expiry fail-stops the node process, not its enclave sidecar,
    /// so recovery deliberately reuses an already-running sidecar.
    pub(crate) fn join_node_enclave_until(&mut self, index: usize, valid_until: u64) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        let key = read_evm_key(&vd)?;
        if !self.enclaves.contains_key(&index) {
            self.start_node_enclave(index)?;
        }
        let port = self.cfg.tee_port(index);
        let sock = format!("127.0.0.1:{port}");
        let binding_id = random_hex_32()?;
        let mut join = args![
            "tee",
            "join",
            "--enclave-socket",
            sock,
            "--reth-p2p-secret-key",
            vd.join("reth-p2p-secret.hex").display(),
            "--genesis",
            self.cfg.dir.join("genesis.json").display(),
            "--binding-id",
            binding_id,
            "--valid-until",
            valid_until.to_string(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            key,
            "--timeout-secs",
            if self.cfg.tee_mode.passes_sgx_devices() {
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

    /// Read one node's lease through the public CLI and its exact
    /// consensus-finalized Registry view.
    pub(crate) fn node_renewal_status(
        &self,
        index: usize,
    ) -> Result<ManualRenewalStatusObservationV1> {
        let vd = self.cfg.validator_dir(index);
        let output = Sh::new(&self.cfg).cli_required(args![
            "tee",
            "status",
            "--node-data-dir",
            vd.join("data").display(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
        ])?;
        serde_json::from_str(&output).map_err(Into::into)
    }

    /// Run manual renewal through canonical finality and return its typed evidence.
    pub(crate) fn renew_node_enclave_until_finalized(
        &self,
        index: usize,
    ) -> Result<ManualRenewalObservationV1> {
        let vd = self.cfg.validator_dir(index);
        let output = Sh::new(&self.cfg).cli_required(args![
            "tee",
            "renew",
            "--enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--node-data-dir",
            vd.join("data").display(),
            "--reth-p2p-secret-key",
            vd.join("reth-p2p-secret.hex").display(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            read_evm_key(&vd)?,
        ])?;
        match classify_manual_renewal_cli_output(&output) {
            ManualRenewalCliOutcomeV1::Finalized => {}
            ManualRenewalCliOutcomeV1::NotDue => {
                return Err(eyre!(
                    "manual renewal remained NotDue after the harness entered the finalized renewal window for node {index}: {output}"
                ));
            }
            ManualRenewalCliOutcomeV1::Unexpected => {
                return Err(eyre!(
                    "manual renewal returned an unexpected successful result for node {index}: {output}"
                ));
            }
        }
        let journal_path = vd.join("data/tee-renewal-v1/journal.json");
        let journal: serde_json::Value = serde_json::from_slice(
            &fs::read(&journal_path)
                .wrap_err_with(|| format!("read finalized renewal journal {journal_path:?}"))?,
        )?;
        if journal
            .pointer("/lifecycle/state")
            .and_then(|value| value.as_str())
            != Some("finalized")
        {
            return Err(eyre!(
                "manual renewal command returned without a finalized journal for node {index}"
            ));
        }
        let renewal_nonce = journal
            .pointer("/lifecycle/finalized_binding/renewalNonce")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| eyre!("finalized renewal journal has no renewal nonce"))?;
        let valid_until = journal
            .pointer("/lifecycle/finalized_binding/validUntil")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| eyre!("finalized renewal journal has no lease deadline"))?;
        Ok(ManualRenewalObservationV1 {
            renewal_nonce,
            valid_until,
        })
    }

    /// Prove that an expired enclave cannot be renewed and must use the normal
    /// `tee join` recovery transition.
    pub(crate) fn renew_node_enclave_expected_failure(&self, index: usize) -> Result<String> {
        let vd = self.cfg.validator_dir(index);
        Sh::new(&self.cfg).cli_expected_failure(args![
            "tee",
            "renew",
            "--enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--node-data-dir",
            vd.join("data").display(),
            "--reth-p2p-secret-key",
            vd.join("reth-p2p-secret.hex").display(),
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            read_evm_key(&vd)?,
        ])
    }

    /// Restart a joiner's enclave from its existing writable TEE directory.
    /// On hardware SGX this exercises EGETKEY unsealing rather than provisioning
    /// fresh join material.
    pub fn restart_joiner_enclave(&mut self, index: usize) -> Result<()> {
        self.enclaves.remove(&index);
        self.start_node_enclave(index)
    }

    /// Attempt to reopen one already-keyed DCAP enclave with remote attestation
    /// disabled but the same disposable E2E signer and sealed directory.
    pub fn attempt_no_attest_sealed_restart(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        let port = self.cfg.tee_port(index);
        self.enclaves.remove(&index);
        self.ensure_enclave_image_once()?;
        let log_path = vd.join("enclave-no-attest-downgrade.log");
        if log_path.exists() {
            fs::remove_file(&log_path)?;
        }
        let guard = proc::spawn_enclave(proc::EnclaveSpec {
            name: format!("{}-no-attest-downgrade", self.cfg.tee_container(index)),
            tee_port: port,
            enclave_bin: self.real_enclave_bin()?,
            signing_key: self.cfg.dir.join("test-sgx-signing-key.pem"),
            network_descriptor: None,
            launch: self.enclave_launch()?,
            sudo: self.cfg.sudo,
            pass_sgx_devices: true,
            remote_attestation: proc::TestRemoteAttestation::None,
            dkg_seed: None,
            seal: Some(SealSpec {
                tee_dir: vd.join("tee"),
                chain_id_hex: self.chain_id_hex()?,
            }),
            log_path,
            debug: self.cfg.debug,
        })?;
        if wait_tcp(port, 50) {
            drop(guard);
            return Err(eyre!(
                "SGX no-attestation runtime reopened a DCAP-bound enclave listener"
            ));
        }
        drop(guard);
        Ok(())
    }

    pub fn assert_no_attest_restart_rejected(&self, index: usize) -> Result<()> {
        let log_path = self
            .cfg
            .validator_dir(index)
            .join("enclave-no-attest-downgrade.log");
        let log = fs::read_to_string(&log_path)
            .wrap_err_with(|| format!("read downgrade log {}", log_path.display()))?;
        if !has_exact_production_no_attest_rejection(&log) {
            bail!("downgrade runtime did not report a fail-closed DCAP policy rejection");
        }
        if log.contains("unsealed offer key + group signature")
            || log.contains("FinalizedAdmissionIngestedV1")
        {
            bail!("downgrade runtime exposed or activated the permanent offer key");
        }
        Ok(())
    }

    pub fn stop_node_enclave(&mut self, index: usize) {
        self.enclaves.remove(&index);
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
        let endpoint = format!("127.0.0.1:{}", self.cfg.tee_port(index));
        let real_sgx = self.cfg.tee_mode.passes_sgx_devices();
        let attempts = if real_sgx {
            REAL_SGX_OFFER_READ_ATTEMPTS
        } else {
            1
        };
        for attempt in 0..attempts {
            let unexpected_reinitialize =
                |_| Err("committed node unexpectedly required reinitialization".to_owned());
            let mut client = match outbe_tee::connect_or_initialize_node_host_enclave(
                &endpoint,
                &node_data_dir,
                outbe_tee::NodeHostIdentityV1 {
                    network_binding: manifest.network_binding(),
                    reth_p2p_public: manifest.node_id.reth_p2p_public,
                },
                unexpected_reinitialize,
            ) {
                Ok(client) => client,
                Err(error) if retry_node_offer_read(real_sgx, attempt, &error) => {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    continue;
                }
                Err(error) => {
                    return Err(eyre!("authenticated node enclave reopen failed: {error}"));
                }
            };
            let response = match client.request(&EnclaveRequest::GetPublicKeys) {
                Ok(response) => response,
                Err(error) if retry_node_offer_read(real_sgx, attempt, &error) => {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match response {
                EnclaveResponse::PublicKeys {
                    offer_key_ready: true,
                    recipient_x25519_pub,
                    ..
                } => return Ok(recipient_x25519_pub),
                other => {
                    return Err(eyre!(
                        "authenticated node enclave has no permanent offer key: {other:?}"
                    ));
                }
            }
        }
        unreachable!("the bounded authenticated offer-key read loop always returns")
    }

    pub(super) fn start_node_enclave(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        let port = self.cfg.tee_port(index);
        self.ensure_enclave_image_once()?;
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
            network_descriptor: (self.cfg.tee_mode == crate::env::TeeMode::Real)
                .then(|| self.cfg.dir.join("network-descriptor-v1.bin")),
            launch: self.enclave_launch()?,
            sudo: self.cfg.sudo,
            pass_sgx_devices: self.cfg.tee_mode.passes_sgx_devices(),
            remote_attestation: match self.cfg.tee_mode {
                crate::env::TeeMode::Real => proc::TestRemoteAttestation::Dcap,
                crate::env::TeeMode::SgxNoAttest
                | crate::env::TeeMode::GramineDirect
                | crate::env::TeeMode::Mock
                | crate::env::TeeMode::MockNative => proc::TestRemoteAttestation::None,
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
        #[cfg(not(feature = "ocomp-integration"))]
        self.ensure_embedded_ocomp_validator_domain_material(index)?;

        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        // File-based flag: the key must never appear in argv (`ps` leak).
        let secret_file = proc::normalized_secret_file(&vd.join("reth-p2p-secret.hex"))?;
        self.start_radicle(index)?;

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
        a.extend(extra.iter().map(|s| s.to_string()));

        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.env("RUST_MIN_STACK", "16777216").args(&a);
        attach_log(&mut cmd, &vd)?;
        let guard = self.spawn_node(&format!("validator-{index}"), index, &vd, cmd)?;
        self.validators.insert(index, guard);
        Ok(())
    }

    /// Stop the joiner node (drop its owned handle -> kill + reap). Port of
    /// `e2e_stop_joiner`.
    pub fn stop_joiner(&mut self, index: usize) -> Result<()> {
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
mod tests {
    use super::*;

    #[test]
    fn real_sgx_offer_read_retries_only_bounded_io_timeouts() {
        assert!(retry_node_offer_read(
            true,
            0,
            &TransportError::IoTimeout {
                operation: "Noise handshake response read",
                timeout_secs: 30,
            }
        ));
        assert!(retry_node_offer_read(
            true,
            1,
            &TransportError::IoTimeout {
                operation: "request response read",
                timeout_secs: 30,
            }
        ));
        assert!(!retry_node_offer_read(
            true,
            2,
            &TransportError::IoTimeout {
                operation: "request response read",
                timeout_secs: 30,
            }
        ));
        assert!(!retry_node_offer_read(
            false,
            0,
            &TransportError::IoTimeout {
                operation: "Noise handshake response read",
                timeout_secs: 30,
            }
        ));
        assert!(!retry_node_offer_read(
            true,
            0,
            &TransportError::Handshake(
                "wrapper mentioned enclave io timeout but this is policy".to_owned()
            )
        ));
    }

    #[test]
    fn manual_renewal_cli_result_is_checked_before_reading_the_journal() {
        assert_eq!(
            classify_manual_renewal_cli_output(
                "Finalized {\n    finalized_height: 42,\n    valid_until: 99,\n}"
            ),
            ManualRenewalCliOutcomeV1::Finalized
        );
        assert_eq!(
            classify_manual_renewal_cli_output(
                "NotDue {\n    finalized_height: 41,\n    opens_at_timestamp: 98,\n}"
            ),
            ManualRenewalCliOutcomeV1::NotDue
        );
        assert_eq!(
            classify_manual_renewal_cli_output("Submitted { transaction_hash: 0x01 }"),
            ManualRenewalCliOutcomeV1::Unexpected
        );
    }

    #[test]
    fn full_node_runtime_does_not_load_separately_provisioned_role_keys() {
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

    #[test]
    fn downgrade_log_requires_the_exact_production_no_attest_rejection() {
        assert!(has_exact_production_no_attest_rejection(
            "production DCAP release refuses runtime attestation none (gramine-sgx; remote attestation disabled - EGETKEY sealing available)"
        ));
        assert!(!has_exact_production_no_attest_rejection(
            "runtime mode does not satisfy DcapRequired"
        ));
        assert!(!has_exact_production_no_attest_rejection(
            "production DCAP release refuses runtime attestation sgx-no-attest"
        ));
    }
}
