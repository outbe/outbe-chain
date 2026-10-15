use super::*;

impl Localnet {
    /// Add the canonical block-1 TEE manifest after every scenario has finished
    /// mutating genesis. The DCAP lane binds the exact test SIGSTRUCT into a
    /// `DcapRequired` policy. SGX without DCAP and the non-hardware lanes use an
    /// explicit `GramineDirectDev` policy. The product binary constructs and
    /// validates the policy bytes. The harness never reimplements its codec.
    pub(crate) fn bind_tee_genesis(&self) -> Result<()> {
        self.ensure_ocomp_genesis()?;
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
    /// The current node binary requires a genesis-active OCOMP install for
    /// every network. Build that independent prerequisite with the product
    /// tooling before binding the hardware TEE policy. The DCAP harness does
    /// not reproduce either OCOMP's canonical codec or its signatures.
    pub(super) fn ensure_ocomp_genesis(&self) -> Result<()> {
        eyre::ensure!(
            self.committee_size() > 0,
            "LocalNet OCOMP genesis requires validators"
        );

        let genesis = self.cfg.dir.join("genesis.json");
        let current: serde_json::Value = serde_json::from_slice(&fs::read(&genesis)?)?;
        if current
            .get("config")
            .and_then(|config| config.get("ocompForkInstallV1"))
            .is_some()
        {
            return Ok(());
        }

        let validators = self.cfg.dir.join("validators.json");
        let bindings_path = self.cfg.dir.join("ocomp-bindings-v1.json");
        let mut bindings_command = Command::new(&self.cfg.bin_chain);
        bindings_command.args([
            "ocomp",
            "bindings",
            "--input",
            genesis
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 genesis path"))?,
            "--validators",
            validators
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 validators path"))?,
            "--output",
            bindings_path
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 OCOMP bindings path"))?,
        ]);
        self.run_setup(&mut bindings_command, "outbe-chain ocomp bindings")?;

        let bindings: serde_json::Value = serde_json::from_slice(&fs::read(&bindings_path)?)?;
        let required_string = |name: &str| -> Result<&str> {
            bindings
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("OCOMP bindings missing string {name}"))
        };
        let required_u64 = |name: &str| -> Result<u64> {
            bindings
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| eyre!("OCOMP bindings missing u64 {name}"))
        };
        let identities = bindings
            .get("validatorIdentityHashes")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| eyre!("OCOMP bindings missing validator identities"))?;
        let validator_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&validators)?)?;
        let validator_manifest = validator_manifest
            .as_array()
            .ok_or_else(|| eyre!("validators manifest is not an array"))?;
        eyre::ensure!(
            identities.len() == self.committee_size()
                && validator_manifest.len() == self.committee_size(),
            "OCOMP founder registrations must cover the complete genesis ValidatorSet"
        );
        for (index, validator) in validator_manifest.iter().enumerate() {
            let validator_address = validator
                .get("address")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("validator-{index} manifest address is missing"))?;
            let consensus_bls = validator
                .get("public_key")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("validator-{index} manifest public key is missing"))?;
            let output_dir = self.cfg.validator_dir(index);
            let mut keygen = Command::new(&self.cfg.bin_keygen);
            keygen
                .arg("ocomp")
                .arg("--output-dir")
                .arg(&output_dir)
                .arg("--chain-id")
                .arg(required_u64("chainId")?.to_string())
                .arg("--genesis-hash")
                .arg(required_string("genesisHash")?)
                .arg("--validator-address")
                .arg(validator_address)
                .arg("--consensus-bls-min-pk")
                .arg(consensus_bls);
            self.run_setup(&mut keygen, "outbe-keygen ocomp")?;
        }

        let generated = self.cfg.dir.join("genesis.ocomp.json");
        let protocol_bundle = self.cfg.dir.join("protocol-bundle-v1.ocb1");
        let mut genesis_command = Command::new(&self.cfg.bin_chain);
        genesis_command
            .arg("ocomp")
            .arg("genesis")
            .arg("--input")
            .arg(&genesis)
            .arg("--validators")
            .arg(&validators)
            .arg("--registrations-dir")
            .arg(&self.cfg.dir)
            .arg("--output")
            .arg(&generated)
            .arg("--protocol-bundle-output")
            .arg(&protocol_bundle);
        self.run_setup(&mut genesis_command, "outbe-chain ocomp genesis")?;

        let original = self.cfg.dir.join("genesis.pre-ocomp.json");
        fs::rename(&genesis, &original)?;
        fs::rename(&generated, &genesis)?;
        Ok(())
    }
    /// Stage the founder material required by each node's mandatory embedded
    /// OCOMP runtime when the harness is built without the external OCOMP
    /// scenario drivers.
    #[cfg(not(feature = "ocomp-integration"))]
    pub(crate) fn ensure_embedded_ocomp_domain_material(&self) -> Result<()> {
        for index in 0..self.committee_size() {
            self.ensure_embedded_ocomp_validator_domain_material(index)?;
        }
        Ok(())
    }
    /// Stage one validator's mandatory embedded OCOMP runtime domain. Founder
    /// startup and late validator promotion share this exact material contract.
    #[cfg(not(feature = "ocomp-integration"))]
    pub(crate) fn ensure_embedded_ocomp_validator_domain_material(
        &self,
        index: usize,
    ) -> Result<()> {
        let bundle = fs::read(self.cfg.dir.join("protocol-bundle-v1.ocb1"))?;
        let validator_dir = self.cfg.validator_dir(index);
        let signing_key = fs::read(validator_dir.join("ocomp-key-v1.hex"))?;
        let domain = validator_dir.join("ocomp").join("domain-v1");
        fs::create_dir_all(&domain)?;
        publish_embedded_ocomp_file(&domain.join("protocol-bundle-v1.ocb1"), &bundle, 0o640)?;
        publish_embedded_ocomp_file(&domain.join("ocomp-key-v1.hex"), &signing_key, 0o600)?;
        let validator_index = u8::try_from(index)?;
        let evm_key = hex::encode([validator_index.saturating_add(0x71); 32]);
        publish_embedded_ocomp_file(
            &domain.join("ocomp-evm-key.hex"),
            format!("{evm_key}\n").as_bytes(),
            0o600,
        )?;
        Ok(())
    }
}
