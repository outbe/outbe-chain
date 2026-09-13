use crate::world::ocomp::*;

impl OcompTopology {
    /// Launch the external compute clients for every genesis ACTIVE validator:
    /// one SnapshotExporter and Worker ordinal 0. Each node owns its embedded
    /// Supervisor and Worker endpoint.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_baseline_runtime(&mut self, identity: OcompLaunchIdentityV1) -> Result<()> {
        self.install_ocomp_delegate_bindings()?;
        self.start_validator_roles(identity)?;
        for validator_index in self.validator_indices()? {
            self.activate_worker(validator_index, 0, identity)?;
        }
        Ok(())
    }

    /// Start the external SnapshotExporter in every validator domain after the
    /// corresponding Node-owned embedded Supervisor endpoint is ready.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_validator_roles(&mut self, identity: OcompLaunchIdentityV1) -> Result<()> {
        if !self.cfg.bin_ocomp.is_file() {
            eyre::bail!(
                "outbe-ocomp binary does not exist: {}",
                self.cfg.bin_ocomp.display()
            );
        }
        if self.launch_identity.is_some() {
            eyre::bail!("OCOMP validator runtime was already started for this scenario");
        }
        self.launch_identity = Some(identity);
        self.launch_identity_evidence = Some(OcompLaunchIdentityEvidenceV1 {
            chain_id: identity.chain_id,
            genesis_hash: format!("{:#x}", identity.genesis_hash),
            protocol_bundle_hash: format!("{:#x}", identity.protocol_bundle_hash),
            fork_install_hash: format!("{:#x}", identity.fork_install_hash),
            classification: match identity.classification {
                OcompForkInstallClassification::Measurement => "measurement",
                OcompForkInstallClassification::Final => "final",
            }
            .to_owned(),
            activation_height: identity.activation_height,
            metadosis_storage_layout_hash: format!("{:#x}", identity.metadosis_storage_layout_hash),
        });
        for validator_index in self.validator_indices()? {
            self.start_validator_roles_for_domain(validator_index, identity)?;
        }

        sleep(Duration::from_secs(2));
        self.ensure_validator_roles_alive()
    }

    /// Start the external OCOMP exporter for a validator only after the
    /// certified boundary has made it ACTIVE and its domain has been appended.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_active_validator_roles(&mut self, validator_index: u8) -> Result<()> {
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        self.start_validator_roles_for_domain(validator_index, identity)?;
        sleep(Duration::from_secs(2));
        self.ensure_validator_roles_alive()
    }

    /// Start the exact keyless compute plane required by a certified FullNode.
    /// The FullNode owns its embedded Supervisor; the harness starts only its
    /// external SnapshotExporter and Worker and provides no vote keys.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_keyless_full_node_roles(&mut self, validator_index: u8) -> Result<()> {
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let domain = self.keyless_full_node_domain(validator_index)?;
        eyre::ensure!(
            domain.snapshot_exporter.is_none() && domain.workers.is_empty(),
            "keyless FullNode OCOMP roles are already started"
        );
        let exporter = self.spawn_keyless_full_node_exporter(validator_index, identity)?;
        self.attach_keyless_full_node_owned(
            validator_index,
            OcompProcessRole::SnapshotExporter,
            None,
            exporter,
        )?;
        let worker = self.spawn_worker_process(
            validator_index,
            0,
            0,
            self.keyless_full_node_domain(validator_index)?.root.clone(),
            identity,
        )?;
        self.attach_keyless_full_node_owned(
            validator_index,
            OcompProcessRole::Worker,
            Some(0),
            worker,
        )?;
        if let Some(successor) = self.successor_identity {
            self.start_keyless_successor_worker(validator_index, successor)?;
        }
        sleep(Duration::from_secs(2));
        self.ensure_keyless_full_node_roles_alive(validator_index)
    }

    #[cfg(feature = "ocomp-integration")]
    fn start_validator_roles_for_domain(
        &mut self,
        validator_index: u8,
        identity: OcompLaunchIdentityV1,
    ) -> Result<()> {
        let domain = self.domain(validator_index)?;
        eyre::ensure!(
            domain.snapshot_exporter.is_none(),
            "validator-{validator_index} OCOMP runtime is already started"
        );

        let exporter = self.spawn_validator_role(
            validator_index,
            OcompProcessRole::SnapshotExporter,
            identity,
        )?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::SnapshotExporter,
            None,
            exporter,
        )?;
        Ok(())
    }

    /// Activate one production worker through the same inherited-FD boundary
    /// used by the Supervisor. The authenticated control session remains
    /// private to the topology so Cucumber steps cannot inject work.
    #[cfg(feature = "ocomp-integration")]
    pub fn activate_worker(
        &mut self,
        validator_index: u8,
        worker_ordinal: u32,
        identity: OcompLaunchIdentityV1,
    ) -> Result<()> {
        self.require_launch_identity(identity)?;
        let domain = self.domain(validator_index)?;
        if domain.workers.contains_key(&worker_ordinal) {
            eyre::bail!(
                "validator-{validator_index} worker ordinal {worker_ordinal} is already active"
            );
        }
        if domain.workers.len() >= OCOMP_MAX_WORKERS_PER_DOMAIN {
            eyre::bail!(
                "validator-{validator_index} reached the bounded worker concurrency limit \
                 {OCOMP_MAX_WORKERS_PER_DOMAIN}"
            );
        }

        let domain_root = domain.root.clone();
        let guard =
            self.spawn_worker_process(validator_index, worker_ordinal, 0, domain_root, identity)?;

        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Worker,
            Some(worker_ordinal),
            guard,
        )?;
        Ok(())
    }

    /// Starts one Worker on the successor bundle lane in every validator and
    /// the optional keyless FullNode domain. Ordinal 1 is process-local; it
    /// does not add the FullNode to validator membership. The lane has its own
    /// Supervisor registry and endpoint range.
    #[cfg(feature = "ocomp-integration")]
    pub fn activate_successor_workers(
        &mut self,
        successor_identity: OcompLaunchIdentityV1,
    ) -> Result<()> {
        let current = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        eyre::ensure!(
            successor_identity.chain_id == current.chain_id
                && successor_identity.genesis_hash == current.genesis_hash
                && successor_identity.protocol_bundle_hash != current.protocol_bundle_hash,
            "OCOMP successor Worker identity is not bound to this domain"
        );
        eyre::ensure!(
            self.successor_identity.is_none(),
            "successor Workers already activated"
        );
        // Check the entire expected inventory before starting any new child.
        for validator_index in self.validator_indices()? {
            eyre::ensure!(
                !self.domain(validator_index)?.workers.contains_key(&1),
                "validator-{validator_index} successor Worker is already active"
            );
        }
        if let Some((index, domain)) = &self.keyless_full_node_domain {
            eyre::ensure!(
                domain.snapshot_exporter.is_some()
                    && domain.workers.contains_key(&0)
                    && !domain.workers.contains_key(&1),
                "FullNode {index} must have restored V1 clients before successor activation"
            );
        }
        for validator_index in self.validator_indices()? {
            let worker_ordinal = 1;
            let domain = self.domain(validator_index)?;
            eyre::ensure!(
                !domain.workers.contains_key(&worker_ordinal),
                "validator-{validator_index} successor Worker is already active"
            );
            let guard = self.spawn_worker_process(
                validator_index,
                worker_ordinal,
                1,
                domain.root.clone(),
                successor_identity,
            )?;
            self.attach_owned(
                Some(validator_index),
                OcompProcessRole::Worker,
                Some(worker_ordinal),
                guard,
            )?;
        }
        if let Some((index, _)) = &self.keyless_full_node_domain {
            self.start_keyless_successor_worker(*index, successor_identity)?;
        }
        self.successor_identity = Some(successor_identity);
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn start_keyless_successor_worker(
        &mut self,
        index: u8,
        identity: OcompLaunchIdentityV1,
    ) -> Result<()> {
        let domain = self.keyless_full_node_domain(index)?;
        eyre::ensure!(
            !domain.workers.contains_key(&1),
            "FullNode V2 Worker already active"
        );
        let guard = self.spawn_worker_process(index, 1, 1, domain.root.clone(), identity)?;
        self.attach_keyless_full_node_owned(index, OcompProcessRole::Worker, Some(1), guard)
    }

    #[cfg(feature = "ocomp-integration")]
    pub(in crate::world::ocomp) fn spawn_worker_process(
        &self,
        validator_index: u8,
        worker_ordinal: u32,
        bundle_lane: u16,
        domain_root: PathBuf,
        identity: OcompLaunchIdentityV1,
    ) -> Result<ChildGuard> {
        let log_path = domain_root.join(format!("worker-{worker_ordinal}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;
        let base_port = self.cfg.ocomp_endpoint_port(usize::from(validator_index));
        let lane_stride = u16::try_from(OCOMP_MAX_WORKERS_PER_DOMAIN)?
            .checked_add(2)
            .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port stride overflow"))?;
        let supervisor_port = base_port
            .checked_add(
                bundle_lane
                    .checked_mul(lane_stride)
                    .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port offset overflow"))?,
            )
            .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port overflow"))?;
        let supervisor_address = std::net::SocketAddr::from(([127, 0, 0, 1], supervisor_port));
        let worker_boot_nonce = worker_boot_nonce(validator_index, worker_ordinal);
        let expected_observability_port = self
            .cfg
            .ocomp_worker_port(usize::from(validator_index), worker_ordinal);
        if bundle_lane == 0 {
            debug_assert_eq!(
                supervisor_address.port() + 2 + u16::try_from(worker_ordinal).unwrap(),
                expected_observability_port
            );
        }

        let mut command = self.release_role_command(validator_index);
        command
            .arg("worker")
            .arg("--chain-id")
            .arg(identity.chain_id.to_string())
            .arg("--genesis-hash")
            .arg(format!("{:#x}", identity.genesis_hash))
            .arg("--boot-nonce")
            .arg(format!("{worker_boot_nonce:#x}"))
            .arg("--worker-ordinal")
            .arg(worker_ordinal.to_string())
            .arg("--protocol-bundle-hash")
            .arg(format!("{:#x}", identity.protocol_bundle_hash))
            .arg("--supervisor-address")
            .arg(supervisor_address.to_string())
            .current_dir(&self.cfg.repo)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        if self.cfg.debug {
            eprintln!(
                "[ocomp] activate validator-{validator_index} worker-{worker_ordinal}: {}",
                self.cfg.bin_ocomp.display()
            );
        }
        ChildGuard::spawn(
            format!("validator-{validator_index} OCOMP worker-{worker_ordinal}"),
            command,
        )
    }

    #[cfg(feature = "ocomp-integration")]
    fn release_role_command(&self, validator_index: u8) -> Command {
        let mut command = Command::new(&self.cfg.bin_ocomp);
        configure_release_layout(&mut command, &self.cfg.dir, validator_index);
        command
    }

    #[cfg(feature = "ocomp-integration")]
    pub(in crate::world::ocomp) fn spawn_validator_role(
        &mut self,
        validator_index: u8,
        role: OcompProcessRole,
        identity: OcompLaunchIdentityV1,
    ) -> Result<ChildGuard> {
        let domain_root = self.domain(validator_index)?.root.clone();
        eyre::ensure!(
            role == OcompProcessRole::SnapshotExporter,
            "validator service launcher accepts only the external SnapshotExporter role"
        );
        let role_name = "snapshot-exporter";
        let log_path = domain_root.join(format!("{role_name}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;

        let supervisor_address = SocketAddr::from((
            [127, 0, 0, 1],
            self.cfg.ocomp_endpoint_port(usize::from(validator_index)),
        ));
        let mut command = self.release_role_command(validator_index);
        configure_snapshot_exporter_command(&mut command, supervisor_address);
        command
            .current_dir(&self.cfg.repo)
            .env("OCOMP_CHAIN_ID", identity.chain_id.to_string())
            .env(
                "OCOMP_GENESIS_HASH",
                format!("{:#x}", identity.genesis_hash),
            )
            .env(
                "OCOMP_BOOT_NONCE",
                format!(
                    "{:#x}",
                    B256::repeat_byte(validator_index.saturating_add(1))
                ),
            )
            .env(
                "OCOMP_PROTOCOL_BUNDLE_HASHES",
                installed_protocol_bundle_hashes(&domain_root, identity.protocol_bundle_hash)?,
            )
            .env("OCOMP_REGISTRY_GENERATION", "1");
        let validator_index = usize::from(validator_index);
        command.env("OUTBE_OCOMP_RPC_URL", self.cfg.rpc_url(validator_index));
        configure_snapshot_exporter_projection(&mut command, &self.cfg, validator_index)?;
        if self.cfg.debug {
            eprintln!(
                "[ocomp] launch validator-{validator_index} {role_name}: {}",
                self.cfg.bin_ocomp.display()
            );
        }
        command.stdout(Stdio::from(log)).stderr(Stdio::from(stderr));
        let guard = ChildGuard::spawn(
            format!("validator-{validator_index} OCOMP {role_name}"),
            command,
        )?;
        Ok(guard)
    }

    #[cfg(feature = "ocomp-integration")]
    fn spawn_keyless_full_node_exporter(
        &self,
        validator_index: u8,
        identity: OcompLaunchIdentityV1,
    ) -> Result<ChildGuard> {
        let domain_root = self.keyless_full_node_domain(validator_index)?.root.clone();
        let role_name = "snapshot-exporter";
        let log_path = domain_root.join(format!("{role_name}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;
        let index = usize::from(validator_index);
        let supervisor_address =
            SocketAddr::from(([127, 0, 0, 1], self.cfg.ocomp_endpoint_port(index)));
        let mut command = self.release_role_command(validator_index);
        configure_snapshot_exporter_command(&mut command, supervisor_address);
        command
            .current_dir(&self.cfg.repo)
            .env("OCOMP_CHAIN_ID", identity.chain_id.to_string())
            .env(
                "OCOMP_GENESIS_HASH",
                format!("{:#x}", identity.genesis_hash),
            )
            .env(
                "OCOMP_BOOT_NONCE",
                format!(
                    "{:#x}",
                    B256::repeat_byte(validator_index.saturating_add(1))
                ),
            )
            .env(
                "OCOMP_PROTOCOL_BUNDLE_HASHES",
                installed_protocol_bundle_hashes(&domain_root, identity.protocol_bundle_hash)?,
            );
        command.env("OUTBE_OCOMP_RPC_URL", self.cfg.rpc_url(index));
        configure_snapshot_exporter_projection(&mut command, &self.cfg, index)?;
        command.stdout(Stdio::from(log)).stderr(Stdio::from(stderr));
        ChildGuard::spawn(
            format!("full-node-{validator_index} OCOMP {role_name}"),
            command,
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub(in crate::world::ocomp) fn attach_keyless_full_node_owned(
        &mut self,
        validator_index: u8,
        role: OcompProcessRole,
        worker_ordinal: Option<u32>,
        guard: ChildGuard,
    ) -> Result<()> {
        let domain = self.keyless_full_node_domain(validator_index)?;
        match (role, worker_ordinal) {
            (OcompProcessRole::SnapshotExporter, None) if domain.snapshot_exporter.is_none() => {}
            (OcompProcessRole::Worker, Some(ordinal @ 0..=1))
                if !domain.workers.contains_key(&ordinal) => {}
            _ => {
                eyre::bail!("invalid or duplicate keyless FullNode role attachment");
            }
        }
        let record_index = self.records.len();
        self.records.push(OcompProcessRecordV1 {
            validator_index: Some(validator_index),
            role,
            worker_ordinal,
            pid: guard.pid(),
            started_at_millis: unix_time_millis(),
            stopped_at_millis: None,
        });
        let process = OwnedProcess {
            guard,
            record_index,
        };
        let domain = self.keyless_full_node_domain_mut(validator_index)?;
        match (role, worker_ordinal) {
            (OcompProcessRole::SnapshotExporter, None) => domain.snapshot_exporter = Some(process),
            (OcompProcessRole::Worker, Some(ordinal)) => {
                domain.workers.insert(ordinal, process);
            }
            _ => unreachable!("role and ordinal validated before recording ownership"),
        }
        Ok(())
    }

    #[cfg(any(feature = "ocomp-integration", test))]
    pub(in crate::world::ocomp) fn attach_owned(
        &mut self,
        validator_index: Option<u8>,
        role: OcompProcessRole,
        worker_ordinal: Option<u32>,
        guard: ChildGuard,
    ) -> Result<()> {
        match role {
            OcompProcessRole::SnapshotExporter => {
                let index = validator_index
                    .ok_or_else(|| eyre::eyre!("snapshot exporter requires a validator index"))?;
                if worker_ordinal.is_some() || self.domain(index)?.snapshot_exporter.is_some() {
                    eyre::bail!("invalid or duplicate snapshot exporter attachment");
                }
            }
            OcompProcessRole::Worker => {
                let index = validator_index
                    .ok_or_else(|| eyre::eyre!("worker requires a validator index"))?;
                let ordinal =
                    worker_ordinal.ok_or_else(|| eyre::eyre!("worker ordinal missing"))?;
                if self.domain(index)?.workers.contains_key(&ordinal) {
                    eyre::bail!("worker ordinal is already attached");
                }
            }
        }

        let record_index = self.records.len();
        self.records.push(OcompProcessRecordV1 {
            validator_index,
            role,
            worker_ordinal,
            pid: guard.pid(),
            started_at_millis: unix_time_millis(),
            stopped_at_millis: None,
        });
        let process = OwnedProcess {
            guard,
            record_index,
        };
        match role {
            OcompProcessRole::SnapshotExporter => {
                self.domain_mut(validator_index.expect("validated above"))?
                    .snapshot_exporter = Some(process);
            }
            OcompProcessRole::Worker => {
                let ordinal = worker_ordinal.expect("validated above");
                if self
                    .domain_mut(validator_index.expect("validated above"))?
                    .workers
                    .insert(ordinal, process)
                    .is_some()
                {
                    unreachable!("duplicate worker was rejected before recording evidence");
                }
            }
        }
        Ok(())
    }
}

#[cfg(feature = "ocomp-integration")]
const OCOMP_MAX_WORKERS_PER_DOMAIN: usize = 4;

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) const OCOMP_BASE_PATH_ENV: &str = "OUTBE_OCOMP_BASE_PATH";

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) const OCOMP_VALIDATOR_INDEX_ENV: &str = "OCOMP_VALIDATOR_INDEX";

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn configure_release_layout(
    command: &mut Command,
    base_path: &Path,
    validator_index: u8,
) {
    command
        .env(OCOMP_BASE_PATH_ENV, base_path)
        .env(OCOMP_VALIDATOR_INDEX_ENV, validator_index.to_string());
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn configure_snapshot_exporter_command(
    command: &mut Command,
    supervisor_address: SocketAddr,
) {
    command
        .arg("snapshot-exporter")
        .arg("--supervisor-address")
        .arg(supervisor_address.to_string());
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn configure_snapshot_exporter_projection(
    command: &mut Command,
    cfg: &Config,
    validator_index: usize,
) -> Result<()> {
    crate::world::projection::rocksdb_config(cfg, validator_index)?;
    command.env(
        "OUTBE_OCOMP_STORAGE_CONFIG",
        cfg.projection_storage_config(validator_index),
    );
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn worker_boot_nonce(validator_index: u8, worker_ordinal: u32) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[0] = validator_index.saturating_add(1);
    bytes[28..].copy_from_slice(&worker_ordinal.to_be_bytes());
    B256::from(bytes)
}
