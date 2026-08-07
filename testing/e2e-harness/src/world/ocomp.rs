//! Scenario-owned OCOMP process topology.
//!
//! The handle exposes only fixed roles and typed fault operations. It has no
//! method that can insert a JobIntent, result, root or chain state; scenarios
//! must observe those values through the production RPC/control/artifact path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::Result;
use serde::{Deserialize, Serialize};

#[cfg(feature = "ocomp-integration")]
use alloy_consensus::TxEip1559;
#[cfg(feature = "ocomp-integration")]
use alloy_eips::eip2718::Encodable2718 as _;
#[cfg(feature = "ocomp-integration")]
use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
#[cfg(feature = "ocomp-integration")]
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
#[cfg(feature = "ocomp-integration")]
use outbe_common::WorldwideDay;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::config::{
    OcompForkInstallClassification, OcompForkInstallV1, OcompRequestProfile,
    OCOMP_POC_FINAL_ACTIVATION_HEIGHT,
};
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::genesis::FreshDevnetGenesisBuilder;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH;
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::{
    activation::SignOncePurpose,
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::BoundedBytes,
    profile::{CapacityProfileV1, ProtocolBundleV1},
    registry::{FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID},
    vote::{ResultVoteSigningSubjectV1, ResultVoteV1},
    PreparedVoteTransactionV1,
};
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, TRIBUTE_ADDRESS, VALIDATOR_SET_ADDRESS},
    signer::OutbeEvmSigner,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    OutbeHeader,
};
#[cfg(feature = "ocomp-integration")]
use std::fs::{self, OpenOptions};
#[cfg(feature = "ocomp-integration")]
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
#[cfg(feature = "ocomp-integration")]
use std::net::{SocketAddr, TcpStream};
#[cfg(feature = "ocomp-integration")]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
#[cfg(feature = "ocomp-integration")]
use std::process::{Command, Stdio};
#[cfg(feature = "ocomp-integration")]
use std::str::FromStr as _;
#[cfg(feature = "ocomp-integration")]
use std::sync::Arc;
#[cfg(feature = "ocomp-integration")]
use std::thread::sleep;
#[cfg(feature = "ocomp-integration")]
use std::time::{Duration, Instant};

use crate::internal::config::Config;
use crate::internal::proc::ChildGuard;
use crate::ocomp_evidence::{
    CorrelatedTributeFixtureV1, CorrelationError, JobIntentCorrelationV1,
    PublicTributeCorrelationV1, TributeCorrelationBuilder, ValidatorSourceCorrelationV1,
};

#[cfg(feature = "ocomp-integration")]
const OCOMP_MAX_WORKERS_PER_DOMAIN: usize = 4;
#[cfg(any(feature = "ocomp-integration", test))]
const OCOMP_MAX_PROCESS_RECORDS: usize = 64;
const OCOMP_MAX_FAULT_RECORDS: usize = 32;
pub const METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX: &str =
    "0x06de88157b2c94c36b929a65c9db8d0f6a7ca10fad6d40be14098019f5749187";
#[cfg(feature = "ocomp-integration")]
pub const OCOMP_MEASUREMENT_ACTIVATION_HEIGHT: u64 =
    outbe_node::ocomp::fork::GENESIS_ACTIVE_OCOMP_HEIGHT;
#[cfg(feature = "ocomp-integration")]
pub const OCOMP_FINAL_ACTIVATION_HEIGHT: u64 = OCOMP_POC_FINAL_ACTIVATION_HEIGHT;
/// Provisional block envelope used by the disposable OCM-25 measurement chain.
#[cfg(feature = "ocomp-integration")]
const OCOMP_MEASUREMENT_BLOCK_GAS_LIMIT: u64 = 40_000_000;
#[cfg(feature = "ocomp-integration")]
const OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS: u64 = 120;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS: u64 = 360;
#[cfg(feature = "ocomp-integration")]
const OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS: u64 = 180;
#[cfg(feature = "ocomp-integration")]
const OCOMP_DYNAMIC_SECOND_OFFERING_AFTER_GENESIS_SECS: u64 = 700;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_TEST_EPOCH_LENGTH_BLOCKS: u64 = 300;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS: u64 = 10;

/// Fixed process roles represented in one validator's OCOMP domain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OcompProcessRole {
    Supervisor,
    /// Compute-only FullNode role: executes canonical Lysis and commits the
    /// local result, but never opens a voting runtime.
    Follower,
    SnapshotExporter,
    Worker,
}

/// Typed fault operations available to OCOMP scenarios.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OcompProcessFault {
    StopSupervisor {
        validator_index: u8,
    },
    StopSnapshotExporter {
        validator_index: u8,
    },
    StopWorker {
        validator_index: u8,
        worker_ordinal: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompFaultRecordV1 {
    pub fault: OcompProcessFault,
    pub applied_at_millis: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompLaunchIdentityEvidenceV1 {
    pub chain_id: u64,
    pub genesis_hash: String,
    pub protocol_bundle_hash: String,
    pub fork_install_hash: String,
    pub classification: String,
    pub activation_height: u64,
    pub metadosis_storage_layout_hash: String,
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

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompMismatchedForkManifestV1 {
    pub path: PathBuf,
    pub canonical_install_hash: B256,
    pub mismatched_install_hash: B256,
    pub canonical_activation_height: u64,
    pub mismatched_activation_height: u64,
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

/// Exact chain/bundle identity shared by one measurement network.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcompLaunchIdentityV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub protocol_bundle_hash: B256,
    pub fork_install_hash: B256,
    pub classification: OcompForkInstallClassification,
    pub activation_height: u64,
    pub metadosis_storage_layout_hash: B256,
}

/// Live process and registration counts for the baseline validator OCOMP runtime.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OcompRuntimeCountsV1 {
    pub supervisors: usize,
    pub snapshot_exporters: usize,
    pub workers: usize,
    pub registered_workers: usize,
    pub connected_workers: usize,
}

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisorWorkerStatusV1 {
    registry_generation: u64,
    registered_workers: usize,
    connected_workers: usize,
    busy_workers: usize,
    accepted_leases: usize,
    queued_units: usize,
    max_workers: usize,
}

/// Exact measurement manifest generated before any node process starts.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompMeasurementForkV1 {
    pub install: OcompForkInstallV1,
    pub install_hash: B256,
}

/// Exact two-job schedule plus immutable fork used by the dynamic-membership E2E.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompDynamicMembershipForkV1 {
    pub fork: OcompMeasurementForkV1,
    pub first_worldwide_day: WorldwideDay,
    pub second_worldwide_day: WorldwideDay,
    pub first_processing_time: u64,
    pub second_processing_time: u64,
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

#[derive(Debug)]
struct OwnedProcess {
    guard: ChildGuard,
    record_index: usize,
}

#[derive(Debug)]
struct OcompDomain {
    root: PathBuf,
    supervisor: Option<OwnedProcess>,
    snapshot_exporter: Option<OwnedProcess>,
    workers: BTreeMap<u32, OwnedProcess>,
}

impl OcompDomain {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            supervisor: None,
            snapshot_exporter: None,
            workers: BTreeMap::new(),
        }
    }
}

/// Sole scenario owner of one isolated compute domain per configured validator.
#[derive(Debug)]
pub struct OcompTopology {
    cfg: Config,
    domains: Vec<OcompDomain>,
    /// One synchronized, non-voting FullNode compute domain awaiting canonical
    /// validator admission. It is deliberately outside `domains`, whose order
    /// is the ACTIVE OCOMP membership asserted by the harness.
    keyless_full_node_domain: Option<(u8, OcompDomain)>,
    records: Vec<OcompProcessRecordV1>,
    faults: Vec<OcompFaultRecordV1>,
    launch_identity_evidence: Option<OcompLaunchIdentityEvidenceV1>,
    fork_restart_evidence: Option<OcompForkRestartEvidenceV1>,
    fork_mismatch_evidence: Option<OcompForkMismatchEvidenceV1>,
    #[cfg(feature = "ocomp-integration")]
    launch_identity: Option<OcompLaunchIdentityV1>,
    tribute_correlation: TributeCorrelationBuilder,
    correlated_tribute: Option<CorrelatedTributeFixtureV1>,
}

impl OcompTopology {
    pub(crate) fn new(cfg: Config) -> Self {
        let domains = (0..cfg.validators)
            .map(|index| OcompDomain::new(cfg.validator_dir(index).join("ocomp").join("domain-v1")))
            .collect();
        let tribute_correlation = TributeCorrelationBuilder::new(cfg.validators)
            .expect("harness validator count must fit the validator index format");
        Self {
            domains,
            keyless_full_node_domain: None,
            cfg,
            records: Vec::new(),
            faults: Vec::new(),
            launch_identity_evidence: None,
            fork_restart_evidence: None,
            fork_mismatch_evidence: None,
            #[cfg(feature = "ocomp-integration")]
            launch_identity: None,
            tribute_correlation,
            correlated_tribute: None,
        }
    }

    /// Extend the process topology after the canonical ValidatorSet has
    /// activated exactly the next ordered validator. This allocates local
    /// harness resources only; chain membership remains authoritative.
    pub fn add_active_validator_domain(&mut self, validator_index: u8) -> Result<()> {
        let expected = self.domains.len();
        eyre::ensure!(
            usize::from(validator_index) == expected,
            "active validator domain must append at index {expected}"
        );
        let domain = match self.keyless_full_node_domain.take() {
            Some((index, domain)) => {
                eyre::ensure!(
                    index == validator_index,
                    "staged FullNode domain belongs to validator-{index}, not validator-{validator_index}"
                );
                eyre::ensure!(
                    domain.supervisor.is_none()
                        && domain.snapshot_exporter.is_none()
                        && domain.workers.is_empty(),
                    "keyless FullNode roles must stop before validator activation"
                );
                domain
            }
            None => OcompDomain::new(
                self.cfg
                    .validator_dir(expected)
                    .join("ocomp")
                    .join("domain-v1"),
            ),
        };
        self.domains.push(domain);
        Ok(())
    }

    /// Stage the compute-only profile required by an OCOMP-enabled FullNode.
    /// The node itself no longer owns an OCOMP control transport, so this
    /// returns no node CLI arguments. The durable domain remains outside the
    /// ACTIVE voting topology and intentionally contains no voting keys.
    #[cfg(feature = "ocomp-integration")]
    pub fn stage_keyless_full_node_domain(&mut self, validator_index: u8) -> Result<Vec<String>> {
        let index = usize::from(validator_index);
        eyre::ensure!(
            index == self.domains.len(),
            "keyless FullNode must use the next ordered validator slot"
        );
        eyre::ensure!(
            self.keyless_full_node_domain.is_none(),
            "a keyless FullNode domain is already staged"
        );
        self.launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let source_bundle = self.domain_root(0)?.join("protocol-bundle-v1.ocb1");
        let root = self
            .cfg
            .validator_dir(index)
            .join("ocomp")
            .join("domain-v1");
        fs::create_dir_all(&root)?;
        publish_exact_file(
            &root.join("protocol-bundle-v1.ocb1"),
            &fs::read(source_bundle)?,
            0o640,
        )?;
        eyre::ensure!(
            !root.join("ocomp-key-v1.hex").exists() && !root.join("ocomp-evm-key.hex").exists(),
            "keyless FullNode domain contains validator voting material"
        );
        self.keyless_full_node_domain = Some((validator_index, OcompDomain::new(root)));

        Ok(Vec::new())
    }

    /// Stage the next validator's local runtime material before it starts in
    /// validator mode. This does not add it to the voting topology; membership
    /// changes only after the canonical ValidatorSet activation boundary.
    #[cfg(feature = "ocomp-integration")]
    pub fn stage_joiner_domain_material(&self, validator_index: u8) -> Result<()> {
        let index = usize::from(validator_index);
        eyre::ensure!(
            index == self.domains.len(),
            "staged joiner must be the next ordered validator index"
        );
        let source_bundle = self.domain_root(0)?.join("protocol-bundle-v1.ocb1");
        let source_key = self.cfg.validator_dir(index).join("ocomp-key-v1.hex");
        let bundle = fs::read(&source_bundle)?;
        let signing_key = fs::read(&source_key)?;
        let root = self
            .cfg
            .validator_dir(index)
            .join("ocomp")
            .join("domain-v1");
        fs::create_dir_all(&root)?;
        publish_exact_file(&root.join("protocol-bundle-v1.ocb1"), &bundle, 0o640)?;
        publish_exact_file(&root.join("ocomp-key-v1.hex"), &signing_key, 0o600)?;
        let evm_key = format!("{}\n", ocomp_evm_private_key(validator_index));
        publish_exact_file(&root.join("ocomp-evm-key.hex"), evm_key.as_bytes(), 0o600)?;
        Ok(())
    }

    /// Scenario-owned root for one validator domain.
    pub fn domain_root(&self, validator_index: u8) -> Result<&Path> {
        Ok(&self.domain(validator_index)?.root)
    }

    /// Canonical chain manifest selected for this scenario before any
    /// mismatched-install fault is injected.
    #[cfg(feature = "ocomp-integration")]
    #[must_use]
    pub fn canonical_chain_manifest_path(&self) -> PathBuf {
        self.cfg.dir.join("genesis.json")
    }

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
                &root.join("worker-inbox-v1").join("artifacts"),
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
                .join(format!("{job_component}.vote.v1"));
            let vote_metadata = fs::symlink_metadata(&vote_path)?;
            eyre::ensure!(
                vote_metadata.file_type().is_file()
                    && !vote_metadata.file_type().is_symlink()
                    && vote_metadata.len() > 0,
                "validator-{validator_index} has no durable vote submission for job {job_id:#x}"
            );

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

    /// Ask the node for only the inner OCOMP attestation, then build and sign
    /// the exact public transaction with this domain's dedicated OCOMP EVM key.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_held_vote_transaction(
        &self,
        validator_index: u8,
        mut vote: ResultVoteV1,
        nonce: u64,
        max_fee_per_gas: u128,
        gas_limit: u64,
    ) -> Result<PreparedVoteTransactionV1> {
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is unavailable"))?;
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let bundle = ProtocolBundleV1::decode_canonical(
            &fs::read(
                self.domain_root(validator_index)?
                    .join("protocol-bundle-v1.ocb1"),
            )?,
            &limits,
        )?;
        let key = fs::read_to_string(self.domain_root(validator_index)?.join("ocomp-key-v1.hex"))?;
        let signing_key = SigningKey::from_slice(&hex::decode(key.trim())?)?;
        let public_key = signing_key.verifying_key().to_encoded_point(true);
        vote.ocomp_key_hash = keccak256(public_key.as_bytes());
        vote.signature_rs = [0; 64];
        let subject = ResultVoteSigningSubjectV1 {
            chain_id: identity.chain_id,
            genesis_hash: identity.genesis_hash,
            fork_id: bundle.fork_id,
            protocol_bundle_hash: vote.protocol_bundle_hash,
            job_id: vote.job_id,
            attempt: vote.attempt,
            result_validator_set_epoch: vote.result_validator_set_epoch,
            result_committee_set_hash: vote.result_committee_set_hash,
            result_ocomp_binding_hash: vote.result_ocomp_binding_hash,
            ocomp_key_hash: vote.ocomp_key_hash,
            key_epoch: vote.key_epoch,
            purpose: SignOncePurpose::ResultSignature as u8,
            result_digest: vote.result_digest(&limits)?,
        };
        let signature: Signature =
            signing_key.sign_prehash(subject.signing_digest()?.as_slice())?;
        vote.signature_rs = signature
            .normalize_s()
            .unwrap_or(signature)
            .to_bytes()
            .into();
        let canonical_vote = vote.encode_canonical(&limits)?;
        let calldata =
            outbe_ocomp_protocol::abi::encode_submit_lysis_result_calldata(&vote, &limits)?;
        let signer = OutbeEvmSigner::from_file(
            self.domain_root(validator_index)?.join("ocomp-evm-key.hex"),
        )?;
        let signed = signer.sign_eip1559(TxEip1559 {
            chain_id: identity.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input: Bytes::from(calldata),
            access_list: Default::default(),
        })?;
        let transaction_hash = *signed.hash();
        let mut raw_transaction = Vec::with_capacity(signed.encode_2718_len());
        signed.encode_2718(&mut raw_transaction);
        Ok(PreparedVoteTransactionV1 {
            canonical_vote: BoundedBytes(canonical_vote),
            raw_transaction: BoundedBytes(raw_transaction),
            transaction_hash,
        })
    }

    /// Generate and publish the complete immutable measurement fork before any
    /// node process starts.
    ///
    /// The resulting base genesis hash binds the request profile and protocol
    /// bundle without a synthetic generic Update. Adding the canonical install
    /// under `genesis.config` does not alter that header hash.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_measurement_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        self.prepare_measurement_fork_install_inner(None, &[], false, None)
    }

    /// Prepare the same immutable measurement fork plus a short, pre-start
    /// WorldwideDay schedule. This changes no JobIntent/result/state after
    /// launch: it only lets a public Tribute offered by the scenario naturally
    /// reach the production Metadosis request transition in bounded test time.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_public_measurement_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        self.prepare_measurement_fork_install_inner(
            Some(OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS),
            &[],
            false,
            None,
        )
    }

    /// Prepare a public measurement chain whose first no-quorum expiry
    /// deterministically exhausts the attempt budget. This changes only the
    /// immutable Measurement capacity profile; live Metadosis/OCOMP state is
    /// still created and advanced exclusively by production block execution.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_failure_recovery_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        self.prepare_measurement_fork_install_inner(
            Some(OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS),
            &[],
            false,
            Some(1),
        )
    }

    /// Prepare two independently scheduled public jobs around one real DKG
    /// membership boundary. The shortened epoch is still above the normative
    /// snapshot-retention lower bound; the exact production 1,800-block
    /// compute-and-vote deadline is left unchanged.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_dynamic_membership_fork_install(&self) -> Result<OcompDynamicMembershipForkV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
        let chain_id = genesis_chain_id(&genesis)?;
        let schedule = schedule_dynamic_membership_days(&mut genesis, chain_id)?;
        let config = genesis
            .get("config")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis config is not an object"))?;
        eyre::ensure!(
            config
                .get(outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY)
                .and_then(serde_json::Value::as_u64)
                == Some(OCOMP_TEST_EPOCH_LENGTH_BLOCKS)
                && config
                    .get("dkgPrepareWindowBlocks")
                    .and_then(serde_json::Value::as_u64)
                    == Some(OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS),
            "dynamic OCOMP epoch and DKG window must be configured before ValidatorSet genesis is seeded"
        );
        replace_json_atomically(&genesis_path, &genesis)?;

        let fork = self.prepare_measurement_fork_install_inner(None, &[], false, None)?;
        Ok(OcompDynamicMembershipForkV1 {
            fork,
            first_worldwide_day: schedule.0,
            second_worldwide_day: schedule.1,
            first_processing_time: schedule.2,
            second_processing_time: schedule.3,
        })
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
            None,
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
            self.prepare_measurement_fork_install_inner(None, &private_keys, true, None)?;
        Ok((prepared, private_keys))
    }

    /// Load the checked-in canonical `Final` install and publish only its
    /// node-local bundle and test-validator signing material.
    ///
    /// Unlike the measurement helpers, this path never mutates genesis,
    /// committee membership, capacity, schedule or fork bindings.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_final_fork_install(&self) -> Result<OcompMeasurementForkV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let spec = parse_outbe_chain_spec(&genesis_path)?;
        let chain_id = spec.chain().id();
        let genesis_hash = spec.genesis_hash();
        let loaded = outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&spec)?;
        let install = loaded.as_ref().clone();
        if install.classification != OcompForkInstallClassification::Final {
            eyre::bail!(
                "canonical OCOMP fixture contains a non-Final fork install: {:?}",
                install.classification
            );
        }
        if install.activation_height != OCOMP_FINAL_ACTIVATION_HEIGHT {
            eyre::bail!(
                "canonical OCOMP fixture activates at {}, expected {}",
                install.activation_height,
                OCOMP_FINAL_ACTIVATION_HEIGHT
            );
        }
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        install.validate_for_chain(chain_id, genesis_hash, &limits)?;
        let install_hash = install.install_hash(&limits)?;
        self.publish_validator_domain_material(&install)?;
        Ok(OcompMeasurementForkV1 {
            install,
            install_hash,
        })
    }

    /// Recover the deterministic public capacity-owner keys funded by the
    /// canonical Final fixture. Keys remain harness-only test material.
    #[cfg(feature = "ocomp-integration")]
    pub fn final_capacity_tribute_private_keys(&self, count: usize) -> Result<Vec<String>> {
        capacity_tribute_private_keys(count)
    }

    #[cfg(feature = "ocomp-integration")]
    fn publish_validator_domain_material(&self, install: &OcompForkInstallV1) -> Result<()> {
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let canonical_bundle = install.protocol_bundle.encode_canonical(&limits)?;
        for (validator_index, domain) in self.domains.iter().enumerate() {
            fs::create_dir_all(&domain.root)?;
            publish_exact_file(
                &domain.root.join("protocol-bundle-v1.ocb1"),
                &canonical_bundle,
                0o640,
            )?;
            let key = measurement_signing_key(u8::try_from(validator_index)?);
            let key_bytes = format!("{}\n", hex::encode(key.to_bytes()));
            publish_exact_file(
                &domain.root.join("ocomp-key-v1.hex"),
                key_bytes.as_bytes(),
                0o600,
            )?;
            let evm_key = ocomp_evm_private_key(u8::try_from(validator_index)?);
            publish_exact_file(
                &domain.root.join("ocomp-evm-key.hex"),
                format!("{evm_key}\n").as_bytes(),
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
                signing_key.verifying_key().to_encoded_point(true).as_bytes()
                    == registration.core.ocomp_public_key_sec1.as_slice(),
                "validator-{index} OCOMP result-signing key does not match its genesis registration"
            );

            fs::create_dir_all(&domain.root)?;
            publish_exact_file(
                &domain.root.join("protocol-bundle-v1.ocb1"),
                &bootstrapped_bundle,
                0o640,
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
        }
        .launch_identity())
    }

    /// Launch the complete baseline compute runtime for every genesis ACTIVE
    /// validator: one Supervisor, one SnapshotExporter, and Worker ordinal 0.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_baseline_runtime(&mut self, identity: OcompLaunchIdentityV1) -> Result<()> {
        self.install_ocomp_delegate_bindings()?;
        self.start_validator_roles(identity)?;
        for validator_index in self.validator_indices()? {
            self.activate_worker(validator_index, 0, identity)?;
        }
        Ok(())
    }

    /// Prove both child-process liveness and mutual Worker/Supervisor
    /// registration for the complete baseline runtime.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_baseline_runtime_ready(
        &mut self,
        expected_workers_per_supervisor: usize,
    ) -> Result<OcompRuntimeCountsV1> {
        self.ensure_baseline_processes_alive(expected_workers_per_supervisor)?;
        self.observe_baseline_runtime(expected_workers_per_supervisor)
    }

    /// Fail immediately when a required owned OCOMP role exits. Registration
    /// convergence is retryable during startup; a dead child is not.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_baseline_processes_alive(
        &mut self,
        expected_workers_per_supervisor: usize,
    ) -> Result<()> {
        self.ensure_validator_roles_alive()?;
        for validator_index in self.validator_indices()? {
            for worker_ordinal in 0..u32::try_from(expected_workers_per_supervisor)? {
                self.ensure_worker_alive(validator_index, worker_ordinal)?;
            }
        }
        Ok(())
    }

    /// Probe the public Supervisor status surfaces without relying on retained
    /// child guards. This is used by the separate `localnet status` process.
    #[cfg(feature = "ocomp-integration")]
    pub fn observe_baseline_runtime(
        &self,
        expected_workers_per_supervisor: usize,
    ) -> Result<OcompRuntimeCountsV1> {
        let mut registered_workers = 0usize;
        let mut connected_workers = 0usize;
        for validator_index in self.validator_indices()? {
            let index = usize::from(validator_index);
            let address = SocketAddr::from(([127, 0, 0, 1], self.cfg.ocomp_supervisor_port(index)));
            let status = fetch_supervisor_status(address)?;
            ensure_supervisor_status_ready(
                validator_index,
                &status,
                expected_workers_per_supervisor,
            )?;
            registered_workers = registered_workers
                .checked_add(status.registered_workers)
                .ok_or_else(|| eyre::eyre!("OCOMP registered-worker count overflow"))?;
            connected_workers = connected_workers
                .checked_add(status.connected_workers)
                .ok_or_else(|| eyre::eyre!("OCOMP connected-worker count overflow"))?;
        }
        let supervisors = self.domains.len();
        let workers = supervisors
            .checked_mul(expected_workers_per_supervisor)
            .ok_or_else(|| eyre::eyre!("OCOMP worker count overflow"))?;
        Ok(OcompRuntimeCountsV1 {
            supervisors,
            snapshot_exporters: supervisors,
            workers,
            registered_workers,
            connected_workers,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn install_ocomp_delegate_bindings(&self) -> Result<()> {
        const OCOMP_ROLE: u8 = 2;
        let validator_indices = self.validator_indices()?;
        for validator_index in validator_indices.iter().copied() {
            let index = usize::from(validator_index);
            let validator_key =
                crate::internal::proc::read_evm_key(&self.cfg.validator_dir(index))?;
            let delegate = self.ocomp_delegate_address(validator_index)?;
            let url = self.cfg.rpc_url(index);
            let tx_hash = crate::internal::eth::send_call(
                &url,
                VALIDATOR_SET_ADDRESS,
                &validator_key,
                &crate::internal::eth::IValidatorSet::setDelegateCall {
                    role: OCOMP_ROLE,
                    delegate,
                },
                None,
            )?;
            eyre::ensure!(
                crate::internal::eth::receipt_success(&url, &tx_hash) == Some(true),
                "validator-{validator_index} OCOMP delegation transaction failed"
            );
            if crate::internal::eth::balance(&url, delegate) == Some(U256::ZERO) {
                let funding_tx = crate::internal::eth::send_value(
                    &url,
                    delegate,
                    &validator_key,
                    crate::internal::eth::coen(1),
                )?;
                eyre::ensure!(
                    crate::internal::eth::receipt_success(&url, &funding_tx) == Some(true),
                    "validator-{validator_index} OCOMP delegate funding transaction failed"
                );
            }
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let complete = validator_indices.iter().copied().all(|validator_index| {
                let delegate = self
                    .ocomp_delegate_address(validator_index)
                    .unwrap_or(Address::ZERO);
                let expected_validator = crate::internal::proc::read_evm_key(
                    &self.cfg.validator_dir(usize::from(validator_index)),
                )
                .ok()
                .and_then(|key| crate::internal::eth::address_of(&key))
                .unwrap_or(Address::ZERO);
                (0..self.domains.len()).all(|rpc_index| {
                    let rpc_url = self.cfg.rpc_url(rpc_index);
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::resolveValidatorCall {
                            role: OCOMP_ROLE,
                            signer: delegate,
                        },
                    ) == Some(expected_validator)
                        && crate::internal::eth::balance(&rpc_url, delegate)
                            .is_some_and(|balance| !balance.is_zero())
                })
            });
            if complete {
                return Ok(());
            }
            eyre::ensure!(
                Instant::now() < deadline,
                "OCOMP delegate bindings did not converge on every validator"
            );
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_delegate_address(&self, validator_index: u8) -> Result<Address> {
        self.domain(validator_index)?;
        crate::internal::eth::address_of(&ocomp_evm_private_key(validator_index))
            .ok_or_else(|| eyre::eyre!("invalid deterministic OCOMP EVM key"))
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn verify_ocomp_delegate_bindings(&self) -> Result<()> {
        const ORACLE_ROLE: u8 = 1;
        const OCOMP_ROLE: u8 = 2;

        let mut observed_delegates = Vec::with_capacity(self.domains.len());
        for validator_index in self.validator_indices()? {
            let index = usize::from(validator_index);
            let validator_key =
                crate::internal::proc::read_evm_key(&self.cfg.validator_dir(index))?;
            let validator = crate::internal::eth::address_of(&validator_key)
                .ok_or_else(|| eyre::eyre!("invalid validator-{validator_index} EVM key"))?;
            let delegate = self.ocomp_delegate_address(validator_index)?;
            eyre::ensure!(
                delegate != validator,
                "validator-{validator_index} reused its validator EVM key for OCOMP"
            );
            eyre::ensure!(
                !observed_delegates.contains(&delegate),
                "two validator domains share the same OCOMP delegate"
            );
            observed_delegates.push(delegate);

            for rpc_index in 0..self.domains.len() {
                let rpc_url = self.cfg.rpc_url(rpc_index);
                eyre::ensure!(
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::getDelegateCall {
                            validator,
                            role: OCOMP_ROLE,
                        },
                    ) == Some(delegate),
                    "validator-{validator_index} OCOMP delegate is inconsistent on RPC {rpc_index}"
                );
                eyre::ensure!(
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::resolveValidatorCall {
                            role: OCOMP_ROLE,
                            signer: delegate,
                        },
                    ) == Some(validator),
                    "validator-{validator_index} OCOMP delegate does not resolve on RPC {rpc_index}"
                );
                eyre::ensure!(
                    crate::internal::eth::read_call(
                        &rpc_url,
                        VALIDATOR_SET_ADDRESS,
                        &crate::internal::eth::IValidatorSet::resolveValidatorCall {
                            role: ORACLE_ROLE,
                            signer: delegate,
                        },
                    ) == Some(Address::ZERO),
                    "validator-{validator_index} OCOMP delegate also has the ORACLE role on RPC {rpc_index}"
                );
            }
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn prepare_measurement_fork_install_inner(
        &self,
        public_offering_after_genesis_secs: Option<u64>,
        capacity_tribute_private_keys: &[String],
        clear_seeded_metadosis: bool,
        max_terminal_job_records: Option<u16>,
    ) -> Result<OcompMeasurementForkV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
        let chain_id = genesis_chain_id(&genesis)?;
        let capacity_accounts_changed =
            fund_capacity_tribute_accounts(&mut genesis, capacity_tribute_private_keys)?;
        let public_day_changed = if let Some(offering_after_genesis_secs) =
            public_offering_after_genesis_secs
        {
            schedule_public_measurement_day(&mut genesis, chain_id, offering_after_genesis_secs)?
        } else {
            false
        };
        let seeded_metadosis_changed = if clear_seeded_metadosis {
            clear_seeded_metadosis_days(&mut genesis, chain_id)?
        } else {
            false
        };
        let gas_envelope_changed = apply_measurement_gas_envelope(&mut genesis)?;
        if capacity_accounts_changed
            || public_day_changed
            || seeded_metadosis_changed
            || gas_envelope_changed
        {
            replace_json_atomically(&genesis_path, &genesis)?;
        }

        let base_spec = parse_outbe_chain_spec(&genesis_path)?;
        let base_genesis_hash = base_spec.genesis_hash();
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let install = measurement_fork_install(
            chain_id,
            base_genesis_hash,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT,
            &self.cfg.dir.join("validators.json"),
            &limits,
            max_terminal_job_records,
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

    /// Start the production Supervisor and SnapshotExporter in every validator
    /// domain after the corresponding node control sockets are ready.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_validator_roles(&mut self, identity: OcompLaunchIdentityV1) -> Result<()> {
        if !self.cfg.bin_ocomp.is_file() {
            eyre::bail!(
                "outbe-ocomp binary does not exist: {}",
                self.cfg.bin_ocomp.display()
            );
        }
        if self.launch_identity.is_some() {
            eyre::bail!("OCOMP validator roles were already started for this scenario");
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

    /// Start node-facing OCOMP roles for a validator only after the certified
    /// boundary has made it ACTIVE and its domain has been appended.
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
    /// Its `follower` process shares the validator Supervisor pipeline but has
    /// no signing key, EVM relay or vote submission path.
    #[cfg(feature = "ocomp-integration")]
    pub fn start_keyless_full_node_roles(&mut self, validator_index: u8) -> Result<()> {
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let domain = self.keyless_full_node_domain(validator_index)?;
        eyre::ensure!(
            domain.supervisor.is_none() && domain.snapshot_exporter.is_none(),
            "keyless FullNode OCOMP roles are already started"
        );
        let follower = self.spawn_keyless_full_node_role(
            validator_index,
            OcompProcessRole::Follower,
            identity,
        )?;
        self.attach_keyless_full_node_owned(validator_index, OcompProcessRole::Follower, follower)?;
        let exporter = self.spawn_keyless_full_node_role(
            validator_index,
            OcompProcessRole::SnapshotExporter,
            identity,
        )?;
        self.attach_keyless_full_node_owned(
            validator_index,
            OcompProcessRole::SnapshotExporter,
            exporter,
        )?;
        sleep(Duration::from_secs(2));
        self.ensure_keyless_full_node_roles_alive(validator_index)
    }

    /// Stop only the compute clients; the synchronized FullNode process and
    /// durable domain remain intact for validator-mode promotion.
    #[cfg(feature = "ocomp-integration")]
    pub fn stop_keyless_full_node_roles(&mut self, validator_index: u8) -> Result<()> {
        let (follower, exporter) = {
            let domain = self.keyless_full_node_domain_mut(validator_index)?;
            (domain.supervisor.take(), domain.snapshot_exporter.take())
        };
        let follower = follower.ok_or_else(|| eyre::eyre!("FullNode follower is not running"))?;
        let exporter =
            exporter.ok_or_else(|| eyre::eyre!("FullNode snapshot exporter is not running"))?;
        self.stop_owned(follower);
        self.stop_owned(exporter);
        Ok(())
    }

    /// Network identity pinned when the genesis validator roles were started.
    #[cfg(feature = "ocomp-integration")]
    #[must_use]
    pub fn launch_identity(&self) -> Option<OcompLaunchIdentityV1> {
        self.launch_identity
    }

    #[cfg(feature = "ocomp-integration")]
    fn start_validator_roles_for_domain(
        &mut self,
        validator_index: u8,
        identity: OcompLaunchIdentityV1,
    ) -> Result<()> {
        let domain = self.domain(validator_index)?;
        eyre::ensure!(
            domain.supervisor.is_none() && domain.snapshot_exporter.is_none(),
            "validator-{validator_index} OCOMP roles are already started"
        );
        let supervisor =
            self.spawn_validator_role(validator_index, OcompProcessRole::Supervisor, identity)?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Supervisor,
            None,
            supervisor,
        )?;

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
        let log_path = domain_root.join(format!("worker-{worker_ordinal}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;
        let supervisor_address = std::net::SocketAddr::from((
            [127, 0, 0, 1],
            self.cfg.ocomp_supervisor_port(usize::from(validator_index)),
        ));
        let worker_boot_nonce = worker_boot_nonce(validator_index, worker_ordinal);
        let expected_observability_port = self
            .cfg
            .ocomp_worker_port(usize::from(validator_index), worker_ordinal);
        debug_assert_eq!(
            supervisor_address.port() + 2 + u16::try_from(worker_ordinal).unwrap(),
            expected_observability_port
        );

        let mut command = Command::new(&self.cfg.bin_ocomp);
        command
            .arg("worker")
            .arg("--development-root")
            .arg(&domain_root)
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
        let guard = ChildGuard::spawn(
            format!("validator-{validator_index} OCOMP worker-{worker_ordinal}"),
            command,
        )?;

        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Worker,
            Some(worker_ordinal),
            guard,
        )?;
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn spawn_validator_role(
        &mut self,
        validator_index: u8,
        role: OcompProcessRole,
        identity: OcompLaunchIdentityV1,
    ) -> Result<ChildGuard> {
        let domain_root = self.domain(validator_index)?.root.clone();
        let role_name = match role {
            OcompProcessRole::Supervisor => "supervisor",
            OcompProcessRole::SnapshotExporter => "snapshot-exporter",
            _ => {
                eyre::bail!("validator service launcher accepts only fixed node-facing roles");
            }
        };
        let log_path = domain_root.join(format!("{role_name}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;

        let mut command = Command::new(&self.cfg.bin_ocomp);
        command
            .arg(role_name)
            .arg("--development-root")
            .arg(&domain_root)
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
                "OCOMP_PROTOCOL_BUNDLE_HASH",
                format!("{:#x}", identity.protocol_bundle_hash),
            )
            .env("OCOMP_REGISTRY_GENERATION", "1");
        match role {
            OcompProcessRole::Supervisor => {
                let validator_index = usize::from(validator_index);
                command
                    .arg("--supervisor-address")
                    .arg(format!(
                        "127.0.0.1:{}",
                        self.cfg.ocomp_supervisor_port(validator_index)
                    ))
                    .env("OUTBE_OCOMP_RPC_URL", self.cfg.rpc_url(validator_index))
                    .env("OCOMP_VALIDATOR_INDEX", validator_index.to_string());
            }
            OcompProcessRole::SnapshotExporter => {
                let validator_index = usize::from(validator_index);
                command
                    .env("OUTBE_OCOMP_RPC_URL", self.cfg.rpc_url(validator_index))
                    .env(
                        "OUTBE_OCOMP_PROJECTION_MONGODB_URI",
                        &self.cfg.projection_mongodb_uri,
                    )
                    .env(
                        "OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE",
                        format!(
                            "{}_ocomp",
                            self.cfg.validator_projection_database(validator_index)
                        ),
                    );
            }
            _ => unreachable!("fixed node-facing role validated above"),
        }
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
    fn spawn_keyless_full_node_role(
        &self,
        validator_index: u8,
        role: OcompProcessRole,
        identity: OcompLaunchIdentityV1,
    ) -> Result<ChildGuard> {
        let domain_root = self.keyless_full_node_domain(validator_index)?.root.clone();
        let role_name = match role {
            OcompProcessRole::Follower => "follower",
            OcompProcessRole::SnapshotExporter => "snapshot-exporter",
            _ => {
                eyre::bail!("FullNode launcher accepts only follower/exporter roles");
            }
        };
        let log_path = domain_root.join(format!("{role_name}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr = log.try_clone()?;
        let index = usize::from(validator_index);
        let mut command = Command::new(&self.cfg.bin_ocomp);
        command
            .arg(role_name)
            .arg("--development-root")
            .arg(&domain_root)
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
                "OCOMP_PROTOCOL_BUNDLE_HASH",
                format!("{:#x}", identity.protocol_bundle_hash),
            );
        match role {
            OcompProcessRole::Follower => {
                command
                    .arg("--supervisor-address")
                    .arg(format!(
                        "127.0.0.1:{}",
                        self.cfg.ocomp_supervisor_port(index)
                    ))
                    .env("OUTBE_OCOMP_RPC_URL", self.cfg.rpc_url(index));
            }
            OcompProcessRole::SnapshotExporter => {
                command
                    .env("OUTBE_OCOMP_RPC_URL", self.cfg.rpc_url(index))
                    .env(
                        "OUTBE_OCOMP_PROJECTION_MONGODB_URI",
                        &self.cfg.projection_mongodb_uri,
                    )
                    .env(
                        "OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE",
                        format!("{}_ocomp", self.cfg.validator_projection_database(index)),
                    );
            }
            _ => unreachable!("role validated above"),
        }
        command.stdout(Stdio::from(log)).stderr(Stdio::from(stderr));
        ChildGuard::spawn(
            format!("full-node-{validator_index} OCOMP {role_name}"),
            command,
        )
    }

    #[cfg(feature = "ocomp-integration")]
    fn attach_keyless_full_node_owned(
        &mut self,
        validator_index: u8,
        role: OcompProcessRole,
        guard: ChildGuard,
    ) -> Result<()> {
        if self.records.len() >= OCOMP_MAX_PROCESS_RECORDS {
            eyre::bail!("OCOMP scenario reached the bounded process-record limit");
        }
        let domain = self.keyless_full_node_domain(validator_index)?;
        match role {
            OcompProcessRole::Follower if domain.supervisor.is_none() => {}
            OcompProcessRole::SnapshotExporter if domain.snapshot_exporter.is_none() => {}
            _ => {
                eyre::bail!("invalid or duplicate keyless FullNode role attachment");
            }
        }
        let record_index = self.records.len();
        self.records.push(OcompProcessRecordV1 {
            validator_index: Some(validator_index),
            role,
            worker_ordinal: None,
            pid: guard.pid(),
            started_at_millis: unix_time_millis(),
            stopped_at_millis: None,
        });
        let process = OwnedProcess {
            guard,
            record_index,
        };
        let domain = self.keyless_full_node_domain_mut(validator_index)?;
        match role {
            OcompProcessRole::Follower => domain.supervisor = Some(process),
            OcompProcessRole::SnapshotExporter => domain.snapshot_exporter = Some(process),
            _ => unreachable!("role validated above"),
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn ensure_keyless_full_node_roles_alive(&mut self, validator_index: u8) -> Result<()> {
        for role in [
            OcompProcessRole::Follower,
            OcompProcessRole::SnapshotExporter,
        ] {
            let (record_index, exited) = {
                let domain = self.keyless_full_node_domain_mut(validator_index)?;
                let process = match role {
                    OcompProcessRole::Follower => domain.supervisor.as_mut(),
                    OcompProcessRole::SnapshotExporter => domain.snapshot_exporter.as_mut(),
                    _ => unreachable!(),
                }
                .ok_or_else(|| eyre::eyre!("FullNode OCOMP {role:?} is missing"))?;
                (process.record_index, process.guard.exited())
            };
            if exited {
                self.records[record_index].stopped_at_millis = Some(unix_time_millis());
                let role_name = match role {
                    OcompProcessRole::Follower => "follower",
                    OcompProcessRole::SnapshotExporter => "snapshot-exporter",
                    _ => unreachable!(),
                };
                eyre::bail!(
                    "FullNode OCOMP {role_name} exited during startup:\n{}",
                    tail_file(
                        &self
                            .keyless_full_node_domain(validator_index)?
                            .root
                            .join(format!("{role_name}.log")),
                        20,
                    )
                );
            }
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_validator_roles_alive(&mut self) -> Result<()> {
        for validator_index in self.validator_indices()? {
            for role in [
                OcompProcessRole::Supervisor,
                OcompProcessRole::SnapshotExporter,
            ] {
                let intentionally_stopped = self.faults.iter().any(|record| {
                    matches!(
                        (role, record.fault),
                        (
                            OcompProcessRole::Supervisor,
                            OcompProcessFault::StopSupervisor {
                                validator_index: stopped
                            }
                        ) if stopped == validator_index
                    ) || matches!(
                        (role, record.fault),
                        (
                            OcompProcessRole::SnapshotExporter,
                            OcompProcessFault::StopSnapshotExporter {
                                validator_index: stopped
                            }
                        ) if stopped == validator_index
                    )
                });
                let (record_index, exited) = {
                    let domain = self.domain_mut(validator_index)?;
                    let process = match role {
                        OcompProcessRole::Supervisor => domain.supervisor.as_mut(),
                        OcompProcessRole::SnapshotExporter => domain.snapshot_exporter.as_mut(),
                        _ => unreachable!("fixed iteration above"),
                    };
                    let Some(process) = process else {
                        if intentionally_stopped {
                            continue;
                        }
                        eyre::bail!(
                            "validator-{validator_index} OCOMP {role:?} is missing without a typed fault"
                        );
                    };
                    (process.record_index, process.guard.exited())
                };
                if exited {
                    self.records[record_index].stopped_at_millis = Some(unix_time_millis());
                    let role_name = match role {
                        OcompProcessRole::Supervisor => "supervisor",
                        OcompProcessRole::SnapshotExporter => "snapshot-exporter",
                        _ => unreachable!("fixed iteration above"),
                    };
                    let log_path = self
                        .domain(validator_index)?
                        .root
                        .join(format!("{role_name}.log"));
                    eyre::bail!(
                        "validator-{validator_index} OCOMP {role_name} exited during startup:\n{}",
                        tail_file(&log_path, 20)
                    );
                }
            }
        }
        Ok(())
    }

    /// Require one exact harness-owned worker to still be running. A retained
    /// process record is not sufficient evidence after committee restarts: the
    /// child guard itself must report that the authenticated worker is live.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_worker_alive(&mut self, validator_index: u8, worker_ordinal: u32) -> Result<()> {
        let (record_index, exited) = {
            let domain = self.domain_mut(validator_index)?;
            let process = domain.workers.get_mut(&worker_ordinal).ok_or_else(|| {
                eyre::eyre!("validator-{validator_index} worker-{worker_ordinal} is not registered")
            })?;
            (process.record_index, process.guard.exited())
        };
        if exited {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            let log_path = self
                .domain(validator_index)?
                .root
                .join(format!("worker-{worker_ordinal}.log"));
            eyre::bail!(
                "validator-{validator_index} worker-{worker_ordinal} exited unexpectedly:\n{}",
                tail_file(&log_path, 20)
            );
        }
        Ok(())
    }

    /// Restart only the fixed Supervisor role in one domain after a typed stop.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_supervisor(&mut self, validator_index: u8) -> Result<()> {
        if self.domain(validator_index)?.supervisor.is_some() {
            eyre::bail!("validator-{validator_index} supervisor is already running");
        }
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let supervisor =
            self.spawn_validator_role(validator_index, OcompProcessRole::Supervisor, identity)?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Supervisor,
            None,
            supervisor,
        )?;
        sleep(Duration::from_secs(2));
        let (record_index, exited) = {
            let process = self
                .domain_mut(validator_index)?
                .supervisor
                .as_mut()
                .expect("attached immediately above");
            (process.record_index, process.guard.exited())
        };
        if exited {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            eyre::bail!(
                "validator-{validator_index} OCOMP supervisor exited during typed restart:\n{}",
                tail_file(
                    &self.domain(validator_index)?.root.join("supervisor.log"),
                    20
                )
            );
        }
        Ok(())
    }

    /// Restart the fixed SnapshotExporter role in one domain after a typed stop.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_snapshot_exporter(&mut self, validator_index: u8) -> Result<()> {
        if self.domain(validator_index)?.snapshot_exporter.is_some() {
            eyre::bail!("validator-{validator_index} snapshot exporter is already running");
        }
        let identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
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
        sleep(Duration::from_secs(2));
        let (record_index, exited) = {
            let process = self
                .domain_mut(validator_index)?
                .snapshot_exporter
                .as_mut()
                .expect("attached immediately above");
            (process.record_index, process.guard.exited())
        };
        if exited {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            eyre::bail!(
                "validator-{validator_index} OCOMP snapshot exporter exited during typed restart:\n{}",
                tail_file(
                    &self
                        .domain(validator_index)?
                        .root
                        .join("snapshot-exporter.log"),
                    20
                )
            );
        }
        Ok(())
    }

    /// Restart both fixed node-facing roles while preserving the domain data.
    /// A role may already be absent because an earlier scenario fault stopped
    /// it; restarting the complete domain remains one well-defined operation.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_node_facing_processes(&mut self, validator_index: u8) -> Result<()> {
        if let Some(process) = self.domain_mut(validator_index)?.supervisor.take() {
            self.stop_owned(process);
        }
        if let Some(process) = self.domain_mut(validator_index)?.snapshot_exporter.take() {
            self.stop_owned(process);
        }
        self.restart_snapshot_exporter(validator_index)?;
        self.restart_supervisor(validator_index)
    }

    /// Replace a stopped Supervisor with a process that has a valid local
    /// protocol bundle but an incompatible endpoint identity. The process must
    /// remain outside the node-owned authenticated session while the node and
    /// the other validator domains continue normally.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_incompatible_supervisor(&mut self, validator_index: u8) -> Result<()> {
        if self.domain(validator_index)?.supervisor.is_some() {
            eyre::bail!("validator-{validator_index} supervisor is already running");
        }
        let mut identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        identity.genesis_hash = B256::repeat_byte(0x7f);
        let supervisor =
            self.spawn_validator_role(validator_index, OcompProcessRole::Supervisor, identity)?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Supervisor,
            None,
            supervisor,
        )?;
        sleep(Duration::from_secs(2));
        let (record_index, exited) = {
            let process = self
                .domain_mut(validator_index)?
                .supervisor
                .as_mut()
                .expect("attached immediately above");
            (process.record_index, process.guard.exited())
        };
        if exited {
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            eyre::bail!(
                "validator-{validator_index} incompatible OCOMP supervisor exited instead of retrying:\n{}",
                self.supervisor_log_tail(validator_index, 20)?
            );
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn supervisor_log_tail(&self, validator_index: u8, lines: usize) -> Result<String> {
        Ok(tail_file(
            &self.domain(validator_index)?.root.join("supervisor.log"),
            lines,
        ))
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

    /// Record one independently verified validator Mongo/CE source package.
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

    #[cfg(feature = "ocomp-integration")]
    fn require_launch_identity(&self, identity: OcompLaunchIdentityV1) -> Result<()> {
        match self.launch_identity {
            Some(established) if established == identity => Ok(()),
            Some(_) => {
                eyre::bail!("OCOMP worker launch identity differs from the scenario network");
            }
            None => {
                eyre::bail!("OCOMP validator roles must start before workers");
            }
        }
    }

    /// Execute one authorized process fault without accepting a PID/path/command.
    pub fn apply_process_fault(&mut self, fault: OcompProcessFault) -> Result<()> {
        if self.faults.len() >= OCOMP_MAX_FAULT_RECORDS {
            eyre::bail!("OCOMP scenario reached the bounded fault-record limit");
        }
        match fault {
            OcompProcessFault::StopSupervisor { validator_index } => {
                let process = self
                    .domain_mut(validator_index)?
                    .supervisor
                    .take()
                    .ok_or_else(|| eyre::eyre!("supervisor is not running"))?;
                self.stop_owned(process);
            }
            OcompProcessFault::StopSnapshotExporter { validator_index } => {
                let process = self
                    .domain_mut(validator_index)?
                    .snapshot_exporter
                    .take()
                    .ok_or_else(|| eyre::eyre!("snapshot exporter is not running"))?;
                self.stop_owned(process);
            }
            OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal,
            } => {
                let process = self
                    .domain_mut(validator_index)?
                    .workers
                    .remove(&worker_ordinal)
                    .ok_or_else(|| eyre::eyre!("worker is not running"))?;
                self.stop_owned(process);
            }
        }
        self.faults.push(OcompFaultRecordV1 {
            fault,
            applied_at_millis: unix_time_millis(),
        });
        Ok(())
    }

    fn domain(&self, validator_index: u8) -> Result<&OcompDomain> {
        self.domains
            .get(usize::from(validator_index))
            .ok_or_else(|| eyre::eyre!("validator index is outside the configured topology"))
    }

    fn domain_mut(&mut self, validator_index: u8) -> Result<&mut OcompDomain> {
        self.domains
            .get_mut(usize::from(validator_index))
            .ok_or_else(|| eyre::eyre!("validator index is outside the configured topology"))
    }

    #[cfg(feature = "ocomp-integration")]
    fn keyless_full_node_domain(&self, validator_index: u8) -> Result<&OcompDomain> {
        match self.keyless_full_node_domain.as_ref() {
            Some((index, domain)) if *index == validator_index => Ok(domain),
            _ => Err(eyre::eyre!(
                "validator-{validator_index} has no staged keyless FullNode domain"
            )),
        }
    }

    #[cfg(feature = "ocomp-integration")]
    fn keyless_full_node_domain_mut(&mut self, validator_index: u8) -> Result<&mut OcompDomain> {
        match self.keyless_full_node_domain.as_mut() {
            Some((index, domain)) if *index == validator_index => Ok(domain),
            _ => Err(eyre::eyre!(
                "validator-{validator_index} has no staged keyless FullNode domain"
            )),
        }
    }

    fn validator_indices(&self) -> Result<Vec<u8>> {
        (0..self.domains.len())
            .map(|index| {
                u8::try_from(index)
                    .map_err(|_| eyre::eyre!("validator index exceeds the harness wire format"))
            })
            .collect()
    }

    fn stop_owned(&mut self, mut process: OwnedProcess) {
        process.guard.stop();
        self.records[process.record_index].stopped_at_millis = Some(unix_time_millis());
    }

    #[cfg(any(feature = "ocomp-integration", test))]
    fn attach_owned(
        &mut self,
        validator_index: Option<u8>,
        role: OcompProcessRole,
        worker_ordinal: Option<u32>,
        guard: ChildGuard,
    ) -> Result<()> {
        if self.records.len() >= OCOMP_MAX_PROCESS_RECORDS {
            eyre::bail!("OCOMP scenario reached the bounded process-record limit");
        }
        match role {
            OcompProcessRole::Follower => {
                eyre::bail!("keyless FullNode roles use their dedicated attachment path");
            }
            OcompProcessRole::Supervisor => {
                let index = validator_index
                    .ok_or_else(|| eyre::eyre!("supervisor requires a validator index"))?;
                if worker_ordinal.is_some() || self.domain(index)?.supervisor.is_some() {
                    eyre::bail!("invalid or duplicate supervisor attachment");
                }
            }
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
            OcompProcessRole::Follower => {
                unreachable!("keyless FullNode roles use their dedicated attachment path")
            }
            OcompProcessRole::Supervisor => {
                self.domain_mut(validator_index.expect("validated above"))?
                    .supervisor = Some(process);
            }
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
fn fetch_supervisor_status(address: SocketAddr) -> Result<SupervisorWorkerStatusV1> {
    const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| eyre::eyre!("connect to OCOMP Supervisor {address}: {error}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(
        format!("GET /v1/status HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut response)
        .map_err(|error| eyre::eyre!("read OCOMP Supervisor {address} status: {error}"))?;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| offset + 4)
        .ok_or_else(|| eyre::eyre!("OCOMP Supervisor {address} returned malformed HTTP"))?;
    let status_line_end = response
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| eyre::eyre!("OCOMP Supervisor {address} returned no HTTP status"))?;
    let status_line = std::str::from_utf8(&response[..status_line_end])?.trim();
    eyre::ensure!(
        status_line.split_whitespace().nth(1) == Some("200"),
        "OCOMP Supervisor {address} status request failed: {status_line}"
    );
    serde_json::from_slice(&response[header_end..])
        .map_err(|error| eyre::eyre!("decode OCOMP Supervisor {address} status: {error}"))
}

#[cfg(feature = "ocomp-integration")]
fn ensure_supervisor_status_ready(
    validator_index: u8,
    status: &SupervisorWorkerStatusV1,
    expected_workers: usize,
) -> Result<()> {
    eyre::ensure!(
        status.registry_generation > 0,
        "validator-{validator_index} OCOMP Supervisor has no registry generation"
    );
    eyre::ensure!(
        status.max_workers >= expected_workers,
        "validator-{validator_index} OCOMP Supervisor capacity {} is below expected worker count {expected_workers}",
        status.max_workers
    );
    eyre::ensure!(
        status.registered_workers == expected_workers,
        "validator-{validator_index} OCOMP Supervisor reports {} registered workers, expected {expected_workers}",
        status.registered_workers
    );
    eyre::ensure!(
        status.connected_workers == expected_workers,
        "validator-{validator_index} OCOMP Supervisor reports {} connected workers, expected {expected_workers}",
        status.connected_workers
    );
    eyre::ensure!(
        status.busy_workers <= status.connected_workers,
        "validator-{validator_index} OCOMP Supervisor reports more busy than connected workers"
    );
    let _ = (status.accepted_leases, status.queued_units);
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn worker_boot_nonce(validator_index: u8, worker_ordinal: u32) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[0] = validator_index.saturating_add(1);
    bytes[28..].copy_from_slice(&worker_ordinal.to_be_bytes());
    B256::from(bytes)
}

#[cfg(feature = "ocomp-integration")]
impl Drop for OcompTopology {
    fn drop(&mut self) {
        if let Some((_, domain)) = self.keyless_full_node_domain.as_mut() {
            domain.workers.clear();
            domain.snapshot_exporter.take();
            domain.supervisor.take();
        }
        for domain in &mut self.domains {
            domain.workers.clear();
            domain.snapshot_exporter.take();
            domain.supervisor.take();
        }
    }
}

#[cfg(feature = "ocomp-integration")]
fn tail_file(path: &Path, max_lines: usize) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return format!("<unable to open {}>", path.display());
    };
    let size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = size.saturating_sub(16 * 1024);
    let _ = file.seek(SeekFrom::Start(start));
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    text.lines()
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(feature = "ocomp-integration")]
fn parse_outbe_chain_spec(path: &Path) -> Result<Arc<reth_chainspec::ChainSpec<OutbeHeader>>> {
    let path = path
        .to_str()
        .ok_or_else(|| eyre::eyre!("genesis path is not valid UTF-8"))?;
    Ok(reth_ethereum::cli::chainspec::chain_value_parser(path)?
        .as_ref()
        .clone()
        .map_header(OutbeHeader::new)
        .into())
}

#[cfg(feature = "ocomp-integration")]
fn genesis_chain_id(genesis: &serde_json::Value) -> Result<u64> {
    let value = genesis
        .get("config")
        .and_then(|config| config.get("chainId"))
        .ok_or_else(|| eyre::eyre!("generated genesis has no config.chainId"))?;
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| eyre::eyre!("genesis chainId is outside u64")),
        serde_json::Value::String(encoded) => {
            let encoded = encoded.strip_prefix("0x").unwrap_or(encoded);
            u64::from_str_radix(encoded, 16).map_err(Into::into)
        }
        _ => {
            eyre::bail!("genesis chainId is neither a number nor a hex string");
        }
    }
}

#[cfg(feature = "ocomp-integration")]
fn schedule_dynamic_membership_days(
    genesis: &mut serde_json::Value,
    chain_id: u64,
) -> Result<(WorldwideDay, WorldwideDay, u64, u64)> {
    const SECONDS_PER_DAY: u64 = 86_400;

    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let first_processing_time = genesis_timestamp
        .checked_add(OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS)
        .ok_or_else(|| eyre::eyre!("first dynamic OCOMP processing time overflow"))?;
    let second_processing_time = genesis_timestamp
        .checked_add(OCOMP_DYNAMIC_SECOND_OFFERING_AFTER_GENESIS_SECS)
        .ok_or_else(|| eyre::eyre!("second dynamic OCOMP processing time overflow"))?;
    let first_worldwide_day = crate::world::localnet::worldwide_day()
        .parse::<WorldwideDay>()
        .map_err(|error| eyre::eyre!("invalid measurement WorldwideDay: {error}"))?;
    let second_worldwide_day = WorldwideDay::from_timestamp(
        first_worldwide_day
            .start_timestamp()
            .checked_add(SECONDS_PER_DAY)
            .ok_or_else(|| eyre::eyre!("second dynamic OCOMP WorldwideDay overflow"))?,
    );

    let mut provider = HashMapStorageProvider::new(chain_id);
    {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
        let words = alloc
            .get(&metadosis_key)
            .and_then(|account| account.get("storage"))
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account has no storage object"))?;
        for (slot, value) in words {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot)?),
                parse_storage_word(value)?,
            );
        }
        if let Some(tribute_key) = find_alloc_address_key(alloc, TRIBUTE_ADDRESS)? {
            let tribute_words = alloc
                .get(&tribute_key)
                .and_then(|account| account.get("storage"))
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| eyre::eyre!("Tribute genesis account has no storage object"))?;
            for (slot, value) in tribute_words {
                provider.storage.insert(
                    (TRIBUTE_ADDRESS, parse_hex_word(slot)?),
                    parse_storage_word(value)?,
                );
            }
        }
    }

    StorageHandle::enter(&mut provider, |storage| {
        let first = outbe_metadosis::api::worldwide_day(storage.clone(), first_worldwide_day)?
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "dynamic OCOMP genesis is missing its seeded WorldwideDay".into(),
                )
            })?;
        FreshDevnetGenesisBuilder::new()
            .retime_offering_day(first_worldwide_day, first_processing_time)
            .seed_active_worldwide_day(outbe_metadosis::genesis::GenesisWorldwideDay {
                worldwide_day: second_worldwide_day,
                status: first.status,
                day_type: first.day_type,
                forming_start: first.forming_start,
                forming_end: first.forming_end,
                lookback_end: first.lookback_end,
                offering_end: second_processing_time,
                scheduled_process_time: second_processing_time,
                metadosis_limit_amount: first.metadosis_limit_amount,
                previous_vwap: first.previous_vwap,
                current_vwap: first.current_vwap,
            })
            .apply(storage.clone())?;
        outbe_tribute::TributeContract::new(storage).unseal_day(second_worldwide_day)
    })?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    for (address, label) in [
        (METADOSIS_ADDRESS, "Metadosis"),
        (TRIBUTE_ADDRESS, "Tribute"),
    ] {
        let account_key = match find_alloc_address_key(alloc, address)? {
            Some(account_key) => account_key,
            None if address == TRIBUTE_ADDRESS => {
                let account_key = hex::encode(address.as_slice());
                alloc.insert(
                    account_key.clone(),
                    serde_json::json!({
                        "code": "0xef",
                        "balance": "0x0",
                        "storage": {},
                    }),
                );
                account_key
            }
            None => {
                eyre::bail!("generated genesis has no {label} account");
            }
        };
        let words = alloc
            .get_mut(&account_key)
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|account| account.get_mut("storage"))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("{label} genesis account has no storage object"))?;
        for ((stored_address, slot), value) in &provider.storage {
            if *stored_address != address {
                continue;
            }
            let slot = format!("0x{slot:064x}");
            if value.is_zero() {
                words.remove(&slot);
            } else {
                words.insert(slot, serde_json::Value::String(format!("0x{value:064x}")));
            }
        }
    }

    Ok((
        first_worldwide_day,
        second_worldwide_day,
        first_processing_time,
        second_processing_time,
    ))
}

#[cfg(feature = "ocomp-integration")]
fn schedule_public_measurement_day(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    offering_after_genesis_secs: u64,
) -> Result<bool> {
    if offering_after_genesis_secs == 0 {
        eyre::bail!("OCOMP public measurement offering duration must be non-zero");
    }
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let offering_end = genesis_timestamp
        .checked_add(offering_after_genesis_secs)
        .ok_or_else(|| eyre::eyre!("OCOMP public measurement offering end overflow"))?;
    let worldwide_day = crate::world::localnet::worldwide_day()
        .parse::<WorldwideDay>()
        .map_err(|error| eyre::eyre!("invalid measurement WorldwideDay: {error}"))?;

    let mut provider = HashMapStorageProvider::new(chain_id);
    {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
        let words = alloc
            .get(&metadosis_key)
            .and_then(|account| account.get("storage"))
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account has no storage object"))?;
        for (slot, value) in words {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot)?),
                parse_storage_word(value)?,
            );
        }
    }

    let changed = StorageHandle::enter(&mut provider, |storage| {
        FreshDevnetGenesisBuilder::new()
            .retime_offering_day(worldwide_day, offering_end)
            .apply(storage)
            .map(|report| report.changed)
    })?;

    if !changed {
        return Ok(false);
    }
    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
        .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
    let words = alloc
        .get_mut(&metadosis_key)
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|account| account.get_mut("storage"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("Metadosis genesis account has no storage object"))?;
    for ((address, slot), value) in &provider.storage {
        if *address != METADOSIS_ADDRESS {
            continue;
        }
        let slot = format!("0x{slot:064x}");
        if value.is_zero() {
            words.remove(&slot);
        } else {
            words.insert(slot, serde_json::Value::String(format!("0x{value:064x}")));
        }
    }
    Ok(true)
}

#[cfg(feature = "ocomp-integration")]
fn clear_seeded_metadosis_days(genesis: &mut serde_json::Value, chain_id: u64) -> Result<bool> {
    let mut provider = HashMapStorageProvider::new(chain_id);
    {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
        let words = alloc
            .get(&metadosis_key)
            .and_then(|account| account.get("storage"))
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account has no storage object"))?;
        for (slot, value) in words {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot)?),
                parse_storage_word(value)?,
            );
        }
    }

    let seeded = crate::world::localnet::worldwide_day()
        .parse::<WorldwideDay>()
        .map_err(|error| eyre::eyre!("invalid fixture WorldwideDay: {error}"))?;
    StorageHandle::enter(&mut provider, |storage| {
        FreshDevnetGenesisBuilder::new()
            .clear_single_offering_day(seeded)
            .apply(storage)
    })?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
        .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
    let words = alloc
        .get_mut(&metadosis_key)
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|account| account.get_mut("storage"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("Metadosis genesis account has no storage object"))?;
    for ((address, slot), value) in &provider.storage {
        if *address != METADOSIS_ADDRESS {
            continue;
        }
        let slot = format!("0x{slot:064x}");
        if value.is_zero() {
            words.remove(&slot);
        } else {
            words.insert(slot, serde_json::Value::String(format!("0x{value:064x}")));
        }
    }
    Ok(true)
}

#[cfg(feature = "ocomp-integration")]
fn apply_measurement_gas_envelope(genesis: &mut serde_json::Value) -> Result<bool> {
    let object = genesis
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("measurement genesis must be a JSON object"))?;
    let expected = serde_json::Value::String(format!("0x{OCOMP_MEASUREMENT_BLOCK_GAS_LIMIT:x}"));
    if object.get("gasLimit") == Some(&expected) {
        return Ok(false);
    }
    object.insert("gasLimit".to_owned(), expected);
    Ok(true)
}

#[cfg(feature = "ocomp-integration")]
fn find_alloc_address_key(
    alloc: &serde_json::Map<String, serde_json::Value>,
    expected: Address,
) -> Result<Option<String>> {
    for key in alloc.keys() {
        let normalized = if key.starts_with("0x") {
            key.clone()
        } else {
            format!("0x{key}")
        };
        if Address::from_str(&normalized)
            .map_err(|error| eyre::eyre!("invalid genesis alloc address {key}: {error}"))?
            == expected
        {
            return Ok(Some(key.clone()));
        }
    }
    Ok(None)
}

#[cfg(feature = "ocomp-integration")]
fn parse_storage_word(value: &serde_json::Value) -> Result<U256> {
    let encoded = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("genesis storage word is not a string"))?;
    parse_hex_word(encoded)
}

#[cfg(feature = "ocomp-integration")]
fn parse_hex_word(encoded: &str) -> Result<U256> {
    U256::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16).map_err(Into::into)
}

#[cfg(feature = "ocomp-integration")]
fn capacity_tribute_private_keys(count: usize) -> Result<Vec<String>> {
    const FIRST_CAPACITY_SCALAR: u64 = 0x1_0000;

    let mut private_keys = Vec::new();
    private_keys.try_reserve_exact(count)?;
    for index in 0..count {
        let scalar = FIRST_CAPACITY_SCALAR
            .checked_add(u64::try_from(index)?)
            .ok_or_else(|| eyre::eyre!("capacity Tribute owner scalar overflow"))?;
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&scalar.to_be_bytes());
        SigningKey::from_bytes((&bytes).into())
            .map_err(|error| eyre::eyre!("invalid capacity Tribute owner scalar: {error}"))?;
        private_keys.push(format!("0x{}", hex::encode(bytes)));
    }
    Ok(private_keys)
}

#[cfg(feature = "ocomp-integration")]
fn fund_capacity_tribute_accounts(
    genesis: &mut serde_json::Value,
    private_keys: &[String],
) -> Result<bool> {
    const CAPACITY_OWNER_BALANCE_COEN: u64 = 1_000;
    const COEN_BASE_UNITS: u64 = 1_000_000_000_000_000_000;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let balance = U256::from(CAPACITY_OWNER_BALANCE_COEN)
        .checked_mul(U256::from(COEN_BASE_UNITS))
        .ok_or_else(|| eyre::eyre!("capacity Tribute owner balance overflow"))?;
    let balance_hex = format!("{balance:#x}");
    let mut changed = false;
    for private_key in private_keys {
        let owner = crate::internal::eth::address_of(private_key)
            .ok_or_else(|| eyre::eyre!("cannot derive capacity Tribute owner"))?;
        let key = format!("{owner:#x}");
        match alloc.get(&key) {
            Some(existing)
                if existing.as_object().is_some_and(|account| {
                    account.len() == 1
                        && account.get("balance").and_then(serde_json::Value::as_str)
                            == Some(balance_hex.as_str())
                }) => {}
            Some(_) => {
                eyre::bail!(
                    "capacity Tribute owner {owner:#x} collides with a different genesis account"
                );
            }
            None => {
                alloc.insert(
                    key,
                    serde_json::json!({
                        "balance": balance_hex,
                    }),
                );
                changed = true;
            }
        }
    }
    Ok(changed)
}

#[cfg(feature = "ocomp-integration")]
fn measurement_fork_install(
    chain_id: u64,
    genesis_hash: B256,
    activation_height: u64,
    validators_path: &Path,
    limits: &outbe_ocomp_protocol::SchemaLimits,
    max_terminal_job_records: Option<u16>,
) -> Result<OcompForkInstallV1> {
    let protocol_bundle = provisional_measurement_bundle();
    let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(limits)?;
    let founder_registrations =
        measurement_founder_registrations(validators_path, chain_id, genesis_hash, limits)?;
    let mut capacity_profile = provisional_measurement_capacity_profile();
    if let Some(max_terminal_job_records) = max_terminal_job_records {
        eyre::ensure!(
            max_terminal_job_records > 0,
            "Measurement terminal job record cap must be non-zero"
        );
        capacity_profile.max_terminal_job_records = max_terminal_job_records;
    }
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
fn measurement_founder_registrations(
    validators_path: &Path,
    chain_id: u64,
    genesis_hash: B256,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> Result<Vec<OcompKeyRegistrationV1>> {
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(validators_path)?)?;
    let validators = manifest
        .as_array()
        .ok_or_else(|| eyre::eyre!("validators manifest must be a JSON array"))?;
    let max_validators = usize::try_from(outbe_consensus::bls::MAX_VALIDATORS)?;
    eyre::ensure!(
        !validators.is_empty(),
        "validators manifest must not be empty"
    );
    eyre::ensure!(
        validators.len() <= max_validators,
        "validators manifest exceeds consensus bound {max_validators}"
    );

    validators
        .iter()
        .enumerate()
        .map(|(index, validator)| {
            let address = validator
                .get("address")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("validator-{index} has no address"))
                .and_then(|value| Address::from_str(value).map_err(Into::into))?;
            let consensus_key = validator
                .get("public_key")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("validator-{index} has no public_key"))?;
            let consensus_key =
                hex::decode(consensus_key.strip_prefix("0x").unwrap_or(consensus_key))?;
            let consensus_key: [u8; 48] = consensus_key.try_into().map_err(|value: Vec<u8>| {
                eyre::eyre!(
                    "validator-{index} BLS MinPk must be 48 bytes, got {}",
                    value.len()
                )
            })?;
            let signing_key = measurement_signing_key(u8::try_from(index)?);
            let public_key: [u8; 33] = signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| eyre::eyre!("measurement OCOMP public key is not SEC1-33"))?;
            let mut registration = OcompKeyRegistrationV1 {
                core: OcompKeyRegistrationCoreV1 {
                    chain_id,
                    genesis_hash,
                    validator_identity_hash: validator_identity_hash_v1(address, &consensus_key)?,
                    ocomp_public_key_sec1: public_key,
                    key_epoch: POC_KEY_EPOCH,
                    allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
                },
                proof_of_possession: [0; 64],
            };
            let proof: Signature = signing_key
                .sign_prehash(registration.proof_of_possession_digest(limits)?.as_slice())
                .map_err(|_| eyre::eyre!("cannot sign validator-{index} OCOMP PoP"))?;
            registration.proof_of_possession =
                proof.normalize_s().unwrap_or(proof).to_bytes().into();
            registration.validate_proof_of_possession(limits)?;
            Ok(registration)
        })
        .collect()
}

#[cfg(feature = "ocomp-integration")]
fn measurement_signing_key(validator_index: u8) -> SigningKey {
    SigningKey::from_bytes((&[validator_index.saturating_add(1); 32]).into())
        .expect("indices 0..3 produce valid deterministic measurement scalars")
}

#[cfg(feature = "ocomp-integration")]
fn ocomp_evm_private_key(validator_index: u8) -> String {
    hex::encode([validator_index.saturating_add(0x71); 32])
}

#[cfg(feature = "ocomp-integration")]
fn provisional_measurement_capacity_profile() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: B256::repeat_byte(13),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        retry_backoff_blocks: 1,
        max_terminal_job_records: 365,
        max_reference_currencies: 256,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks: outbe_ocomp_protocol::profile::OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: B256::repeat_byte(23),
    }
}

#[cfg(feature = "ocomp-integration")]
fn publish_exact_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
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
fn replace_json_atomically(path: &Path, value: &serde_json::Value) -> Result<()> {
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

fn unix_time_millis() -> u64 {
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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use crate::env::Environment;
    use crate::internal::proc::ChildGuard;
    use alloy_primitives::B256;
    #[cfg(feature = "ocomp-integration")]
    use outbe_metadosis::{genesis::GenesisWorldwideDay, WwdDayType, WwdStatus};

    use super::*;

    const CHILD_MODE: &str = "OUTBE_OCOMP_TOPOLOGY_CHILD";

    struct TestTopology {
        _directory: tempfile::TempDir,
        topology: OcompTopology,
    }

    impl std::ops::Deref for TestTopology {
        type Target = OcompTopology;

        fn deref(&self) -> &Self::Target {
            &self.topology
        }
    }

    impl std::ops::DerefMut for TestTopology {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.topology
        }
    }

    fn topology_with_validators(validators: usize) -> TestTopology {
        let directory = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: directory.path().to_path_buf(),
            validators,
            ..Environment::default()
        };
        env.ports.start_scenario(env.validators).unwrap();
        TestTopology {
            _directory: directory,
            topology: OcompTopology::new(Config::for_scenario(&env, 1)),
        }
    }

    fn topology() -> TestTopology {
        topology_with_validators(Environment::default().validators)
    }

    fn child_guard() -> ChildGuard {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("world::ocomp::tests::typed_fault_stops_only_the_selected_owned_process")
            .arg("--nocapture")
            .env(CHILD_MODE, "1");
        ChildGuard::spawn("ocomp topology child", command).unwrap()
    }

    fn launch_identity_evidence(
        activation_height: u64,
        fork_install_hash: B256,
    ) -> OcompLaunchIdentityEvidenceV1 {
        OcompLaunchIdentityEvidenceV1 {
            chain_id: 1,
            genesis_hash: format!("{:#x}", B256::repeat_byte(9)),
            protocol_bundle_hash: format!("{:#x}", B256::repeat_byte(8)),
            fork_install_hash: format!("{fork_install_hash:#x}"),
            classification: "final".to_owned(),
            activation_height,
            metadosis_storage_layout_hash: METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX.to_owned(),
        }
    }

    #[cfg(feature = "ocomp-integration")]
    fn stage_completed_job_footprint(topology: &OcompTopology, job_id: B256) {
        let job_component = hex::encode(job_id);
        for validator_index in topology.validator_indices().unwrap() {
            let root = topology.domain_root(validator_index).unwrap();
            let admissions = root
                .join("supervisor-v1")
                .join("jobs")
                .join(&job_component)
                .join("admissions");
            let worker_outputs = root.join("worker-inbox-v1").join("artifacts");
            let votes = root.join("supervisor-v1").join("vote-submissions");
            fs::create_dir_all(&admissions).unwrap();
            fs::create_dir_all(&worker_outputs).unwrap();
            fs::create_dir_all(&votes).unwrap();
            fs::write(admissions.join("0000000000.admission"), b"same-admission").unwrap();
            fs::write(worker_outputs.join("unit.ocb1"), b"same-worker-output").unwrap();
            fs::write(
                votes.join(format!("{job_component}.vote.v1")),
                [validator_index],
            )
            .unwrap();
        }
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn completed_job_artifacts_prove_four_isolated_deterministic_footprints() {
        let topology = topology();
        let job_id = B256::repeat_byte(0x42);
        stage_completed_job_footprint(&topology, job_id);

        topology.verify_completed_job_artifacts(job_id).unwrap();
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn staged_joiner_domain_uses_its_registration_key_and_the_pinned_bundle() {
        let topology = topology_with_validators(4);
        let founder_bundle = topology
            .domain_root(0)
            .unwrap()
            .join("protocol-bundle-v1.ocb1");
        fs::create_dir_all(founder_bundle.parent().unwrap()).unwrap();
        fs::write(&founder_bundle, b"pinned-bundle").unwrap();
        let joiner_key = topology.cfg.validator_dir(4).join("ocomp-key-v1.hex");
        fs::create_dir_all(joiner_key.parent().unwrap()).unwrap();
        fs::write(&joiner_key, b"joiner-registration-secret\n").unwrap();

        topology.stage_joiner_domain_material(4).unwrap();

        let staged = topology
            .cfg
            .validator_dir(4)
            .join("ocomp")
            .join("domain-v1");
        assert_eq!(
            fs::read(staged.join("protocol-bundle-v1.ocb1")).unwrap(),
            b"pinned-bundle"
        );
        assert_eq!(
            fs::read(staged.join("ocomp-key-v1.hex")).unwrap(),
            b"joiner-registration-secret\n"
        );
        let operational_key = fs::read(staged.join("ocomp-evm-key.hex")).unwrap();
        assert_eq!(operational_key.len(), 65);
        assert_eq!(operational_key[64], b'\n');
        assert!(operational_key[..64]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)));
        assert!(topology.domain_root(4).is_err());
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn completed_job_artifacts_reject_one_domain_with_different_worker_output() {
        let topology = topology();
        let job_id = B256::repeat_byte(0x43);
        stage_completed_job_footprint(&topology, job_id);
        let changed = topology
            .domain_root(3)
            .unwrap()
            .join("worker-inbox-v1")
            .join("artifacts")
            .join("unit.ocb1");
        fs::write(changed, b"different-worker-output").unwrap();

        let error = topology.verify_completed_job_artifacts(job_id).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("retained different deterministic worker outputs"),
            "{error:#}"
        );
    }

    #[test]
    fn topology_follows_the_configured_validator_count() {
        let topology = topology_with_validators(5);
        let roots = (0..5_u8)
            .map(|index| topology.domain_root(index).unwrap().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(roots.len(), 5);
        assert_eq!(
            roots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            5
        );
        assert!(topology.domain_root(5).is_err());
        assert!(topology.process_records().is_empty());
    }

    #[test]
    fn topology_evidence_covers_every_configured_validator() {
        let topology = topology_with_validators(5);

        let evidence = topology.evidence_snapshot().unwrap();

        assert_eq!(evidence.domain_roots.len(), 5);
    }

    #[test]
    fn active_joiner_appends_one_domain_without_rewriting_existing_roots() {
        let mut topology = topology_with_validators(4);
        let original = topology
            .validator_indices()
            .unwrap()
            .into_iter()
            .map(|index| topology.domain_root(index).unwrap().to_owned())
            .collect::<Vec<_>>();

        topology.add_active_validator_domain(4).unwrap();

        assert_eq!(topology.evidence_snapshot().unwrap().domain_roots.len(), 5);
        for (index, expected) in original.iter().enumerate() {
            assert_eq!(
                topology.domain_root(u8::try_from(index).unwrap()).unwrap(),
                expected
            );
        }
        assert!(topology.add_active_validator_domain(4).is_err());
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn keyless_full_node_profile_is_complete_without_joining_active_topology() {
        let mut topology = topology_with_validators(4);
        prepare_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_measurement_fork_install().unwrap();
        topology.launch_identity = Some(prepared.launch_identity());

        let args = topology.stage_keyless_full_node_domain(4).unwrap();

        assert_eq!(topology.evidence_snapshot().unwrap().domain_roots.len(), 4);
        assert!(args.is_empty());

        let root = topology
            .cfg
            .validator_dir(4)
            .join("ocomp")
            .join("domain-v1");
        assert!(root.join("protocol-bundle-v1.ocb1").is_file());
        assert!(!root.join("ocomp-key-v1.hex").exists());
        assert!(!root.join("ocomp-evm-key.hex").exists());
    }

    #[test]
    fn fork_restart_evidence_requires_recovery_on_each_side_of_h() {
        let mut topology = topology();
        topology.launch_identity_evidence =
            Some(launch_identity_evidence(32, B256::repeat_byte(1)));
        topology
            .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
                validator_index: 0,
                activation_height: 32,
                pre_fork_restart_from_height: 2,
                pre_fork_rejoined_height: 4,
                down_across_fork_from_height: 30,
                finalized_while_down_height: 33,
                replayed_through_height: 34,
                post_fork_restart_from_height: 35,
                post_fork_rejoined_height: 36,
            })
            .unwrap();

        let snapshot = topology.evidence_snapshot().unwrap();
        snapshot.validate().unwrap();
        assert_eq!(snapshot.fork_restart.unwrap().replayed_through_height, 34);
    }

    #[test]
    fn fork_restart_evidence_requires_the_exact_launch_identity() {
        let mut topology = topology();
        let error = topology
            .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
                validator_index: 0,
                activation_height: 32,
                pre_fork_restart_from_height: 2,
                pre_fork_rejoined_height: 4,
                down_across_fork_from_height: 30,
                finalized_while_down_height: 33,
                replayed_through_height: 34,
                post_fork_restart_from_height: 35,
                post_fork_rejoined_height: 36,
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("requires the exact OCOMP launch identity"));
    }

    #[test]
    fn fork_restart_evidence_rejects_a_validator_that_did_not_replay_h() {
        let mut topology = topology();
        topology.launch_identity_evidence =
            Some(launch_identity_evidence(32, B256::repeat_byte(1)));
        let error = topology
            .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
                validator_index: 0,
                activation_height: 32,
                pre_fork_restart_from_height: 2,
                pre_fork_rejoined_height: 4,
                down_across_fork_from_height: 30,
                finalized_while_down_height: 33,
                replayed_through_height: 31,
                post_fork_restart_from_height: 35,
                post_fork_rejoined_height: 36,
            })
            .unwrap_err();

        assert!(error.to_string().contains("does not span H-1/H/H+1 safely"));
        assert!(topology.evidence_snapshot().unwrap().fork_restart.is_none());
    }

    #[test]
    fn fork_mismatch_evidence_requires_canonical_progress_and_isolated_head() {
        let mut topology = topology();
        topology.launch_identity_evidence =
            Some(launch_identity_evidence(32, B256::repeat_byte(1)));
        topology
            .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
                validator_index: 0,
                canonical_install_hash: format!("{:#x}", B256::repeat_byte(1)),
                mismatched_install_hash: format!("{:#x}", B256::repeat_byte(2)),
                canonical_activation_height: 32,
                mismatched_activation_height: 32,
                canonical_head_before_restart: 7,
                mismatched_head_after_fork: 31,
                canonical_finalized_after_fork: 33,
            })
            .unwrap();

        let snapshot = topology.evidence_snapshot().unwrap();
        snapshot.validate().unwrap();
        assert_eq!(
            snapshot
                .fork_mismatch
                .unwrap()
                .canonical_finalized_after_fork,
            33
        );
    }

    #[test]
    fn fork_mismatch_evidence_rejects_a_different_canonical_launch_identity() {
        let mut topology = topology();
        topology.launch_identity_evidence =
            Some(launch_identity_evidence(32, B256::repeat_byte(3)));
        let error = topology
            .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
                validator_index: 0,
                canonical_install_hash: format!("{:#x}", B256::repeat_byte(1)),
                mismatched_install_hash: format!("{:#x}", B256::repeat_byte(2)),
                canonical_activation_height: 32,
                mismatched_activation_height: 32,
                canonical_head_before_restart: 7,
                mismatched_head_after_fork: 31,
                canonical_finalized_after_fork: 33,
            })
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("does not match the launch identity"));
    }

    #[test]
    fn fork_mismatch_evidence_rejects_a_node_that_imported_h() {
        let mut topology = topology();
        topology.launch_identity_evidence =
            Some(launch_identity_evidence(32, B256::repeat_byte(1)));
        let error = topology
            .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
                validator_index: 0,
                canonical_install_hash: format!("{:#x}", B256::repeat_byte(1)),
                mismatched_install_hash: format!("{:#x}", B256::repeat_byte(2)),
                canonical_activation_height: 32,
                mismatched_activation_height: 32,
                canonical_head_before_restart: 7,
                mismatched_head_after_fork: 32,
                canonical_finalized_after_fork: 33,
            })
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("does not prove fail-closed isolation"));
        assert!(topology
            .evidence_snapshot()
            .unwrap()
            .fork_mismatch
            .is_none());
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn measurement_fork_install_arms_genesis_without_a_synthetic_update() {
        let topology = topology();
        prepare_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_measurement_fork_install().unwrap();
        assert_eq!(
            prepared.install.classification,
            OcompForkInstallClassification::Measurement
        );
        assert_eq!(
            prepared.install.activation_height,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT
        );
        assert_eq!(
            prepared.install.founder_registrations.len(),
            topology.domains.len()
        );
        for (index, registration) in prepared.install.founder_registrations.iter().enumerate() {
            assert_eq!(
                registration.core.validator_identity_hash,
                validator_identity_hash_v1(
                    Address::with_last_byte(u8::try_from(index).unwrap() + 1),
                    &[u8::try_from(index).unwrap() + 11; 48],
                )
                .unwrap()
            );
            assert_eq!(
                registration.core.ocomp_public_key_sec1.as_slice(),
                measurement_signing_key(u8::try_from(index).unwrap())
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
            );
        }
        assert_eq!(
            format!("{METADOSIS_STORAGE_LAYOUT_V1_HASH:#x}"),
            METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX
        );

        let chain_spec = parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json")).unwrap();
        let loaded =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&chain_spec).unwrap();
        assert_eq!(loaded.as_ref(), &prepared.install);
        assert_eq!(
            loaded
                .install_hash(&outbe_ocomp_protocol::profile::poc_schema_limits())
                .unwrap(),
            prepared.install_hash
        );

        let genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        let alloc = genesis["alloc"].as_object().unwrap();
        assert!(
            find_alloc_address_key(alloc, outbe_primitives::addresses::UPDATE_ADDRESS)
                .unwrap()
                .is_none(),
            "measurement genesis must not schedule a generic Update for OCOMP"
        );

        let bundles = topology
            .validator_indices()
            .unwrap()
            .into_iter()
            .map(|index| {
                std::fs::read(
                    topology
                        .domain_root(index)
                        .unwrap()
                        .join("protocol-bundle-v1.ocb1"),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        assert!(bundles.iter().all(|bundle| bundle == &bundles[0]));
        for index in topology.validator_indices().unwrap() {
            let key = std::fs::read_to_string(
                topology
                    .domain_root(index)
                    .unwrap()
                    .join("ocomp-key-v1.hex"),
            )
            .unwrap();
            let signer =
                SigningKey::from_bytes((&hex::decode(key.trim()).unwrap()[..]).into()).unwrap();
            assert_eq!(
                signer.verifying_key().to_encoded_point(true).as_bytes(),
                measurement_signing_key(index)
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
            );
        }
        assert_eq!(
            topology.prepare_measurement_fork_install().unwrap(),
            prepared
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn bootstrapped_runtime_preserves_the_exact_genesis_result_signing_keys() {
        let topology = topology();
        prepare_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_measurement_fork_install().unwrap();
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let canonical_bundle = prepared
            .install
            .protocol_bundle
            .encode_canonical(&limits)
            .unwrap();
        fs::write(
            topology.cfg.dir.join("protocol-bundle-v1.ocb1"),
            canonical_bundle,
        )
        .unwrap();

        let mut expected_keys = Vec::new();
        for (index, registration) in prepared.install.founder_registrations.iter().enumerate() {
            let domain_key = fs::read(
                topology
                    .domain_root(u8::try_from(index).unwrap())
                    .unwrap()
                    .join("ocomp-key-v1.hex"),
            )
            .unwrap();
            expected_keys.push(domain_key.clone());
            fs::write(
                topology.cfg.validator_dir(index).join("ocomp-key-v1.hex"),
                domain_key,
            )
            .unwrap();
            fs::write(
                topology
                    .cfg
                    .validator_dir(index)
                    .join("ocomp-registration-v1.ocb1"),
                registration.encode_canonical(&limits).unwrap(),
            )
            .unwrap();
            fs::remove_dir_all(topology.domain_root(u8::try_from(index).unwrap()).unwrap())
                .unwrap();
        }

        let identity = topology.prepare_bootstrapped_runtime().unwrap();
        assert_eq!(identity, prepared.launch_identity());
        for (index, expected_key) in expected_keys.iter().enumerate() {
            assert_eq!(
                fs::read(
                    topology
                        .domain_root(u8::try_from(index).unwrap())
                        .unwrap()
                        .join("ocomp-key-v1.hex")
                )
                .unwrap(),
                *expected_key
            );
        }
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn bootstrapped_runtime_rejects_a_key_substituted_after_genesis() {
        let topology = topology();
        prepare_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_measurement_fork_install().unwrap();
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        fs::write(
            topology.cfg.dir.join("protocol-bundle-v1.ocb1"),
            prepared
                .install
                .protocol_bundle
                .encode_canonical(&limits)
                .unwrap(),
        )
        .unwrap();
        fs::write(
            topology
                .cfg
                .validator_dir(0)
                .join("ocomp-registration-v1.ocb1"),
            prepared.install.founder_registrations[0]
                .encode_canonical(&limits)
                .unwrap(),
        )
        .unwrap();
        let substituted = SigningKey::from_bytes((&[99_u8; 32]).into()).unwrap();
        fs::write(
            topology.cfg.validator_dir(0).join("ocomp-key-v1.hex"),
            format!("{}\n", hex::encode(substituted.to_bytes())),
        )
        .unwrap();
        fs::remove_dir_all(topology.domain_root(0).unwrap()).unwrap();

        let error = topology.prepare_bootstrapped_runtime().unwrap_err();
        assert!(error
            .to_string()
            .contains("result-signing key does not match its genesis registration"));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn supervisor_readiness_requires_the_expected_registered_connected_workers() {
        let ready = SupervisorWorkerStatusV1 {
            registry_generation: 1,
            registered_workers: 1,
            connected_workers: 1,
            busy_workers: 0,
            accepted_leases: 0,
            queued_units: 0,
            max_workers: 4,
        };
        ensure_supervisor_status_ready(0, &ready, 1).unwrap();

        let mut missing = ready.clone();
        missing.registered_workers = 0;
        assert!(ensure_supervisor_status_ready(0, &missing, 1).is_err());

        let mut disconnected = ready;
        disconnected.connected_workers = 0;
        assert!(ensure_supervisor_status_ready(0, &disconnected, 1).is_err());
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn measurement_fork_install_rejects_layout_mismatch_before_node_start() {
        let topology = topology();
        prepare_measurement_genesis_fixture(&topology);
        topology.prepare_measurement_fork_install().unwrap();
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        genesis["config"][outbe_node::ocomp::fork::METADOSIS_STORAGE_LAYOUT_GENESIS_KEY]
            ["layoutHash"] = serde_json::json!(alloy_primitives::B256::repeat_byte(0x44));
        replace_json_atomically(&genesis_path, &genesis).unwrap();

        let mismatched = parse_outbe_chain_spec(&genesis_path).unwrap();
        let error =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&mismatched).unwrap_err();
        assert!(error.to_string().contains("layout hash mismatch"));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn public_measurement_schedule_keeps_the_seeded_day_offering_then_reaches_ready() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_public_measurement_fork_install().unwrap();
        let genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        let genesis_timestamp =
            u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
        let worldwide_day = crate::world::localnet::worldwide_day()
            .parse::<WorldwideDay>()
            .unwrap();
        let alloc = genesis["alloc"].as_object().unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        StorageHandle::enter(&mut provider, |storage| {
            let day = outbe_metadosis::api::worldwide_day(storage, worldwide_day)
                .unwrap()
                .unwrap();
            assert_eq!(day.status, WwdStatus::Offering);
            assert_eq!(
                day.offering_end,
                genesis_timestamp + OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS
            );
            assert_eq!(
                day.scheduled_process_time,
                genesis_timestamp + OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS
            );
        });
        assert_eq!(
            prepared.install.request_profile.genesis_hash,
            parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
                .unwrap()
                .genesis_hash()
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn dynamic_membership_fixture_schedules_two_distinct_public_jobs() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut configured: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        configured["config"][outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY] =
            serde_json::json!(OCOMP_TEST_EPOCH_LENGTH_BLOCKS);
        configured["config"]["dkgPrepareWindowBlocks"] =
            serde_json::json!(OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS);
        std::fs::write(
            &genesis_path,
            serde_json::to_vec_pretty(&configured).unwrap(),
        )
        .unwrap();

        let prepared = topology.prepare_dynamic_membership_fork_install().unwrap();

        let genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        assert_eq!(
            genesis["config"][outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY],
            serde_json::json!(OCOMP_TEST_EPOCH_LENGTH_BLOCKS)
        );
        let alloc = genesis["alloc"].as_object().unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        let tribute_key = find_alloc_address_key(alloc, TRIBUTE_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        for (slot, value) in alloc[&tribute_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (TRIBUTE_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        StorageHandle::enter(&mut provider, |storage| {
            let days = outbe_metadosis::api::worldwide_days(storage.clone()).unwrap();
            assert_eq!(days.len(), 2);
            assert_eq!(
                days.iter()
                    .map(|day| (day.worldwide_day, day.status, day.scheduled_process_time))
                    .collect::<Vec<_>>(),
                vec![
                    (
                        prepared.first_worldwide_day,
                        WwdStatus::Offering,
                        prepared.first_processing_time,
                    ),
                    (
                        prepared.second_worldwide_day,
                        WwdStatus::Offering,
                        prepared.second_processing_time,
                    ),
                ]
            );
            let second_totals = outbe_tribute::TributeContract::new(storage.clone())
                .get_day_totals(prepared.second_worldwide_day)
                .unwrap();
            assert!(second_totals.initialized);
            assert!(!second_totals.is_sealed);
        });
        assert!(prepared.first_processing_time < prepared.second_processing_time);
        assert_eq!(prepared.fork.install.founder_registrations.len(), 4);
        let chain_spec = parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json")).unwrap();
        outbe_node::ocomp::fork::load_ocomp_fork_install(&chain_spec)
            .unwrap()
            .expect("dynamic membership fixture is startup-valid");
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn dynamic_membership_fixture_refuses_a_post_seed_epoch_rewrite() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);

        let error = topology
            .prepare_dynamic_membership_fork_install()
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("must be configured before ValidatorSet genesis is seeded"));
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn public_capacity_fixture_funds_every_distinct_tribute_owner_before_genesis_is_bound() {
        const TRIBUTE_COUNT: usize = 257;

        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let (prepared, private_keys) = topology
            .prepare_public_capacity_fork_install(TRIBUTE_COUNT)
            .unwrap();

        assert_eq!(private_keys.len(), TRIBUTE_COUNT);
        assert_eq!(
            private_keys
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            TRIBUTE_COUNT
        );

        let genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        let genesis_timestamp =
            u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
        let alloc = genesis["alloc"].as_object().unwrap();
        for private_key in private_keys {
            let owner = crate::internal::eth::address_of(&private_key).unwrap();
            let alloc_key = find_alloc_address_key(alloc, owner)
                .unwrap()
                .expect("capacity Tribute owner is funded in base genesis");
            assert!(
                parse_hex_word(alloc[&alloc_key]["balance"].as_str().unwrap()).unwrap()
                    > U256::ZERO
            );
        }
        let worldwide_day = crate::world::localnet::worldwide_day()
            .parse::<WorldwideDay>()
            .unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        StorageHandle::enter(&mut provider, |storage| {
            let day = outbe_metadosis::api::worldwide_day(storage, worldwide_day)
                .unwrap()
                .unwrap();
            assert_eq!(day.status, WwdStatus::Offering);
            assert_eq!(
                day.offering_end,
                genesis_timestamp + OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS
            );
            assert_eq!(
                day.scheduled_process_time,
                genesis_timestamp + OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS
            );
        });
        assert_eq!(
            prepared.install.request_profile.genesis_hash,
            parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
                .unwrap()
                .genesis_hash()
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn fresh_metadosis_capacity_fixture_starts_without_active_day_or_shortened_phases() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let (prepared, private_keys) = topology
            .prepare_fresh_metadosis_capacity_fork_install(3)
            .unwrap();
        assert_eq!(private_keys.len(), 3);

        let genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        let alloc = genesis["alloc"].as_object().unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        let seeded = crate::world::localnet::worldwide_day()
            .parse::<WorldwideDay>()
            .unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            assert!(
                outbe_metadosis::test_support::fresh_devnet_sentinel_is_pristine(storage, seeded)
                    .unwrap()
            );
        });
        assert_eq!(
            prepared.install.activation_height,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT
        );
        assert_eq!(
            prepared.install.request_profile.genesis_hash,
            parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json"))
                .unwrap()
                .genesis_hash()
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn mismatched_fork_manifest_is_valid_but_has_a_distinct_install_identity() {
        let topology = topology();
        prepare_measurement_genesis_fixture(&topology);
        let canonical = topology.prepare_measurement_fork_install().unwrap();
        let mismatched = topology.prepare_mismatched_fork_manifest(0).unwrap();

        assert_eq!(mismatched.canonical_install_hash, canonical.install_hash);
        assert_ne!(mismatched.mismatched_install_hash, canonical.install_hash);
        let canonical_spec =
            parse_outbe_chain_spec(&topology.cfg.dir.join("genesis.json")).unwrap();
        let mismatched_spec = parse_outbe_chain_spec(&mismatched.path).unwrap();
        assert_eq!(
            canonical_spec.genesis_hash(),
            mismatched_spec.genesis_hash()
        );
        let loaded =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&mismatched_spec).unwrap();
        assert_eq!(
            loaded.activation_height,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT
        );
        assert_ne!(
            loaded.request_profile.source_availability_policy_id,
            canonical
                .install
                .request_profile
                .source_availability_policy_id
        );
    }

    #[cfg(feature = "ocomp-integration")]
    fn prepare_measurement_genesis_fixture(topology: &OcompTopology) {
        std::fs::create_dir_all(&topology.cfg.dir).unwrap();
        let spec = reth_chainspec::ChainSpec::<OutbeHeader>::default();
        let mut genesis = serde_json::to_value(&spec.genesis).unwrap();
        genesis["config"][outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY] =
            serde_json::json!(OCOMP_TEST_EPOCH_LENGTH_BLOCKS);
        std::fs::write(
            topology.cfg.dir.join("genesis.json"),
            serde_json::to_vec_pretty(&genesis).unwrap(),
        )
        .unwrap();
        let validators = (0..4_u8)
            .map(|index| {
                serde_json::json!({
                    "address": format!("{:#x}", Address::with_last_byte(index + 1)),
                    "public_key": format!("0x{}", hex::encode([index + 11; 48])),
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            topology.cfg.dir.join("validators.json"),
            serde_json::to_vec_pretty(&validators).unwrap(),
        )
        .unwrap();
    }

    #[cfg(feature = "ocomp-integration")]
    fn prepare_public_measurement_genesis_fixture(topology: &OcompTopology) {
        prepare_measurement_genesis_fixture(topology);
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        genesis["timestamp"] = serde_json::Value::String(format!("0x{now:x}"));
        let chain_id = genesis_chain_id(&genesis).unwrap();
        let worldwide_day = crate::world::localnet::worldwide_day()
            .parse::<WorldwideDay>()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(chain_id);
        StorageHandle::enter(&mut provider, |storage| {
            FreshDevnetGenesisBuilder::new()
                .seed_active_worldwide_day(GenesisWorldwideDay {
                    worldwide_day,
                    status: WwdStatus::Offering,
                    day_type: WwdDayType::Green,
                    forming_start: now.saturating_sub(30),
                    forming_end: now.saturating_sub(20),
                    lookback_end: now.saturating_sub(10),
                    offering_end: 4_000_000_000,
                    scheduled_process_time: 4_100_000_000,
                    metadosis_limit_amount: U256::from(500),
                    previous_vwap: U256::ZERO,
                    current_vwap: U256::from(1),
                })
                .apply(storage)
                .unwrap();
        });
        let storage = provider
            .storage
            .iter()
            .filter(|((address, _), value)| *address == METADOSIS_ADDRESS && !value.is_zero())
            .map(|((_, slot), value)| {
                (
                    format!("0x{slot:064x}"),
                    serde_json::Value::String(format!("0x{value:064x}")),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        genesis["alloc"][format!("{METADOSIS_ADDRESS:#x}")] = serde_json::json!({
            "balance": "0x0",
            "storage": storage,
        });
        std::fs::write(genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
    }

    #[test]
    fn typed_fault_stops_only_the_selected_owned_process() {
        if std::env::var_os(CHILD_MODE).is_some() {
            loop {
                std::thread::park_timeout(std::time::Duration::from_secs(60));
            }
        }

        let mut topology = topology();
        topology
            .attach_owned(Some(0), OcompProcessRole::Supervisor, None, child_guard())
            .unwrap();
        topology
            .attach_owned(Some(1), OcompProcessRole::Supervisor, None, child_guard())
            .unwrap();

        topology
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index: 0 })
            .unwrap();

        assert!(topology.records[0].stopped_at_millis.is_some());
        assert!(topology.records[1].stopped_at_millis.is_none());
        assert!(topology.domains[0].supervisor.is_none());
        assert!(topology.domains[1].supervisor.is_some());
        assert_eq!(
            topology.faults,
            vec![OcompFaultRecordV1 {
                fault: OcompProcessFault::StopSupervisor { validator_index: 0 },
                applied_at_millis: topology.faults[0].applied_at_millis,
            }]
        );

        let snapshot = topology.evidence_snapshot().unwrap();
        assert_eq!(snapshot.processes, topology.records);
        assert_eq!(snapshot.faults, topology.faults);
        assert_eq!(snapshot.domain_roots.len(), 4);
        let canonical = serde_json::to_vec(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_slice::<OcompScenarioTopologyV1>(&canonical).unwrap(),
            snapshot
        );
    }
}
