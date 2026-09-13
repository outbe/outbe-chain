use crate::world::ocomp::*;

impl OcompTopology {
    /// Verify the durable footprint left by one completed production job in
    /// every isolated validator domain.
    ///
    /// Development workers are deliberately short-lived: the Supervisor
    /// authenticates one, executes one unit, waits for it to exit, and then
    /// admits its output. Consequently, a post-activation E2E assertion must
    /// inspect the admitted worker outputs rather than require idle worker
    /// processes to remain alive.
    #[cfg(feature = "ocomp-integration")]
    pub fn verify_completed_job_artifacts(&self, job_id: B256) -> Result<()> {
        let bundle_hash = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is unavailable"))?
            .protocol_bundle_hash;
        self.verify_completed_job_artifacts_for_bundle(job_id, bundle_hash)
    }

    /// Verify a completed job specifically in the worker inbox selected by its
    /// consensus bundle pin. This prevents predecessor artifacts from being
    /// mistaken for successor execution evidence.
    #[cfg(feature = "ocomp-integration")]
    pub fn verify_completed_job_artifacts_for_bundle(
        &self,
        job_id: B256,
        bundle_hash: B256,
    ) -> Result<()> {
        // Existing callers retain the strict four-journal contract. Only the
        // explicitly armed canonical-proof entry point permits a late nonvoter.
        self.verify_completed_job_artifacts_inner(job_id, bundle_hash, None, &[])
    }

    /// Call while the exact bundle's workers are held, after the last node
    /// replacement and before releasing work. A late checkpoint is not proof.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn arm_completed_artifact_phase(
        &mut self,
        bundle_hash: B256,
        node_pids: BTreeMap<u8, u32>,
        budget: Duration,
    ) -> Result<()> {
        eyre::ensure!(
            self.domains.len() == 4,
            "artifact proof requires four validators"
        );
        eyre::ensure!(
            node_pids.keys().copied().collect::<Vec<_>>() == self.validator_indices()?,
            "artifact phase omitted an expected validator"
        );
        eyre::ensure!(
            node_pids.values().all(|pid| *pid != 0)
                && node_pids
                    .values()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == 4,
            "artifact phase has invalid or duplicate node identities"
        );
        eyre::ensure!(
            !self.artifact_phases.contains_key(&bundle_hash),
            "artifact phase is already armed; refusing to erase earlier evidence"
        );
        let mut nodes = BTreeMap::new();
        for (index, pid) in node_pids {
            let log = crate::internal::launch_log::LaunchLog::checkpoint(
                &self.cfg.validator_dir(usize::from(index)).join("node.log"),
            )?;
            nodes.insert(index, (pid, log));
        }
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or_else(|| eyre::eyre!("artifact observation budget overflows"))?;
        self.artifact_phases.insert(
            bundle_hash,
            OcompArtifactPhase {
                job_id: None,
                deadline,
                nodes,
            },
        );
        Ok(())
    }

    /// All four domains must retain the same successful computation. Only
    /// canonical voters must have submission journals; a missing nonvoter
    /// journal needs an exact, current-incarnation successful late outcome.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn verify_completed_artifacts_canonical(
        &mut self,
        proof: &OcompCanonicalArtifactProof,
        node_pids: &BTreeMap<u8, u32>,
    ) -> Result<Option<serde_json::Value>> {
        let job_id = proof.result.job_id;
        eyre::ensure!(
            self.domains.len() == 4,
            "artifact proof requires four validators"
        );
        eyre::ensure!(
            (3..=4).contains(&proof.voters.len())
                && proof.voters.windows(2).all(|pair| pair[0] < pair[1])
                && proof.voters.iter().all(|index| *index < 4),
            "canonical artifact voters are not a distinct four-member quorum"
        );
        let phase = self
            .artifact_phases
            .get_mut(&proof.bundle_hash)
            .ok_or_else(|| eyre::eyre!("artifact phase was not armed before work"))?;
        let deadline = phase.deadline;
        eyre::ensure!(
            Instant::now() < deadline,
            "artifact observation deadline elapsed"
        );
        eyre::ensure!(
            phase.job_id.is_none_or(|previous| previous == job_id),
            "artifact phase cannot be reused for another job"
        );
        phase.job_id = Some(job_id);
        eyre::ensure!(
            node_pids.len() == phase.nodes.len(),
            "artifact node inventory changed"
        );
        let mut late_nonvoters = Vec::new();
        let mut observations = Vec::new();
        for (&index, (pid, log)) in &mut phase.nodes {
            eyre::ensure!(
                node_pids.get(&index) == Some(pid),
                "artifact node incarnation changed"
            );
            let text = log.read()?;
            let exact_job = format!("job_id={job_id:#x}");
            let exact_digest = format!(
                "result_digest={:#x}",
                proof
                    .result
                    .result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())?
            );
            let matched = text.lines().find(|line| {
                let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
                fields.contains(&exact_job.as_str())
                    && line.contains("outbe_chain::ocomp_exex:")
                    && ((line.contains("ignored late OCOMP result before local persistence")
                        && fields.contains(&"reason=\"checkpoint_pruned\""))
                        || (line.contains("embedded OCOMP local result arrived after canonical settlement; protocol owns the job")
                            && fields.contains(&exact_digest.as_str())))
            });
            if !proof.voters.contains(&u16::from(index)) && matched.is_some() {
                late_nonvoters.push(index);
            }
            observations.push(serde_json::json!({
                "validator_index": index, "node_pid": pid,
                "log_start_offset": log.start_offset(), "late_disposition": matched,
            }));
        }
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let canonical = proof.result.encode_canonical(&limits)?;
        let result_digest = proof.result.result_digest(&limits)?;
        let reference = outbe_ocomp_protocol::CasObjectRefV1 {
            transport_digest: keccak256(&canonical),
            encoded_bytes: u64::try_from(canonical.len())?,
            expected_ocb1_kind: Some(outbe_ocomp_protocol::ObjectKind::LysisResultV1.tag()),
        };
        for index in self.validator_indices()? {
            // The production finalizer publishes this exact result to its own
            // CAS before reporting completion, even if node-v1 later prunes it.
            // Use only the read-only reader; never invoke the publishing finalizer.
            let local_result = outbe_ocomp::cas::FilesystemCasReader::open(
                self.domain_root(index)?.join("cas-v1"),
                outbe_ocomp::cas::CasLimits {
                    max_object_bytes: reference.encoded_bytes,
                    max_total_bytes: u64::MAX,
                },
            )
            .and_then(|reader| reader.read_verified(&reference));
            let local_result = match local_result {
                Ok(result) => result,
                Err(outbe_ocomp::cas::CasError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            eyre::ensure!(
                local_result.bytes() == canonical,
                "validator-{index} did not retain the exact canonical computed result"
            );
            let component = hex::encode(job_id);
            let vote_path = self
                .domain_root(index)?
                .join("supervisor-v1")
                .join("vote-submissions")
                .join(&component)
                .join(format!("{component}.vote.v1"));
            match fs::symlink_metadata(vote_path) {
                Ok(metadata) => eyre::ensure!(
                    metadata.file_type().is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() > 0,
                    "validator-{index} has an invalid vote journal"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if proof.voters.contains(&u16::from(index)) || !late_nonvoters.contains(&index)
                    {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.verify_completed_job_artifacts_inner(
            job_id,
            proof.bundle_hash,
            Some(&proof.voters),
            &late_nonvoters,
        )?;
        eyre::ensure!(
            Instant::now() < deadline,
            "artifact observation deadline elapsed"
        );
        Ok(Some(serde_json::json!({
            "job_id": job_id, "protocol_bundle_hash": proof.bundle_hash,
            "height": proof.checkpoint.height, "block_hash": proof.checkpoint.block_hash,
            "state_root": proof.checkpoint.state_root, "canonical_voters": proof.voters,
            "result_digest": result_digest, "result_transport_digest": reference.transport_digest,
            "nodes": observations,
        })))
    }

    #[cfg(feature = "ocomp-integration")]
    fn verify_completed_job_artifacts_inner(
        &self,
        job_id: B256,
        bundle_hash: B256,
        canonical_voters: Option<&[u16]>,
        late_nonvoters: &[u8],
    ) -> Result<()> {
        let job_component = hex::encode(job_id);
        let mut expected_admissions = None;
        let mut expected_worker_outputs = None;
        let mut physical_files = BTreeMap::<String, Vec<(u64, u64)>>::new();

        for validator_index in self.validator_indices()? {
            let root = self.domain_root(validator_index)?;
            let job_root = root.join("supervisor-v1").join("jobs").join(&job_component);
            let admissions = fingerprint_regular_directory(
                &job_root.join("admissions"),
                "admission",
                validator_index,
                &mut physical_files,
            )?;
            eyre::ensure!(
                admissions
                    .iter()
                    .any(|entry| entry.name.ends_with(".admission")),
                "validator-{validator_index} has no admitted units for job {job_id:#x}"
            );

            let worker_outputs = fingerprint_regular_directory(
                &root
                    .join("worker-inbox-v1")
                    .join(hex::encode(bundle_hash))
                    .join("artifacts"),
                "worker-output",
                validator_index,
                &mut physical_files,
            )?;
            eyre::ensure!(
                !worker_outputs.is_empty(),
                "validator-{validator_index} has no authenticated worker outputs for job \
                 {job_id:#x}"
            );

            let vote_path = root
                .join("supervisor-v1")
                .join("vote-submissions")
                .join(&job_component)
                .join(format!("{job_component}.vote.v1"));
            match fs::symlink_metadata(&vote_path) {
                Ok(metadata) => eyre::ensure!(
                    metadata.file_type().is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() > 0,
                    "validator-{validator_index} has an invalid vote journal for job {job_id:#x}"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    eyre::ensure!(
                        canonical_voters.is_some_and(|voters| {
                            !voters.contains(&u16::from(validator_index))
                        }) && late_nonvoters.contains(&validator_index),
                        "validator-{validator_index} lacks a required vote journal or exact successful late disposition for job {job_id:#x}"
                    );
                }
                Err(error) => return Err(error.into()),
            }

            match &expected_admissions {
                Some(expected) => eyre::ensure!(
                    expected == &admissions,
                    "validator-{validator_index} admitted a different deterministic job trace"
                ),
                None => expected_admissions = Some(admissions),
            }
            match &expected_worker_outputs {
                Some(expected) => eyre::ensure!(
                    expected == &worker_outputs,
                    "validator-{validator_index} retained different deterministic worker outputs"
                ),
                None => expected_worker_outputs = Some(worker_outputs),
            }
        }

        for (logical_file, identities) in physical_files {
            let unique = identities
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            eyre::ensure!(
                unique.len() == self.domains.len(),
                "{logical_file} is shared by hard link across validator domains"
            );
        }
        Ok(())
    }

    /// Durable fatal-evidence directory owned by the embedded FullNode ExEx.
    #[cfg(feature = "ocomp-integration")]
    pub fn keyless_full_node_fatal_evidence_root(&self, validator_index: u8) -> Result<PathBuf> {
        Ok(self
            .keyless_full_node_domain(validator_index)?
            .root
            .join("node-v1")
            .join("fatal-evidence"))
    }

    /// Restart the real exporter with its acknowledged export left intact.
    /// Prepared-before-commit recovery is covered separately by the existing
    /// export_receipt integration tests; deleting a receipt after ACK does not
    /// reproduce that crash window.
    #[cfg(feature = "ocomp-integration")]
    pub fn verify_committed_exporter_restart(
        &mut self,
        validator_index: u8,
        job_id: B256,
    ) -> Result<()> {
        let receipt_root = self
            .domain_root(validator_index)?
            .join("exporter-v1")
            .join("receipts")
            .join(hex::encode(job_id));
        let prepared_path = receipt_root.join("prepared.ref");
        let receipt_path = receipt_root.join("receipt.ref");
        let prepared_before = fs::read(&prepared_path).map_err(|error| {
            eyre::eyre!(
                "read validator-{validator_index} prepared export {}: {error}",
                prepared_path.display()
            )
        })?;
        let receipt_before = fs::read(&receipt_path).map_err(|error| {
            eyre::eyre!(
                "read validator-{validator_index} committed export {}: {error}",
                receipt_path.display()
            )
        })?;
        eyre::ensure!(
            !prepared_before.is_empty() && !receipt_before.is_empty(),
            "prepared export references must be non-empty"
        );

        self.apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index })?;
        self.restart_snapshot_exporter(validator_index)?;

        self.ensure_validator_roles_alive()?;
        eyre::ensure!(
            fs::read(&receipt_path)? == receipt_before,
            "exporter restart changed the committed export reference"
        );
        eyre::ensure!(
            fs::read(&prepared_path)? == prepared_before,
            "exporter restart changed the preparation reference"
        );
        Ok(())
    }

    /// Current process inventory, including already stopped owned processes.
    #[must_use]
    pub fn process_records(&self) -> &[OcompProcessRecordV1] {
        &self.records
    }

    /// Bounded, serializable process/correlation snapshot for scenario evidence.
    pub fn evidence_snapshot(&self) -> Result<OcompScenarioTopologyV1> {
        let mut domain_roots = Vec::with_capacity(self.domains.len());
        for validator_index in self.validator_indices()? {
            domain_roots.push(
                self.domain_root(validator_index)?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Ok(OcompScenarioTopologyV1 {
            launch_identity: self.launch_identity_evidence.clone(),
            domain_roots,
            processes: self.records.clone(),
            faults: self.faults.clone(),
            fork_restart: self.fork_restart_evidence.clone(),
            fork_mismatch: self.fork_mismatch_evidence.clone(),
            correlated_tribute: self.correlated_tribute.clone(),
        })
    }

    /// Retains one validated H-1/H/H+1 restart observation for scenario evidence.
    pub fn record_fork_restart_evidence(
        &mut self,
        evidence: OcompForkRestartEvidenceV1,
    ) -> Result<()> {
        self.domain(evidence.validator_index)?;
        evidence.validate(self.domains.len())?;
        let identity = self.launch_identity_evidence.as_ref().ok_or_else(|| {
            eyre::eyre!("OCOMP fork evidence requires the exact OCOMP launch identity")
        })?;
        evidence.validate_launch_identity(identity)?;
        if self.fork_restart_evidence.is_some() {
            eyre::bail!("OCOMP fork restart evidence was already recorded");
        }
        self.fork_restart_evidence = Some(evidence);
        Ok(())
    }

    pub fn record_fork_mismatch_evidence(
        &mut self,
        evidence: OcompForkMismatchEvidenceV1,
    ) -> Result<()> {
        self.domain(evidence.validator_index)?;
        evidence.validate(self.domains.len())?;
        let identity = self.launch_identity_evidence.as_ref().ok_or_else(|| {
            eyre::eyre!("OCOMP fork evidence requires the exact OCOMP launch identity")
        })?;
        evidence.validate_launch_identity(identity)?;
        if self.fork_mismatch_evidence.is_some() {
            eyre::bail!("OCOMP fork mismatch evidence was already recorded");
        }
        self.fork_mismatch_evidence = Some(evidence);
        Ok(())
    }

    /// Record the successful public Tribute transaction and finalized anchor.
    pub fn observe_public_tribute(
        &mut self,
        evidence: PublicTributeCorrelationV1,
    ) -> Result<(), CorrelationError> {
        self.ensure_correlation_open()?;
        self.tribute_correlation.record_public_tribute(evidence)
    }

    /// Record one independently verified validator RocksDB/CE source package.
    pub fn observe_validator_source(
        &mut self,
        evidence: ValidatorSourceCorrelationV1,
    ) -> Result<(), CorrelationError> {
        self.ensure_correlation_open()?;
        self.tribute_correlation.record_validator_source(evidence)
    }

    /// Bind the production-observed JobIntent only after every pinned source exists.
    pub fn observe_job_intent(
        &mut self,
        evidence: JobIntentCorrelationV1,
    ) -> Result<(), CorrelationError> {
        self.ensure_correlation_open()?;
        self.tribute_correlation.record_job_intent(evidence)
    }

    /// Close and retain the public-Tribute prefix for later bundle publication.
    pub fn close_tribute_correlation(
        &mut self,
    ) -> Result<&CorrelatedTributeFixtureV1, CorrelationError> {
        if self.correlated_tribute.is_none() {
            self.correlated_tribute = Some(self.tribute_correlation.clone().finish()?);
        }
        Ok(self.correlated_tribute.as_ref().expect("set above"))
    }

    /// Read-only closed fixture correlation, if the production path reached it.
    #[must_use]
    pub fn correlated_tribute(&self) -> Option<&CorrelatedTributeFixtureV1> {
        self.correlated_tribute.as_ref()
    }

    fn ensure_correlation_open(&self) -> Result<(), CorrelationError> {
        if self.correlated_tribute.is_some() {
            return Err(CorrelationError::CorrelationClosed);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompFaultRecordV1 {
    pub fault: OcompProcessFault,
    pub applied_at_millis: u64,
}

/// Exact validator restart observations around the measurement fork height.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompForkRestartEvidenceV1 {
    pub validator_index: u8,
    pub activation_height: u64,
    pub pre_fork_restart_from_height: u64,
    pub pre_fork_rejoined_height: u64,
    pub down_across_fork_from_height: u64,
    pub finalized_while_down_height: u64,
    pub replayed_through_height: u64,
    pub post_fork_restart_from_height: u64,
    pub post_fork_rejoined_height: u64,
}

impl OcompForkRestartEvidenceV1 {
    fn validate(&self, validator_count: usize) -> Result<()> {
        if usize::from(self.validator_index) >= validator_count {
            eyre::bail!("OCOMP fork restart validator index is outside the committee");
        }
        if self.activation_height == 0
            || self.pre_fork_restart_from_height >= self.activation_height
            || self.pre_fork_rejoined_height >= self.activation_height
            || self.pre_fork_rejoined_height < self.pre_fork_restart_from_height
            || self.down_across_fork_from_height >= self.activation_height
            || self.finalized_while_down_height < self.activation_height
            || self.replayed_through_height < self.finalized_while_down_height
            || self.post_fork_restart_from_height < self.activation_height
            || self.post_fork_rejoined_height < self.post_fork_restart_from_height
        {
            eyre::bail!("OCOMP fork restart evidence does not span H-1/H/H+1 safely");
        }
        Ok(())
    }

    fn validate_launch_identity(&self, identity: &OcompLaunchIdentityEvidenceV1) -> Result<()> {
        if self.activation_height != identity.activation_height {
            eyre::bail!("OCOMP fork restart evidence does not match the launch identity");
        }
        Ok(())
    }
}

/// Behavioral proof that one valid but different immutable fork install cannot
/// follow the canonical committee through its activation height.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompForkMismatchEvidenceV1 {
    pub validator_index: u8,
    pub canonical_install_hash: String,
    pub mismatched_install_hash: String,
    pub canonical_activation_height: u64,
    pub mismatched_activation_height: u64,
    pub canonical_head_before_restart: u64,
    pub mismatched_head_after_fork: u64,
    pub canonical_finalized_after_fork: u64,
}

impl OcompForkMismatchEvidenceV1 {
    pub fn validate(&self, validator_count: usize) -> Result<()> {
        if usize::from(self.validator_index) >= validator_count {
            eyre::bail!("OCOMP fork mismatch validator index is outside the committee");
        }
        if self.canonical_install_hash.is_empty()
            || self.mismatched_install_hash.is_empty()
            || self.canonical_install_hash == self.mismatched_install_hash
            || self.canonical_activation_height == 0
            || self.mismatched_activation_height != self.canonical_activation_height
        {
            eyre::bail!("OCOMP fork mismatch evidence has no distinct valid install");
        }
        if self.canonical_head_before_restart >= self.canonical_activation_height
            || self.mismatched_head_after_fork >= self.canonical_activation_height
            || self.canonical_finalized_after_fork
                < self.canonical_activation_height.saturating_add(1)
            || self.canonical_finalized_after_fork <= self.mismatched_head_after_fork
        {
            eyre::bail!("OCOMP fork mismatch evidence does not prove fail-closed isolation");
        }
        Ok(())
    }

    fn validate_launch_identity(&self, identity: &OcompLaunchIdentityEvidenceV1) -> Result<()> {
        if self.canonical_activation_height != identity.activation_height
            || self.canonical_install_hash != identity.fork_install_hash
        {
            eyre::bail!("OCOMP fork mismatch evidence does not match the launch identity");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompScenarioTopologyV1 {
    pub launch_identity: Option<OcompLaunchIdentityEvidenceV1>,
    pub domain_roots: Vec<String>,
    pub processes: Vec<OcompProcessRecordV1>,
    pub faults: Vec<OcompFaultRecordV1>,
    pub fork_restart: Option<OcompForkRestartEvidenceV1>,
    pub fork_mismatch: Option<OcompForkMismatchEvidenceV1>,
    pub correlated_tribute: Option<CorrelatedTributeFixtureV1>,
}

impl OcompScenarioTopologyV1 {
    pub fn validate(&self) -> Result<()> {
        let validator_count = self.domain_roots.len();
        eyre::ensure!(
            validator_count > 0,
            "OCOMP topology has no validator domains"
        );
        if let Some(identity) = &self.launch_identity {
            if !matches!(identity.classification.as_str(), "measurement" | "final")
                || identity.activation_height == 0
                || identity.metadosis_storage_layout_hash != METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX
            {
                eyre::bail!("OCOMP launch identity has an invalid genesis profile binding");
            }
        }
        if let Some(restart) = &self.fork_restart {
            restart.validate(validator_count)?;
            let identity = self.launch_identity.as_ref().ok_or_else(|| {
                eyre::eyre!("OCOMP fork evidence requires the exact OCOMP launch identity")
            })?;
            restart.validate_launch_identity(identity)?;
        }
        if let Some(mismatch) = &self.fork_mismatch {
            mismatch.validate(validator_count)?;
            let identity = self.launch_identity.as_ref().ok_or_else(|| {
                eyre::eyre!("OCOMP fork evidence requires the exact OCOMP launch identity")
            })?;
            mismatch.validate_launch_identity(identity)?;
        }
        Ok(())
    }
}

/// Evidence-safe process lifecycle record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompProcessRecordV1 {
    pub validator_index: Option<u8>,
    pub role: OcompProcessRole,
    pub worker_ordinal: Option<u32>,
    pub pid: u32,
    pub started_at_millis: u64,
    pub stopped_at_millis: Option<u64>,
}

/// One pre-work observation; never carried across a node replacement.
#[cfg(feature = "ocomp-integration")]
#[derive(Debug)]
pub(in crate::world::ocomp) struct OcompArtifactPhase {
    job_id: Option<B256>,
    deadline: Instant,
    nodes: BTreeMap<u8, (u32, crate::internal::launch_log::LaunchLog)>,
}

/// Authority read by the feature from one shared finalized checkpoint.
#[cfg(feature = "ocomp-integration")]
pub(crate) struct OcompCanonicalArtifactProof {
    pub checkpoint: crate::world::rpc::FinalizedCheckpoint,
    pub bundle_hash: B256,
    pub result: outbe_ocomp_protocol::result::LysisResultV1,
    pub voters: Vec<u16>,
}

pub(in crate::world::ocomp) fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct DurableFileFingerprintV1 {
    name: String,
    encoded_bytes: u64,
    transport_digest: B256,
}

#[cfg(feature = "ocomp-integration")]
fn fingerprint_regular_directory(
    directory: &Path,
    logical_kind: &str,
    validator_index: u8,
    physical_files: &mut BTreeMap<String, Vec<(u64, u64)>>,
) -> Result<Vec<DurableFileFingerprintV1>> {
    let directory_metadata = fs::symlink_metadata(directory)?;
    eyre::ensure!(
        directory_metadata.file_type().is_dir() && !directory_metadata.file_type().is_symlink(),
        "validator-{validator_index} {logical_kind} directory is not a safe directory"
    );

    let mut fingerprints = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        eyre::ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "validator-{validator_index} {logical_kind} entry is not a regular file: {}",
            path.display()
        );
        let name = entry.file_name().into_string().map_err(|_| {
            eyre::eyre!("validator-{validator_index} {logical_kind} file name is not UTF-8")
        })?;
        let bytes = fs::read(&path)?;
        physical_files
            .entry(format!("{logical_kind}/{name}"))
            .or_default()
            .push((metadata.dev(), metadata.ino()));
        fingerprints.push(DurableFileFingerprintV1 {
            name,
            encoded_bytes: metadata.len(),
            transport_digest: keccak256(bytes),
        });
    }
    fingerprints.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(fingerprints)
}
