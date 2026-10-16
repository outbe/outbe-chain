//! The joiner's role-neutral enclave: TEE join, renewal, restart, and offer reads.

use std::fs;

use eyre::{bail, eyre, Result, WrapErr as _};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

use super::{
    classify_manual_renewal_cli_output, has_exact_production_no_attest_rejection,
    retry_node_offer_read, ManualRenewalCliOutcomeV1, ManualRenewalObservationV1,
    ManualRenewalStatusObservationV1, REAL_SGX_OFFER_READ_ATTEMPTS,
};
use crate::internal::{
    eth,
    proc::{self, args, random_hex_32, read_evm_key, wait_tcp, SealSpec},
    shell::Sh,
};
use crate::world::localnet::Localnet;

impl Localnet {
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
        self.restart_joiner_enclave_observed(index, |_| Ok(()))
    }

    /// Observe the boundary between the old owned enclave and its replacement.
    pub(crate) fn restart_joiner_enclave_observed(
        &mut self,
        index: usize,
        before_launch: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        if let Some(enclave) = self.enclaves.get_mut(&index) {
            enclave.stop_and_reap()?;
        }
        self.enclaves.remove(&index);
        before_launch(self)?;
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
            dev_network_binding: None,
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
    /// state. This method proves that the same key remains reachable after the
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

    pub(in crate::world::localnet) fn start_node_enclave(&mut self, index: usize) -> Result<()> {
        eyre::ensure!(
            !self.enclaves.contains_key(&index),
            "enclave slot {index} is already owned"
        );
        let vd = self.cfg.validator_dir(index);
        let port = self.cfg.tee_port(index);
        self.ensure_enclave_image_once()?;
        let enclave_bin = if self.cfg.tee_mode.uses_mock_binary() {
            self.cfg.bin_mock.clone()
        } else {
            self.real_enclave_bin()?
        };
        let guard = proc::spawn_enclave_ready(
            proc::EnclaveSpec {
                name: self.cfg.tee_container(index),
                tee_port: port,
                enclave_bin,
                signing_key: self.cfg.dir.join("test-sgx-signing-key.pem"),
                network_descriptor: (!self.cfg.tee_mode.uses_mock_binary())
                    .then(|| self.cfg.dir.join("network-descriptor-v1.bin")),
                dev_network_binding: self.dev_network_binding_hex()?,
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
            },
            self.enclave_startup_deadline(10),
        )?;
        self.enclaves.insert(index, guard);
        Ok(())
    }
}
