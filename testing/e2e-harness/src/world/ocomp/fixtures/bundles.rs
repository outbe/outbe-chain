use crate::world::ocomp::*;

impl OcompTopology {
    /// Generate and publish the complete immutable measurement fork before any
    /// node process starts.
    ///
    /// The resulting base genesis hash binds the request profile and protocol
    /// bundle without a synthetic generic Update. Adding the canonical install
    /// under `genesis.config` does not alter that header hash.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_measurement_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        self.prepare_measurement_fork_install_inner(None, &[], false, false)
    }

    /// Prepare the same immutable measurement fork plus one bounded, internally
    /// consistent OFFERING fixture. Its Metadosis VWAP fields are derived from
    /// Oracle snapshots, and Tribute pricing uses the same VWAP/S-curve state as
    /// production before the public transaction enters the normal lifecycle.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_public_measurement_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        self.prepare_measurement_fork_install_inner(
            Some(OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS),
            &[],
            false,
            false,
        )
    }

    /// Prepare the public measurement fork plus one empty, independently
    /// scheduled next-day WWD used only to prove recovery after Job A expires.
    /// Job B itself is never seeded: Tribute, JobIntent, export, compute, votes
    /// and completion still traverse the production protocol after launch.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_public_recovery_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        self.prepare_measurement_fork_install_inner(
            Some(OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS),
            &[],
            false,
            true,
        )
    }

    /// Prepare a public measurement chain whose base genesis funds exactly
    /// `tribute_count` deterministic, distinct Tribute owners. The owners still
    /// create every Tribute through the ordinary encrypted public transaction
    /// path; this helper supplies only transaction gas funding before the base
    /// genesis hash and immutable fork bindings are derived.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_public_capacity_fork_install(
        &self,
        tribute_count: usize,
    ) -> Result<(OcompMeasurementForkV1, Vec<String>)> {
        if tribute_count == 0 {
            eyre::bail!("public capacity fixture requires at least one Tribute owner");
        }
        let private_keys = capacity_tribute_private_keys(tribute_count)?;
        let prepared = self.prepare_measurement_fork_install_inner(
            Some(OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS),
            &private_keys,
            false,
            false,
        )?;
        Ok((prepared, private_keys))
    }

    /// Prepare the dedicated fresh Metadosis closure chain. Unlike the legacy
    /// OCOMP capacity measurement, this removes the Python-seeded active WWD and
    /// does not shorten any phase timestamp. Block 1 must therefore create the
    /// scenario WWD through the production lifecycle command.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_fresh_metadosis_capacity_fork_install(
        &self,
        tribute_count: usize,
    ) -> Result<(OcompMeasurementForkV1, Vec<String>)> {
        if tribute_count == 0 {
            eyre::bail!("fresh Metadosis fixture requires at least one Tribute owner");
        }
        let private_keys = capacity_tribute_private_keys(tribute_count)?;
        let prepared =
            self.prepare_measurement_fork_install_inner(None, &private_keys, true, false)?;
        Ok((prepared, private_keys))
    }

    #[cfg(feature = "ocomp-integration")]
    fn publish_validator_domain_material(&self, install: &OcompForkInstallV1) -> Result<()> {
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let canonical_bundle = install.protocol_bundle.encode_canonical(&limits)?;
        let protocol_bundle_hash = install.protocol_bundle.protocol_bundle_hash(&limits)?;
        for (validator_index, domain) in self.domains.iter().enumerate() {
            fs::create_dir_all(&domain.root)?;
            publish_exact_file(
                &domain.root.join("protocol-bundle-v1.ocb1"),
                &canonical_bundle,
                0o640,
            )?;
            publish_bundle_catalog_entry(&domain.root, protocol_bundle_hash, &canonical_bundle)?;
            let key = measurement_signing_key(u8::try_from(validator_index)?);
            let key_bytes = format!("{}\n", hex::encode(key.to_bytes()));
            publish_exact_file(
                &domain.root.join("ocomp-key-v1.hex"),
                key_bytes.as_bytes(),
                0o600,
            )?;
            let evm_key = ocomp_evm_private_key(u8::try_from(validator_index)?);
            // The signer trims surrounding whitespace around lowercase 64-hex.
            publish_exact_file(
                &domain.root.join("ocomp-evm-key.hex"),
                format!("{}\n", evm_key.trim_start_matches("0x")).as_bytes(),
                0o600,
            )?;
        }
        Ok(())
    }

    /// Stage the exact random OCOMP result-signing keys and registrations that
    /// were bound into the bootstrapped genesis. Persistent LocalNet must never
    /// replace them with the deterministic measurement-fixture keys used by
    /// isolated scenarios.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_bootstrapped_runtime(&self) -> Result<OcompLaunchIdentityV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let spec = parse_outbe_chain_spec(&genesis_path)?;
        let install = outbe_node::ocomp::fork::require_genesis_active_ocomp_fork_install(&spec)?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let install_hash = install.install_hash(&limits)?;
        eyre::ensure!(
            install.founder_registrations.len() == self.domains.len(),
            "OCOMP founder registration count {} differs from LocalNet validator count {}",
            install.founder_registrations.len(),
            self.domains.len()
        );

        let canonical_bundle = install.protocol_bundle.encode_canonical(&limits)?;
        let bootstrapped_bundle_path = self.cfg.dir.join("protocol-bundle-v1.ocb1");
        let bootstrapped_bundle = fs::read(&bootstrapped_bundle_path)?;
        eyre::ensure!(
            bootstrapped_bundle == canonical_bundle,
            "bootstrapped protocol bundle does not match the genesis OCOMP install"
        );

        for (index, (domain, founder)) in self
            .domains
            .iter()
            .zip(&install.founder_registrations)
            .enumerate()
        {
            let validator_dir = self.cfg.validator_dir(index);
            let registration_path = validator_dir.join("ocomp-registration-v1.ocb1");
            let registration =
                OcompKeyRegistrationV1::decode_canonical(&fs::read(&registration_path)?, &limits)?;
            eyre::ensure!(
                &registration == founder,
                "validator-{index} OCOMP registration differs from the genesis founder registration"
            );
            registration.validate_proof_of_possession(&limits)?;

            let key_path = validator_dir.join("ocomp-key-v1.hex");
            let key_file = fs::read(&key_path)?;
            let key_hex = std::str::from_utf8(&key_file)?.trim();
            let key_bytes = hex::decode(key_hex)?;
            eyre::ensure!(
                key_bytes.len() == 32,
                "validator-{index} OCOMP result-signing key is not 32 bytes"
            );
            let signing_key = SigningKey::from_slice(&key_bytes)?;
            eyre::ensure!(
                signing_key
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
                    == registration.core.ocomp_public_key_sec1.as_slice(),
                "validator-{index} OCOMP result-signing key does not match its genesis registration"
            );

            fs::create_dir_all(&domain.root)?;
            publish_exact_file(
                &domain.root.join("protocol-bundle-v1.ocb1"),
                &bootstrapped_bundle,
                0o640,
            )?;
            publish_bundle_catalog_entry(
                &domain.root,
                install.request_profile.protocol_bundle_hash,
                &bootstrapped_bundle,
            )?;
            publish_exact_file(&domain.root.join("ocomp-key-v1.hex"), &key_file, 0o600)?;
            let evm_key = ocomp_evm_private_key(u8::try_from(index)?);
            publish_exact_file(
                &domain.root.join("ocomp-evm-key.hex"),
                format!("{evm_key}\n").as_bytes(),
                0o600,
            )?;
        }

        Ok(OcompMeasurementForkV1 {
            install: install.as_ref().clone(),
            install_hash,
            public_worldwide_day: None,
        }
        .launch_identity())
    }

    /// Ensure every validator has the complete node-owned OCOMP domain before
    /// the production node starts its embedded ExEx. Ordinary fresh LocalNet
    /// bootstraps stage the generated founder material here. Specialized Final
    /// and measurement fixtures publish their own exact domain material before
    /// reaching this point and must never be overwritten.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_validator_domain_material_before_node_start(&self) -> Result<()> {
        let required_names = [
            "protocol-bundle-v1.ocb1",
            "ocomp-key-v1.hex",
            "ocomp-evm-key.hex",
        ];
        let expected = self.domains.len() * required_names.len();
        let present = self
            .domains
            .iter()
            .flat_map(|domain| {
                required_names
                    .iter()
                    .map(move |name| domain.root.join(name))
            })
            .filter(|path| path.is_file())
            .count();
        match present {
            0 => {
                let _identity = self.prepare_bootstrapped_runtime()?;
                Ok(())
            }
            count if count == expected => Ok(()),
            count => Err(eyre::eyre!(
                "partial OCOMP validator domain material before node start: {count}/{expected} files"
            )),
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub(in crate::world::ocomp) fn prepare_measurement_fork_install_inner(
        &self,
        public_offering_after_genesis_secs: Option<u64>,
        capacity_tribute_private_keys: &[String],
        clear_seeded_metadosis: bool,
        seed_recovery_day: bool,
    ) -> Result<OcompMeasurementForkV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
        let chain_id = genesis_chain_id(&genesis)?;
        let capacity_accounts_changed =
            fund_capacity_tribute_accounts(&mut genesis, capacity_tribute_private_keys)?;
        let (public_day_changed, public_worldwide_day) =
            if let Some(offering_after_genesis_secs) = public_offering_after_genesis_secs {
                let (changed, worldwide_day) = schedule_public_measurement_day(
                    &mut genesis,
                    chain_id,
                    offering_after_genesis_secs,
                )?;
                (changed, Some(worldwide_day))
            } else {
                (false, None)
            };
        eyre::ensure!(
            !seed_recovery_day || public_worldwide_day.is_some(),
            "OCOMP recovery fixture requires its first public WorldwideDay"
        );
        let recovery_day_changed = if seed_recovery_day {
            schedule_public_recovery_day(
                &mut genesis,
                chain_id,
                public_worldwide_day.expect("recovery fixture public WWD"),
            )?;
            true
        } else {
            false
        };
        let seeded_metadosis_changed = if clear_seeded_metadosis {
            clear_seeded_metadosis_days(&mut genesis, chain_id)?
        } else {
            false
        };
        let fresh_oracle_changed = if clear_seeded_metadosis {
            seed_fresh_metadosis_oracle_input(&mut genesis, chain_id)?
        } else {
            false
        };
        let gas_envelope_changed = apply_measurement_gas_envelope(&mut genesis)?;
        if capacity_accounts_changed
            || public_day_changed
            || recovery_day_changed
            || seeded_metadosis_changed
            || fresh_oracle_changed
            || gas_envelope_changed
        {
            replace_json_atomically(&genesis_path, &genesis)?;
        }

        let base_spec = parse_outbe_chain_spec(&genesis_path)?;
        let base_genesis_hash = base_spec.genesis_hash();
        let protocol_constants =
            outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(&genesis)?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let install = measurement_fork_install(
            chain_id,
            base_genesis_hash,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT,
            &self.cfg.dir.join("validators.json"),
            &limits,
            protocol_constants.ocomp_compute_vote_window_blocks,
        )?;
        install.validate_for_chain(chain_id, base_genesis_hash, &limits)?;
        let canonical_install = install.encode_canonical(&limits)?;
        let install_hash = install.install_hash(&limits)?;
        self.publish_validator_domain_material(&install)?;

        let config = genesis
            .get_mut("config")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("generated genesis config is not an object"))?;
        let manifest = serde_json::json!({
            "canonicalBytes": format!("0x{}", hex::encode(&canonical_install)),
            "installHash": install_hash,
        });
        let mut manifest_changed = false;
        match config.get(outbe_node::ocomp::fork::OCOMP_FORK_INSTALL_GENESIS_KEY) {
            Some(existing) if existing == &manifest => {}
            Some(_) => {
                eyre::bail!("refusing to replace a different OCOMP fork install");
            }
            None => {
                config.insert(
                    outbe_node::ocomp::fork::OCOMP_FORK_INSTALL_GENESIS_KEY.to_owned(),
                    manifest,
                );
                manifest_changed = true;
            }
        }
        let layout_manifest = serde_json::json!({
            "layoutHash": METADOSIS_STORAGE_LAYOUT_V1_HASH,
        });
        match config.get(outbe_node::ocomp::fork::METADOSIS_STORAGE_LAYOUT_GENESIS_KEY) {
            Some(existing) if existing == &layout_manifest => {}
            Some(_) => {
                eyre::bail!("refusing to replace a different Metadosis storage layout");
            }
            None => {
                config.insert(
                    outbe_node::ocomp::fork::METADOSIS_STORAGE_LAYOUT_GENESIS_KEY.to_owned(),
                    layout_manifest,
                );
                manifest_changed = true;
            }
        }
        if manifest_changed {
            replace_json_atomically(&genesis_path, &genesis)?;
        }

        let armed_spec = parse_outbe_chain_spec(&genesis_path)?;
        if armed_spec.genesis_hash() != base_genesis_hash {
            eyre::bail!("OCOMP genesis config extension changed the base genesis hash");
        }
        let loaded = outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&armed_spec)?;
        if loaded.as_ref() != &install {
            eyre::bail!("node loader returned a different OCOMP fork install");
        }

        Ok(OcompMeasurementForkV1 {
            install,
            install_hash,
            public_worldwide_day,
        })
    }

    /// Create a second, internally valid chain manifest with the same genesis
    /// header and a distinct OCOMP activation height/install hash.
    ///
    /// Only the selected validator receives this path. Canonical committee
    /// manifests and state remain untouched.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_mismatched_fork_manifest(
        &self,
        validator_index: u8,
    ) -> Result<OcompMismatchedForkManifestV1> {
        self.domain(validator_index)?;
        let canonical_path = self.cfg.dir.join("genesis.json");
        let canonical_spec = parse_outbe_chain_spec(&canonical_path)?;
        let canonical_genesis_hash = canonical_spec.genesis_hash();
        let canonical =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&canonical_spec)?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let canonical_install_hash = canonical.install_hash(&limits)?;
        let mut mismatched = canonical.as_ref().clone();
        mismatched.request_profile.source_availability_policy_id = alloy_primitives::keccak256(
            b"OUTBE_OCOMP_FINAL_MISMATCHED_SOURCE_AVAILABILITY_POLICY_V1\0",
        );
        if mismatched.request_profile.source_availability_policy_id
            == canonical.request_profile.source_availability_policy_id
        {
            eyre::bail!("mismatched OCOMP source-availability policy equals the canonical policy");
        }
        mismatched.validate_for_chain(
            canonical.request_profile.chain_id,
            canonical_genesis_hash,
            &limits,
        )?;
        let canonical_bytes = mismatched.encode_canonical(&limits)?;
        let mismatched_install_hash = mismatched.install_hash(&limits)?;
        if mismatched_install_hash == canonical_install_hash {
            eyre::bail!("distinct OCOMP fork installs produced the same install hash");
        }

        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&canonical_path)?)?;
        let config = genesis
            .get_mut("config")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("generated genesis config is not an object"))?;
        config.insert(
            outbe_node::ocomp::fork::OCOMP_FORK_INSTALL_GENESIS_KEY.to_owned(),
            serde_json::json!({
                "canonicalBytes": format!("0x{}", hex::encode(canonical_bytes)),
                "installHash": mismatched_install_hash,
            }),
        );

        let path = self.cfg.dir.join(format!(
            "genesis-ocomp-mismatch-validator-{validator_index}.json"
        ));
        replace_json_atomically(&path, &genesis)?;
        let mismatched_spec = parse_outbe_chain_spec(&path)?;
        if mismatched_spec.genesis_hash() != canonical_genesis_hash {
            eyre::bail!("mismatched OCOMP manifest changed the canonical genesis header");
        }
        let loaded = outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&mismatched_spec)?;
        if loaded.as_ref() != &mismatched {
            eyre::bail!("node loader did not preserve the mismatched OCOMP install");
        }

        Ok(OcompMismatchedForkManifestV1 {
            path,
            canonical_install_hash,
            mismatched_install_hash,
            canonical_activation_height: canonical.activation_height,
            mismatched_activation_height: mismatched.activation_height,
        })
    }

    /// Installs one hash-named successor bundle in every OCOMP domain. Nodes
    /// preload it before governance staging, so activation itself needs no
    /// process restart.
    #[cfg(feature = "ocomp-integration")]
    pub fn stage_successor_bundle(
        &self,
        protocol_bundle: &ProtocolBundleV1,
    ) -> Result<OcompLaunchIdentityV1> {
        let current = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let canonical = protocol_bundle.encode_canonical(&limits)?;
        let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(&limits)?;
        eyre::ensure!(
            protocol_bundle_hash != current.protocol_bundle_hash,
            "OCOMP successor bundle must differ from the active bundle"
        );
        for validator_index in self.validator_indices()? {
            publish_bundle_catalog_entry(
                self.domain_root(validator_index)?,
                protocol_bundle_hash,
                &canonical,
            )?;
        }
        if let Some((_, domain)) = &self.keyless_full_node_domain {
            publish_bundle_catalog_entry(&domain.root, protocol_bundle_hash, &canonical)?;
        }
        Ok(OcompLaunchIdentityV1 {
            protocol_bundle_hash,
            ..current
        })
    }

    /// Install public runtime bundles before a distinct cold follower's first
    /// provisioning/launch. No chain database, CAS, result, journal or key is
    /// copied. The caller still owns admission, first launch and replay proof.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn stage_cold_history_follower_bundles(&self, index: usize) -> Result<()> {
        eyre::ensure!(
            index >= self.domains.len(),
            "history follower overlaps a validator"
        );
        eyre::ensure!(
            self.keyless_full_node_domain
                .as_ref()
                .is_none_or(|(slot, _)| usize::from(*slot) != index),
            "history follower overlaps the already synchronized FullNode"
        );
        let node_dir = self.cfg.validator_dir(index);
        for path in [
            node_dir.join("data"),
            node_dir.join("node.log"),
            node_dir.join("ocomp"),
        ] {
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => {
                    eyre::bail!("history follower slot is not cold: {}", path.display());
                }
            }
        }
        let initial = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let source = self.domain_root(0)?;
        let hashes = installed_protocol_bundle_hashes(source, initial.protocol_bundle_hash)?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let mut bundles = Vec::new();
        for encoded in hashes.split(',') {
            let hash = encoded.parse::<B256>()?;
            let path = source
                .join("protocol-bundles-v1")
                .join(format!("{}.ocb1", hex::encode(hash)));
            let metadata = fs::symlink_metadata(&path)?;
            eyre::ensure!(
                metadata.file_type().is_file(),
                "history bundle is not a regular file"
            );
            let bytes = fs::read(&path)?;
            let bundle = ProtocolBundleV1::decode_canonical(&bytes, &limits)?;
            eyre::ensure!(
                bundle.protocol_bundle_hash(&limits)? == hash,
                "history bundle hash mismatch"
            );
            bundles.push((hash, bytes));
        }
        let initial_bytes = bundles
            .iter()
            .find(|(hash, _)| *hash == initial.protocol_bundle_hash)
            .ok_or_else(|| eyre::eyre!("history bundle catalog has no predecessor"))?;
        if let Some(successor) = self.successor_identity {
            eyre::ensure!(
                bundles
                    .iter()
                    .any(|(hash, _)| *hash == successor.protocol_bundle_hash),
                "history bundle catalog has no successor"
            );
        }
        let destination = node_dir.join("ocomp").join("domain-v1");
        fs::create_dir_all(&destination)?;
        publish_exact_file(
            &destination.join("protocol-bundle-v1.ocb1"),
            &initial_bytes.1,
            0o640,
        )?;
        for (hash, bytes) in bundles {
            publish_bundle_catalog_entry(&destination, hash, &bytes)?;
        }
        Ok(())
    }
}

#[cfg(feature = "ocomp-integration")]
pub const OCOMP_MEASUREMENT_ACTIVATION_HEIGHT: u64 =
    outbe_node::ocomp::fork::GENESIS_ACTIVE_OCOMP_HEIGHT;

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompMismatchedForkManifestV1 {
    pub path: PathBuf,
    pub canonical_install_hash: B256,
    pub mismatched_install_hash: B256,
    pub canonical_activation_height: u64,
    pub mismatched_activation_height: u64,
}

/// Exact measurement manifest generated before any node process starts.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompMeasurementForkV1 {
    pub install: OcompForkInstallV1,
    pub install_hash: B256,
    pub public_worldwide_day: Option<WorldwideDay>,
}

#[cfg(feature = "ocomp-integration")]
impl OcompMeasurementForkV1 {
    #[must_use]
    pub fn launch_identity(&self) -> OcompLaunchIdentityV1 {
        OcompLaunchIdentityV1 {
            chain_id: self.install.request_profile.chain_id,
            genesis_hash: self.install.request_profile.genesis_hash,
            protocol_bundle_hash: self.install.request_profile.protocol_bundle_hash,
            fork_install_hash: self.install_hash,
            classification: self.install.classification,
            activation_height: self.install.activation_height,
            metadosis_storage_layout_hash: METADOSIS_STORAGE_LAYOUT_V1_HASH,
        }
    }
}

#[cfg(feature = "ocomp-integration")]
fn measurement_fork_install(
    chain_id: u64,
    genesis_hash: B256,
    activation_height: u64,
    validators_path: &Path,
    limits: &outbe_ocomp_protocol::SchemaLimits,
    result_deadline_blocks: u64,
) -> Result<OcompForkInstallV1> {
    let protocol_bundle = provisional_measurement_bundle();
    let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(limits)?;
    let founder_registrations =
        measurement_founder_registrations(validators_path, chain_id, genesis_hash, limits)?;
    let capacity_profile = provisional_measurement_capacity_profile(result_deadline_blocks);
    Ok(OcompForkInstallV1 {
        classification: OcompForkInstallClassification::Measurement,
        activation_height,
        request_profile: OcompRequestProfile {
            chain_id,
            genesis_hash,
            fork_id: protocol_bundle.fork_id,
            protocol_bundle_hash,
            correctness_profile_id: protocol_bundle.correctness_profile_id,
            capacity_profile,
            source_availability_policy_id: B256::repeat_byte(44),
        },
        protocol_bundle,
        founder_registrations,
    })
}

#[cfg(feature = "ocomp-integration")]
fn provisional_measurement_capacity_profile(result_deadline_blocks: u64) -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: B256::repeat_byte(13),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        ready_backoff_blocks: 1,
        max_reference_currencies: 256,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: B256::repeat_byte(23),
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn publish_exact_file(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    match fs::read(path) {
        Ok(existing) if existing == bytes => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o777 != mode {
                eyre::bail!(
                    "existing OCOMP artifact has unsafe metadata: {}",
                    path.display()
                );
            }
            Ok(())
        }
        Ok(_) => {
            eyre::bail!(
                "refusing to replace a different OCOMP artifact at {}",
                path.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn publish_bundle_catalog_entry(
    root: &Path,
    bundle_hash: B256,
    bytes: &[u8],
) -> Result<()> {
    let catalog = root.join("protocol-bundles-v1");
    fs::create_dir_all(&catalog)?;
    publish_exact_file(
        &catalog.join(format!("{}.ocb1", hex::encode(bundle_hash.as_slice()))),
        bytes,
        0o640,
    )
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn installed_protocol_bundle_hashes(
    root: &Path,
    initial_hash: B256,
) -> Result<String> {
    let catalog = root.join("protocol-bundles-v1");
    let mut hashes = std::collections::BTreeSet::from([initial_hash]);
    for entry in fs::read_dir(&catalog)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        eyre::ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "OCOMP bundle catalog contains a non-regular entry"
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| eyre::eyre!("OCOMP bundle catalog filename is not UTF-8"))?;
        let encoded = name
            .strip_suffix(".ocb1")
            .ok_or_else(|| eyre::eyre!("OCOMP bundle catalog filename has no .ocb1 suffix"))?;
        eyre::ensure!(
            encoded.len() == 64
                && encoded
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "OCOMP bundle catalog filename is not canonical lowercase hex"
        );
        hashes.insert(B256::from_slice(&hex::decode(encoded)?));
    }
    eyre::ensure!(
        !hashes.is_empty() && hashes.len() <= 2,
        "OCOMP runtime supports active plus one staged/retiring bundle"
    );
    let mut ordered = hashes.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|hash| *hash != initial_hash);
    Ok(ordered
        .into_iter()
        .map(|hash| format!("{hash:#x}"))
        .collect::<Vec<_>>()
        .join(","))
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn replace_json_atomically(
    path: &Path,
    value: &serde_json::Value,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("generated genesis has no parent directory"))?;
    let temporary = parent.join(format!(".genesis.ocomp.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(&temporary)?;
    if let Err(error) = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn provisional_measurement_bundle() -> ProtocolBundleV1 {
    let hash = |byte| B256::repeat_byte(byte);
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(1),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
        result_codec_id: hash(4),
        action_codec_id: hash(5),
        activation_codec_id: hash(6),
        evidence_codec_id: hash(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: hash(9),
        effect_contract_registry_hash: hash(10),
        object_codec_registry_hash: hash(11),
        correctness_profile_id: hash(12),
        capacity_profile_id: hash(13),
        result_signature_profile_id: hash(14),
        finality_verifier_and_vote_domain_id: hash(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: hash(16),
        mode_pause_revocation_semantics_hash: hash(17),
        upgrade_fsm_semantics_hash: hash(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: hash(19),
        release_requirement_catalog_parent_hash: hash(20),
        release_gate_authority_envelope_hash: hash(21),
        release_approval_policy_hash: hash(22),
        release_validator_command_artifact_hash: hash(23),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(24),
        required_upgrade_handler_set_hash: hash(25),
    }
}
