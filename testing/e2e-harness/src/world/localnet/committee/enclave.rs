//! Committee enclaves: launch, founder node-host preparation, and chain binding.

use std::fs;
use std::path::PathBuf;

use eyre::{ensure, Result, WrapErr};
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};

use super::verify_pre_dkg_public_identity;
use crate::internal::proc::{self, SealSpec};
use crate::world::localnet::Localnet;

impl Localnet {
    /// The lane-default deterministic DKG seed for validator `i` (`None` where
    /// the enclave self-seals its identity, e.g. real SGX).
    pub(super) fn default_dkg_seed(&self, i: usize) -> Option<String> {
        self.cfg
            .tee_mode
            .uses_deterministic_dkg_seed()
            .then(|| format!("{:064x}", i + 1))
    }

    /// Launch validator `i`'s enclave container (`run-testnet.sh:215-293`), owned
    /// and foreground (no `-d`), and wait for its socket.
    pub(super) fn start_enclave(&mut self, i: usize, chain_id_hex: &str) -> Result<()> {
        let seed = self.default_dkg_seed(i);
        self.start_enclave_with_seed(i, chain_id_hex, seed)
    }

    /// Use the same persistent signer, final genesis and data directory as the
    /// node entrypoint. Never pre-recover existing state: restart scenarios must
    /// exercise the node's own reconnect/recovery path without harness warmup.
    pub(super) fn prepare_fresh_founder_node_host(&self, index: usize) -> Result<()> {
        use alloy_signer::SignerSync as _;
        use alloy_signer_local::PrivateKeySigner;

        if !self.cfg.tee_mode.passes_sgx_devices() {
            return Ok(());
        }
        let directory = self.cfg.validator_dir(index);
        let data = directory.join("data");
        if data
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .try_exists()?
        {
            return Ok(());
        }
        // The NodeHost API owns only its private child, while this parent is
        // normally created by launch_validator after cohort preparation.
        fs::create_dir_all(&data)
            .wrap_err_with(|| format!("create founder node data directory {}", data.display()))?;
        let started = std::time::Instant::now();
        let secret = fs::read_to_string(directory.join("reth-p2p-secret.hex"))
            .wrap_err("read founder's provisioned Reth P2P signer")?;
        let signer: PrivateKeySigner = secret
            .trim()
            .parse()
            .map_err(|_| eyre::eyre!("invalid provisioned founder Reth P2P signer"))?;
        let reth_p2p_public = signer
            .credential()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .map_err(|_| eyre::eyre!("founder Reth P2P identity is not SEC1-33"))?;
        let genesis = self
            .validator_chain_manifests
            .get(&index)
            .cloned()
            .unwrap_or_else(|| self.cfg.dir.join("genesis.json"));
        // Use the node's authoritative policy parser, not a mode inferred from
        // the selected harness lane or the pre-measurement genesis helper.
        let spec = reth_ethereum::cli::chainspec::chain_value_parser(
            genesis
                .to_str()
                .ok_or_else(|| eyre::eyre!("founder genesis path is not UTF-8"))?,
        )?
        .as_ref()
        .clone()
        .map_header(outbe_primitives::OutbeHeader::new);
        let state =
            outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                &spec,
            );
        let network_binding = state
            .activation()
            .map_err(|error| eyre::eyre!("invalid founder genesis TEE policy: {error}"))?
            .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
            .map_err(eyre::Report::msg)?
            .network_binding();
        let endpoint = format!("127.0.0.1:{}", self.cfg.tee_port(index));
        let mut client = outbe_tee::connect_or_initialize_node_host_enclave(
            &endpoint,
            &data,
            outbe_tee::NodeHostIdentityV1 {
                network_binding,
                reth_p2p_public,
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
        )
        .wrap_err_with(|| format!("initialize founder-{index} NodeHost before committee launch"))?;
        let manifest = outbe_tee::load_committed_enclave_manifest_v1(&data)?;
        ensure!(
            manifest.node_id.reth_p2p_public == reth_p2p_public
                && manifest.network_binding() == network_binding,
            "prepared founder manifest changed its node/network identity"
        );
        verify_pre_dkg_public_identity(
            client.request(&outbe_tee::protocol::EnclaveRequest::GetPublicKeys)?,
            &manifest,
        )?;
        drop(client);
        let mut reopened =
            outbe_tee::node_host::connect_committed_node_host_enclave(&endpoint, &data)?;
        verify_pre_dkg_public_identity(
            reopened.request(&outbe_tee::protocol::EnclaveRequest::GetPublicKeys)?,
            &manifest,
        )?;
        eprintln!(
            "[e2e] founder-{index} authenticated keyless NodeHost prepared and reopened in {:?}",
            started.elapsed()
        );
        Ok(())
    }

    /// [`Self::start_enclave`] with an explicit DKG-seed override. The
    /// fresh-identity restart verb uses a different seed so the relaunched
    /// enclave presents different keys.
    pub(super) fn start_enclave_with_seed(
        &mut self,
        i: usize,
        chain_id_hex: &str,
        dkg_seed: Option<String>,
    ) -> Result<()> {
        ensure!(
            !self.enclaves.contains_key(&i),
            "enclave slot {i} is already owned"
        );
        let vd = self.cfg.validator_dir(i);
        fs::create_dir_all(&vd)?;
        let port = self.cfg.tee_port(i);
        let enclave_bin = if self.cfg.tee_mode.uses_mock_binary() {
            self.cfg.bin_mock.clone()
        } else {
            self.real_enclave_bin()?
        };
        // The harness always seals (localnet start sets OUTBE_TEE_SEAL). The host
        // dkg-seed is passed except for real+seal, where the enclave self-seals.
        let seal = Some(SealSpec {
            tee_dir: vd.join("tee"),
            chain_id_hex: chain_id_hex.to_string(),
        });

        let guard = proc::spawn_enclave_ready(
            proc::EnclaveSpec {
                name: self.cfg.tee_container(i),
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
                dkg_seed,
                seal,
                log_path: vd.join("enclave.log"),
                debug: self.cfg.debug,
            },
            self.enclave_startup_deadline(20),
        )?;
        self.enclaves.insert(i, guard);
        Ok(())
    }

    /// The genesis chain id as `0x`-padded 64-hex (enclave seal `--chain-id`).
    pub(in crate::world::localnet) fn chain_id_hex(&self) -> Result<String> {
        Ok(format!("0x{:064x}", self.chain_id()?))
    }

    pub(in crate::world::localnet) fn dev_network_binding_hex(&self) -> Result<Option<String>> {
        if !self.cfg.tee_mode.uses_mock_binary() {
            return Ok(None);
        }
        let binding =
            DcapSeededChainSpecBindingV1::from_genesis_path(&self.cfg.dir.join("genesis.json"))
                .map_err(|error| {
                    eyre::eyre!("derive mock network binding from final genesis: {error}")
                })?;
        let canonical = NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(binding.chain_id).to_be_bytes(),
            genesis_hash: binding.genesis_hash,
            attestation_mode: AttestationMode::GramineDirectDev,
        }
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode mock network binding: {error}"))?;
        Ok(Some(alloy_primitives::hex::encode(canonical)))
    }

    /// The chain ID from the immutable localnet genesis configuration.
    pub(in crate::world::localnet) fn chain_id(&self) -> Result<u64> {
        let g: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.cfg.dir.join("genesis.json"))?)?;
        g.get("config")
            .and_then(|c| c.get("chainId"))
            .and_then(|x| x.as_u64())
            .ok_or_else(|| eyre::eyre!("no chainId in genesis.json"))
    }

    /// Resolve the real (non-mock) enclave binary from the build tree.
    pub(in crate::world::localnet) fn real_enclave_bin(&self) -> Result<PathBuf> {
        if self.cfg.bin_enclave.exists() {
            return Ok(self.cfg.bin_enclave.clone());
        }
        Err(eyre::eyre!(
            "production enclave binary `outbe-tee-enclave` not found at {}",
            self.cfg.bin_enclave.display()
        ))
    }
}
