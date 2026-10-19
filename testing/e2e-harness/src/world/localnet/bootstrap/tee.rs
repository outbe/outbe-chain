use super::*;

impl Localnet {
    /// Add the canonical block-1 TEE manifest after every scenario has finished
    /// mutating genesis. The DCAP lane binds the exact test SIGSTRUCT into a
    /// `DcapRequired` policy. SGX without DCAP and the non-hardware lanes use an
    /// explicit `GramineDirectDev` policy. The product binary constructs and
    /// validates the policy bytes. The harness never reimplements its codec.
    pub(crate) fn bind_tee_genesis(&self) -> Result<()> {
        let genesis = self.cfg.dir.join("genesis.json");
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&genesis)?)?;
        if value
            .get("config")
            .and_then(|config| config.get("teeAttestationV1"))
            .is_some()
        {
            return Ok(());
        }
        let seeded = self.cfg.dir.join("genesis.seeded.json");
        if seeded.exists() {
            bail!(
                "refusing to overwrite existing mutable genesis input {}",
                seeded.display()
            );
        }
        fs::rename(&genesis, &seeded)?;
        let network_descriptor = if !self.cfg.tee_mode.uses_mock_binary() {
            let binding = DcapSeededChainSpecBindingV1::from_genesis_path(&seeded)
                .map_err(|error| eyre!("derive seeded network descriptor: {error}"))?;
            let descriptor = TrustedNetworkDescriptorV1 {
                network_binding: NetworkBindingV1 {
                    chain_id: alloy_primitives::U256::from(binding.chain_id).to_be_bytes(),
                    genesis_hash: binding.genesis_hash,
                    attestation_mode: if self.cfg.tee_mode == TeeMode::Real {
                        AttestationMode::DcapRequired
                    } else {
                        AttestationMode::GramineDirectDev
                    },
                },
                genesis_consensus_keys: binding.genesis_consensus_keys,
            }
            .encode_canonical()
            .map_err(|error| eyre!("encode seeded network descriptor: {error}"))?;
            let path = self.cfg.dir.join("network-descriptor-v1.bin");
            fs::write(&path, descriptor)?;
            Some(path)
        } else {
            None
        };
        let mut command = Command::new(&self.cfg.bin_chain);
        command
            .arg("tee")
            .arg("genesis")
            .arg("--input")
            .arg(&seeded)
            .arg("--output")
            .arg(&genesis)
            .arg("--mode");
        match self.cfg.tee_mode {
            TeeMode::Real => {
                let signing_key = self.cfg.dir.join("test-sgx-signing-key.pem");
                let image_id = proc::ensure_enclave_image(
                    &self.cfg.repo,
                    self.cfg.sudo,
                    &signing_key,
                    self.enclave_image_id.as_ref(),
                )?;
                let measurement = proc::inspect_test_sgx_measurement(
                    &self.cfg.repo,
                    &image_id,
                    self.cfg.sudo,
                    proc::MeasurementMounts {
                        enclave_bin: &self.real_enclave_bin()?,
                        signing_key: &signing_key,
                        network_descriptor: network_descriptor
                            .as_deref()
                            .expect("real SGX creates a measured network descriptor"),
                    },
                )?;
                command
                    .arg("dcap-required")
                    .arg("--mrenclave")
                    .arg(measurement.mrenclave)
                    .arg("--mrsigner")
                    .arg(measurement.mrsigner)
                    .arg("--isv-prod-id")
                    .arg(measurement.isv_prod_id.to_string())
                    .arg("--minimum-isv-svn")
                    .arg(measurement.isv_svn.to_string())
                    .arg("--minimum-tcb-evaluation-data-number")
                    .arg("1");
            }
            TeeMode::SgxNoAttest | TeeMode::GramineDirect | TeeMode::Mock | TeeMode::MockNative => {
                command.arg("gramine-direct-dev");
            }
        }
        if let Err(error) = self.run_setup(&mut command, "outbe-chain tee genesis") {
            if !genesis.exists() {
                let _ = fs::rename(&seeded, &genesis);
            }
            return Err(error);
        }
        Ok(())
    }
}
