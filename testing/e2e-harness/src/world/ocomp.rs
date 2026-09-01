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
use outbe_chain_constants::GENESIS_CONFIG_KEY;
#[cfg(feature = "ocomp-integration")]
use outbe_common::WorldwideDay;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::config::{
    OcompForkInstallClassification, OcompForkInstallV1, OcompRequestProfile,
};
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay};
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::{WwdDayType, WwdStatus};
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
    addresses::{METADOSIS_ADDRESS, ORACLE_ADDRESS, TRIBUTE_ADDRESS, VALIDATOR_SET_ADDRESS},
    signer::OutbeEvmSigner,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    OutbeHeader,
};
#[cfg(feature = "ocomp-integration")]
use std::fs::{self, File, OpenOptions};
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
/// Longer than the projection writer lease a killed exporter leaves behind.
#[cfg(feature = "ocomp-integration")]
const WRITER_LEASE_LAPSE: Duration = Duration::from_secs(7);
#[cfg(feature = "ocomp-integration")]
const EXPORTER_RESTART_ATTEMPTS: u8 = 3;
#[cfg(feature = "ocomp-integration")]
const OCOMP_BASE_PATH_ENV: &str = "OUTBE_OCOMP_BASE_PATH";
#[cfg(feature = "ocomp-integration")]
const OCOMP_VALIDATOR_INDEX_ENV: &str = "OCOMP_VALIDATOR_INDEX";
#[cfg(any(feature = "ocomp-integration", test))]
const OCOMP_MAX_PROCESS_RECORDS: usize = 64;
const OCOMP_MAX_FAULT_RECORDS: usize = 32;
#[cfg(feature = "ocomp-integration")]
const OCOMP_RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(30);
pub const METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX: &str =
    "0x06de88157b2c94c36b929a65c9db8d0f6a7ca10fad6d40be14098019f5749187";
#[cfg(feature = "ocomp-integration")]
pub const OCOMP_MEASUREMENT_ACTIVATION_HEIGHT: u64 =
    outbe_node::ocomp::fork::GENESIS_ACTIVE_OCOMP_HEIGHT;
/// Provisional block envelope used by the disposable OCM-25 measurement chain.
#[cfg(feature = "ocomp-integration")]
const OCOMP_MEASUREMENT_BLOCK_GAS_LIMIT: u64 = 40_000_000;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS: u64 = 600;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS: u64 = 3_600;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE: &str = "2";
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO: &str = "0";
#[cfg(feature = "ocomp-integration")]
const OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS: u64 = 180;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_TEST_EPOCH_LENGTH_BLOCKS: u64 = 300;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS: u64 = 10;
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS: u64 = OCOMP_TEST_EPOCH_LENGTH_BLOCKS * 3 / 2;

/// Fixed process roles represented in one validator's OCOMP domain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OcompProcessRole {
    SnapshotExporter,
    Worker,
}

/// Typed fault operations available to OCOMP scenarios.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OcompProcessFault {
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
    pub public_worldwide_day: Option<WorldwideDay>,
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
    snapshot_exporter: Option<OwnedProcess>,
    workers: BTreeMap<u32, OwnedProcess>,
}

impl OcompDomain {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
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
    #[cfg(feature = "ocomp-integration")]
    successor_identity: Option<OcompLaunchIdentityV1>,
    tribute_correlation: TributeCorrelationBuilder,
    correlated_tribute: Option<CorrelatedTributeFixtureV1>,
}

/// Exact external OCOMP process inventory quiesced around a node clock restart.
/// Embedded Supervisors remain node-owned; this plan records only the roles the
/// harness must recreate after every node has crossed the common finality gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OcompNodeFacingResumePlan {
    snapshot_exporters: Vec<u8>,
    workers: Vec<(u8, u32)>,
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
            #[cfg(feature = "ocomp-integration")]
            successor_identity: None,
            tribute_correlation,
            correlated_tribute: None,
        }
    }

    /// Stop every currently attached node-facing OCOMP process without
    /// recording a protocol fault, returning the exact inventory to restore.
    /// Deliberately absent workers therefore remain absent after the restart.
    pub(crate) fn suspend_node_facing_roles(&mut self) -> Result<OcompNodeFacingResumePlan> {
        let mut snapshot_exporters = Vec::new();
        let mut workers = Vec::new();

        for index in 0..self.domains.len() {
            let validator_index = u8::try_from(index)
                .map_err(|_| eyre::eyre!("validator index exceeds the harness wire format"))?;
            let (exporter, attached_workers) = {
                let domain = &mut self.domains[index];
                (
                    domain.snapshot_exporter.take(),
                    std::mem::take(&mut domain.workers),
                )
            };
            if let Some(exporter) = exporter {
                snapshot_exporters.push(validator_index);
                self.stop_owned(exporter);
            }
            for (worker_ordinal, worker) in attached_workers {
                workers.push((validator_index, worker_ordinal));
                self.stop_owned(worker);
            }
        }

        Ok(OcompNodeFacingResumePlan {
            snapshot_exporters,
            workers,
        })
    }

    /// Restore exactly one inventory returned by
    /// [`Self::suspend_node_facing_roles`] after node recovery.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn resume_node_facing_roles(
        &mut self,
        plan: OcompNodeFacingResumePlan,
    ) -> Result<()> {
        for validator_index in plan.snapshot_exporters {
            self.restart_snapshot_exporter(validator_index)?;
        }
        for (validator_index, worker_ordinal) in plan.workers {
            self.restart_worker(validator_index, worker_ordinal)?;
        }
        self.ensure_validator_roles_alive()
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
                    domain.snapshot_exporter.is_none() && domain.workers.is_empty(),
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
    /// The node owns the embedded Supervisor and its Worker endpoint, so this
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
        if let Some(identity) = self.launch_identity {
            publish_bundle_catalog_entry(
                &root,
                identity.protocol_bundle_hash,
                &fs::read(root.join("protocol-bundle-v1.ocb1"))?,
            )?;
        }
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
        let evm_key = format!("{}\n", ocomp_evm_private_key(validator_index));
        stage_joiner_domain_material_with_evm_key(&self.cfg, index, evm_key.as_bytes())
    }

    /// Scenario-owned root for one validator domain.
    pub fn domain_root(&self, validator_index: u8) -> Result<&Path> {
        Ok(&self.domain(validator_index)?.root)
    }

    /// Prove that release OCOMP roles use the scenario base directory and the
    /// canonical `validator-N/ocomp/domain-v1` layout. This is intentionally a
    /// path contract, not a service-UID contract: all harness roles run as the
    /// launching user.
    #[cfg(feature = "ocomp-integration")]
    pub fn verify_release_basedir_contract(&self) -> Result<()> {
        for validator_index in self.validator_indices()? {
            let expected = self
                .cfg
                .validator_dir(usize::from(validator_index))
                .join("ocomp")
                .join("domain-v1");
            let actual = self.domain_root(validator_index)?;
            eyre::ensure!(
                actual == expected,
                "validator-{validator_index} OCOMP root {} differs from basedir contract {}",
                actual.display(),
                expected.display()
            );
            for required in [
                "protocol-bundle-v1.ocb1",
                "ocomp-key-v1.hex",
                "ocomp-evm-key.hex",
            ] {
                eyre::ensure!(
                    actual.join(required).is_file(),
                    "validator-{validator_index} basedir is missing {required}"
                );
            }
        }
        Ok(())
    }

    /// Canonical chain manifest selected for this scenario before any
    /// mismatched-install fault is injected.
    #[cfg(feature = "ocomp-integration")]
    #[must_use]
    pub fn canonical_chain_manifest_path(&self) -> PathBuf {
        self.cfg.dir.join("genesis.json")
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn canonical_fork_install(&self) -> Result<OcompForkInstallV1> {
        let spec = parse_outbe_chain_spec(&self.canonical_chain_manifest_path())?;
        Ok(
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&spec)?
                .as_ref()
                .clone(),
        )
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
            None,
        )
    }

    /// Seed the bounded TRY/EUR Oracle state used by the first Tribute E2E.
    ///
    /// This mutates only the not-yet-started scenario genesis. The production
    /// path still reads the canonical pair registry, WWD snapshot and EUR
    /// S-curve through the ordinary Tribute host/enclave interfaces.
    pub fn prepare_cross_currency_tribute_fixture(&self) -> Result<u32> {
        #[cfg(not(feature = "ocomp-integration"))]
        {
            eyre::bail!("cross-currency Tribute fixture requires ocomp-integration")
        }

        #[cfg(feature = "ocomp-integration")]
        {
            let genesis_path = self.cfg.dir.join("genesis.json");
            let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
            let chain_id = genesis_chain_id(&genesis)?;
            let configured_offering_secs =
                outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(&genesis)?
                    .metadosis_offering_period_seconds;
            let (_, worldwide_day) = schedule_public_measurement_day(
                &mut genesis,
                chain_id,
                OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS.max(configured_offering_secs),
            )?;
            let oracle_key = {
                let alloc = genesis
                    .get("alloc")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
                find_alloc_address_key(alloc, ORACLE_ADDRESS)?
                    .ok_or_else(|| eyre::eyre!("generated genesis has no Oracle account"))?
            };
            let mut provider = HashMapStorageProvider::new(chain_id);
            for (slot, value) in genesis["alloc"][&oracle_key]["storage"]
                .as_object()
                .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?
            {
                provider.storage.insert(
                    (ORACLE_ADDRESS, parse_hex_word(slot)?),
                    parse_storage_word(value)?,
                );
            }

            StorageHandle::enter(&mut provider, |storage| {
                let issuance_pair = outbe_oracle::types::AddressPair::new_coen_to(949);
                let reference_pair = outbe_oracle::types::AddressPair::new_coen_to(978);
                let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                let issuance_index = match oracle.pair_index_of(issuance_pair)? {
                    0 => outbe_oracle::api::register_pair(storage.clone(), issuance_pair)?,
                    index => index,
                };
                let reference_index = match oracle.pair_index_of(reference_pair)? {
                    0 => outbe_oracle::api::register_pair(storage.clone(), reference_pair)?,
                    index => index,
                };
                oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                if !oracle.reference_currencies.read_all()?.contains(&978) {
                    return Err(outbe_primitives::error::PrecompileError::Fatal(
                        "cross-currency fixture requires EUR in the reference registry".into(),
                    ));
                }
                oracle
                    .worldwide_day_vwap_exists
                    .write(&worldwide_day, true)?;
                oracle
                    .worldwide_day_vwap_start
                    .write(&worldwide_day, worldwide_day.start_timestamp())?;
                oracle.worldwide_day_vwap_end.write(
                    &worldwide_day,
                    worldwide_day.start_timestamp()
                        + outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
                )?;
                let values = oracle.worldwide_day_vwap_value.get_nested(&worldwide_day);
                values.write(&issuance_index, U256::from(10_250_000_u64))?;
                values.write(&reference_index, U256::from(250_000_u64))?;
                outbe_oracle::scurve::store_scurve_entry(
                    &mut oracle,
                    reference_pair,
                    worldwide_day.to_timestamp_utc(),
                    U256::from(320_000_u64),
                )?;
                let inputs =
                    outbe_oracle::api::tribute_pricing_inputs(storage, 949, 978, worldwide_day)?
                        .ok_or_else(|| {
                            outbe_primitives::error::PrecompileError::Fatal(
                                "cross-currency issuance pair was not registered".into(),
                            )
                        })?;
                if inputs.issuance_wwd_vwap_minor != U256::from(10_250_000_u64)
                    || inputs.reference_wwd_vwap_minor != U256::from(250_000_u64)
                    || inputs.reference_scurve_minor != U256::from(320_000_u64)
                {
                    return Err(outbe_primitives::error::PrecompileError::Fatal(
                        "cross-currency fixture did not persist its exact Oracle inputs".into(),
                    ));
                }
                Ok(())
            })?;

            let words = genesis["alloc"][&oracle_key]["storage"]
                .as_object_mut()
                .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?;
            words.clear();
            for ((address, slot), value) in &provider.storage {
                if *address == ORACLE_ADDRESS && !value.is_zero() {
                    words.insert(
                        format!("0x{slot:064x}"),
                        serde_json::Value::String(format!("0x{value:064x}")),
                    );
                }
            }
            replace_json_atomically(&genesis_path, &genesis)?;
            Ok(worldwide_day.value())
        }
    }

    /// Prepare two independently scheduled public jobs around one real DKG
    /// membership boundary. The shortened epoch is still above the normative
    /// snapshot-retention lower bound; the compute-and-vote deadline comes from
    /// the test-only genesis override selected by the E2E node build.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_dynamic_membership_fork_install(&self) -> Result<OcompDynamicMembershipForkV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
        let chain_id = genesis_chain_id(&genesis)?;
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
                    == Some(OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS)
                && config
                    .get(GENESIS_CONFIG_KEY)
                    .and_then(|value| value.pointer("/ocomp/computeVoteWindowBlocks"))
                    .and_then(serde_json::Value::as_u64)
                    == Some(OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS),
            "dynamic OCOMP epoch, DKG and vote windows must be configured before ValidatorSet genesis is seeded"
        );
        schedule_public_measurement_day(
            &mut genesis,
            chain_id,
            OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS,
        )?;
        let schedule = schedule_dynamic_membership_days(&mut genesis, chain_id)?;
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

    /// Prove child-process liveness and mutual Worker/embedded-Supervisor
    /// registration for the complete baseline runtime.
    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_baseline_runtime_ready(
        &mut self,
        expected_workers_per_supervisor: usize,
    ) -> Result<OcompRuntimeCountsV1> {
        let deadline = Instant::now() + OCOMP_RUNTIME_READY_TIMEOUT;
        loop {
            self.ensure_baseline_processes_alive(expected_workers_per_supervisor)?;
            match self.observe_baseline_runtime(expected_workers_per_supervisor) {
                Ok(counts) => return Ok(counts),
                Err(error) if Instant::now() >= deadline => {
                    eyre::bail!(
                        "OCOMP Supervisors/Workers did not become ready within {} seconds: {error}",
                        OCOMP_RUNTIME_READY_TIMEOUT.as_secs()
                    );
                }
                Err(_) => sleep(Duration::from_millis(250)),
            }
        }
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
            let address = SocketAddr::from(([127, 0, 0, 1], self.cfg.ocomp_endpoint_port(index)));
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
    pub fn ocomp_delegate_private_key(&self, validator_index: u8) -> Result<String> {
        self.domain(validator_index)?;
        Ok(format!("0x{}", ocomp_evm_private_key(validator_index)))
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_delegate_private_key_for_vote(&self, vote: &ResultVoteV1) -> Result<String> {
        for validator_index in self.validator_indices()? {
            let encoded =
                fs::read_to_string(self.domain_root(validator_index)?.join("ocomp-key-v1.hex"))?;
            let signing_key = SigningKey::from_slice(&hex::decode(encoded.trim())?)?;
            let public_key = signing_key.verifying_key().to_encoded_point(true);
            if keccak256(public_key.as_bytes()) == vote.ocomp_key_hash {
                return self.ocomp_delegate_private_key(validator_index);
            }
        }
        Err(eyre::eyre!(
            "ResultVoteV1 OCOMP key is not owned by this pinned test topology"
        ))
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
            exporter,
        )?;
        let worker = self.spawn_worker_process(
            validator_index,
            0,
            0,
            self.keyless_full_node_domain(validator_index)?.root.clone(),
            identity,
        )?;
        self.attach_keyless_full_node_owned(validator_index, OcompProcessRole::Worker, worker)?;
        sleep(Duration::from_secs(2));
        self.ensure_keyless_full_node_roles_alive(validator_index)
    }

    /// Stop only the compute clients; the synchronized FullNode process and
    /// durable domain remain intact for validator-mode promotion.
    #[cfg(feature = "ocomp-integration")]
    pub fn stop_keyless_full_node_roles(&mut self, validator_index: u8) -> Result<()> {
        let (exporter, workers) = {
            let domain = self.keyless_full_node_domain_mut(validator_index)?;
            (
                domain.snapshot_exporter.take(),
                std::mem::take(&mut domain.workers),
            )
        };
        let exporter =
            exporter.ok_or_else(|| eyre::eyre!("FullNode snapshot exporter is not running"))?;
        self.stop_owned(exporter);
        for (_, worker) in workers {
            self.stop_owned(worker);
        }
        Ok(())
    }

    /// Arm the test-only local-result mutation for one keyless FullNode job.
    /// The production binary claims and binds this empty marker to the first
    /// observed JobId; the harness never supplies a digest or result payload.
    #[cfg(feature = "ocomp-integration")]
    pub fn arm_keyless_full_node_result_mismatch(&self, validator_index: u8) -> Result<PathBuf> {
        let root = self
            .keyless_full_node_domain(validator_index)?
            .root
            .join("test-faults");
        fs::create_dir_all(&root)?;
        let marker = root.join("local-result-mismatch.once");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&marker)?;
        file.sync_all()?;
        File::open(&root)?.sync_all()?;
        Ok(marker)
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

    /// Starts one Worker on the successor bundle lane in every validator
    /// domain. Worker ordinal 1 is process-local; the lane has its own
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
        self.successor_identity = Some(successor_identity);
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ensure_successor_workers_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + OCOMP_RUNTIME_READY_TIMEOUT;
        loop {
            let mut ready = true;
            for validator_index in self.validator_indices()? {
                self.ensure_worker_alive(validator_index, 1)?;
                let base = self.cfg.ocomp_endpoint_port(usize::from(validator_index));
                let successor_port = base
                    .checked_add(6)
                    .ok_or_else(|| eyre::eyre!("OCOMP successor lane endpoint port overflow"))?;
                match fetch_supervisor_status(SocketAddr::from(([127, 0, 0, 1], successor_port)))
                    .and_then(|status| ensure_supervisor_status_ready(validator_index, &status, 1))
                {
                    Ok(()) => {}
                    Err(_) => ready = false,
                }
            }
            if ready {
                return Ok(());
            }
            eyre::ensure!(
                Instant::now() < deadline,
                "OCOMP successor Workers did not register before the readiness deadline"
            );
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    fn spawn_worker_process(
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
    fn spawn_validator_role(
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

        let mut command = self.release_role_command(validator_index);
        command
            .arg(role_name)
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
        let mut command = self.release_role_command(validator_index);
        command
            .arg(role_name)
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
            OcompProcessRole::SnapshotExporter if domain.snapshot_exporter.is_none() => {}
            OcompProcessRole::Worker if !domain.workers.contains_key(&0) => {}
            _ => {
                eyre::bail!("invalid or duplicate keyless FullNode role attachment");
            }
        }
        let record_index = self.records.len();
        self.records.push(OcompProcessRecordV1 {
            validator_index: Some(validator_index),
            role,
            worker_ordinal: (role == OcompProcessRole::Worker).then_some(0),
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
            OcompProcessRole::SnapshotExporter => domain.snapshot_exporter = Some(process),
            OcompProcessRole::Worker => {
                domain.workers.insert(0, process);
            }
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn ensure_keyless_full_node_roles_alive(&mut self, validator_index: u8) -> Result<()> {
        for role in [OcompProcessRole::SnapshotExporter, OcompProcessRole::Worker] {
            let (record_index, exited) = {
                let domain = self.keyless_full_node_domain_mut(validator_index)?;
                let process = match role {
                    OcompProcessRole::SnapshotExporter => domain.snapshot_exporter.as_mut(),
                    OcompProcessRole::Worker => domain.workers.get_mut(&0),
                }
                .ok_or_else(|| eyre::eyre!("FullNode OCOMP {role:?} is missing"))?;
                (process.record_index, process.guard.exited())
            };
            if exited {
                self.records[record_index].stopped_at_millis = Some(unix_time_millis());
                let role_name = match role {
                    OcompProcessRole::SnapshotExporter => "snapshot-exporter",
                    OcompProcessRole::Worker => "worker-0",
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
            for role in [OcompProcessRole::SnapshotExporter] {
                let intentionally_stopped = self.faults.iter().any(|record| {
                    matches!(
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
                        OcompProcessRole::SnapshotExporter => domain.snapshot_exporter.as_mut(),
                        _ => unreachable!("fixed exporter iteration above"),
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
                        OcompProcessRole::SnapshotExporter => "snapshot-exporter",
                        _ => unreachable!("fixed exporter iteration above"),
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

    /// Restart one external Worker after a typed stop. The embedded Supervisor
    /// remains owned by the node throughout the fault.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_worker(&mut self, validator_index: u8, worker_ordinal: u32) -> Result<()> {
        if self
            .domain(validator_index)?
            .workers
            .contains_key(&worker_ordinal)
        {
            eyre::bail!("validator-{validator_index} worker-{worker_ordinal} is already running");
        }
        let initial_identity = self
            .launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let (bundle_lane, identity) = if worker_ordinal == 1 {
            self.successor_identity
                .map_or((0, initial_identity), |identity| (1, identity))
        } else {
            (0, initial_identity)
        };
        let domain_root = self.domain(validator_index)?.root.clone();
        let guard = self.spawn_worker_process(
            validator_index,
            worker_ordinal,
            bundle_lane,
            domain_root,
            identity,
        )?;
        self.attach_owned(
            Some(validator_index),
            OcompProcessRole::Worker,
            Some(worker_ordinal),
            guard,
        )?;
        sleep(Duration::from_secs(2));
        self.ensure_worker_alive(validator_index, worker_ordinal)
    }

    /// Restart the fixed SnapshotExporter role in one domain after a typed stop.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_snapshot_exporter(&mut self, validator_index: u8) -> Result<()> {
        self.restart_snapshot_exporter_inner(validator_index, EXPORTER_RESTART_ATTEMPTS)
    }

    #[cfg(feature = "ocomp-integration")]
    fn restart_snapshot_exporter_inner(
        &mut self,
        validator_index: u8,
        attempts_left: u8,
    ) -> Result<()> {
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
        if exited && attempts_left > 0 {
            // The killed predecessor keeps its projection writer lease until the
            // lease lapses, so an immediate successor loses the race with it.
            self.records[record_index].stopped_at_millis = Some(unix_time_millis());
            if let Some(process) = self.domain_mut(validator_index)?.snapshot_exporter.take() {
                self.stop_owned(process);
            }
            sleep(WRITER_LEASE_LAPSE);
            return self.restart_snapshot_exporter_inner(validator_index, attempts_left - 1);
        }
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

    /// Recreate the durable state left by a crash after `prepared.ref` but
    /// before `receipt.ref`, then prove the production SnapshotExporter restores
    /// the exact receipt reference on restart. No chain state or result is
    /// injected: only the exporter's local terminal marker is removed.
    #[cfg(feature = "ocomp-integration")]
    pub fn verify_prepared_only_exporter_restart(
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
        fs::remove_file(&receipt_path)?;
        self.restart_snapshot_exporter(validator_index)?;

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.ensure_validator_roles_alive()?;
            if let Ok(receipt_after) = fs::read(&receipt_path) {
                eyre::ensure!(
                    receipt_after == receipt_before,
                    "prepared-only restart changed the committed export reference"
                );
                eyre::ensure!(
                    fs::read(&prepared_path)? == prepared_before,
                    "prepared-only restart changed the preparation reference"
                );
                return Ok(());
            }
            eyre::ensure!(
                Instant::now() < deadline,
                "SnapshotExporter did not restore receipt.ref after prepared-only restart"
            );
            sleep(Duration::from_millis(100));
        }
    }

    /// Restart every external compute client while preserving the domain data.
    /// The embedded Supervisor restarts with the node and is not a harness-owned
    /// process.
    #[cfg(feature = "ocomp-integration")]
    pub fn restart_node_facing_processes(&mut self, validator_index: u8) -> Result<()> {
        let (exporter, workers) = {
            let domain = self.domain_mut(validator_index)?;
            (
                domain.snapshot_exporter.take(),
                std::mem::take(&mut domain.workers),
            )
        };
        if let Some(process) = exporter {
            self.stop_owned(process);
        }
        let worker_ordinals = workers.keys().copied().collect::<Vec<_>>();
        for (_, process) in workers {
            self.stop_owned(process);
        }
        self.restart_snapshot_exporter(validator_index)?;
        for worker_ordinal in worker_ordinals {
            self.restart_worker(validator_index, worker_ordinal)?;
        }
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

/// Stage the node-owned OCOMP domain for a joiner that will start directly in
/// Validator mode. Without an explicit OCOMP delegate, the validator EVM key
/// is the canonical carrier authority.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn stage_direct_joiner_domain_material(cfg: &Config, index: usize) -> Result<()> {
    let prefixed = crate::internal::proc::read_evm_key(&cfg.validator_dir(index))?;
    let raw = prefixed
        .strip_prefix("0x")
        .ok_or_else(|| eyre::eyre!("validator EVM key is missing its canonical prefix"))?;
    eyre::ensure!(
        raw.len() == 64
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "validator EVM key must be exactly 32 lowercase hex bytes"
    );
    let evm_key = format!("{raw}\n");
    stage_joiner_domain_material_with_evm_key(cfg, index, evm_key.as_bytes())
}

#[cfg(feature = "ocomp-integration")]
fn stage_joiner_domain_material_with_evm_key(
    cfg: &Config,
    index: usize,
    evm_key: &[u8],
) -> Result<()> {
    let founder = cfg.validator_dir(0).join("ocomp").join("domain-v1");
    let bundle = fs::read(founder.join("protocol-bundle-v1.ocb1"))?;
    let signing_key = fs::read(cfg.validator_dir(index).join("ocomp-key-v1.hex"))?;
    let root = cfg.validator_dir(index).join("ocomp").join("domain-v1");
    fs::create_dir_all(&root)?;
    publish_exact_file(&root.join("protocol-bundle-v1.ocb1"), &bundle, 0o640)?;
    publish_exact_file(&root.join("ocomp-key-v1.hex"), &signing_key, 0o600)?;
    publish_exact_file(&root.join("ocomp-evm-key.hex"), evm_key, 0o600)?;
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn configure_release_layout(command: &mut Command, base_path: &Path, validator_index: u8) {
    command
        .env(OCOMP_BASE_PATH_ENV, base_path)
        .env(OCOMP_VALIDATOR_INDEX_ENV, validator_index.to_string());
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
        }
        for domain in &mut self.domains {
            domain.workers.clear();
            domain.snapshot_exporter.take();
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
    // Job B is deliberately released by the scenario's controlled-time jump,
    // after the certified five-validator activation. A relative `+700s`
    // deadline raced the height-300 activation when SGX/admission work delayed
    // blocks, allowing the job to pin the historical four-member snapshot.
    let second_processing_time = genesis_timestamp
        .checked_div(SECONDS_PER_DAY)
        .and_then(|day| day.checked_add(2))
        .and_then(|day| day.checked_mul(SECONDS_PER_DAY))
        .and_then(|midnight| midnight.checked_add(1))
        .ok_or_else(|| eyre::eyre!("second dynamic OCOMP processing time overflow"))?;
    let first_worldwide_day = WorldwideDay::from_timestamp(genesis_timestamp);
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
        let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Oracle account"))?;
        let oracle_words = alloc
            .get(&oracle_key)
            .and_then(|account| account.get("storage"))
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?;
        for (slot, value) in oracle_words {
            provider.storage.insert(
                (ORACLE_ADDRESS, parse_hex_word(slot)?),
                parse_storage_word(value)?,
            );
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
        outbe_tribute::TributeContract::new(storage.clone()).unseal_day(second_worldwide_day)?;

        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let price = outbe_oracle::api::day_type_pair_vwap(storage.clone(), first_worldwide_day)?
            .filter(|price| !price.is_zero())
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "dynamic OCOMP genesis is missing its seeded Oracle VWAP".into(),
                )
            })?;
        let snapshot_time = second_worldwide_day.start_timestamp();
        let volume = U256::from(1_000_000_u64);
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
        // This scenario advances canonical time by one hour per finalized block.
        // Keep the real feeder's tally cadence below the production six-hour TTL;
        // the ordinary genesis cadence would publish only every eight hours here.
        oracle.config_vote_period.write(2)?;
        oracle.write_snapshot(snapshot_time, &[(pair, price, volume)])?;
        let pair_index = oracle.pair_index_of(pair)?;
        if pair_index == 0 {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "dynamic OCOMP second-day Oracle pair is not registered".into(),
            ));
        }
        let snapshot_end = snapshot_time
            .checked_add(outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS)
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "dynamic OCOMP second-day WWD window overflow".into(),
                )
            })?;
        oracle
            .worldwide_day_vwap_exists
            .write(&second_worldwide_day, true)?;
        oracle
            .worldwide_day_vwap_start
            .write(&second_worldwide_day, snapshot_time)?;
        oracle
            .worldwide_day_vwap_end
            .write(&second_worldwide_day, snapshot_end)?;
        oracle
            .worldwide_day_vwap_value
            .get_nested(&second_worldwide_day)
            .write(&pair_index, price)?;
        Ok(())
    })?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    for (address, label) in [
        (METADOSIS_ADDRESS, "Metadosis"),
        (ORACLE_ADDRESS, "Oracle"),
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
) -> Result<(bool, WorldwideDay)> {
    eyre::ensure!(
        offering_after_genesis_secs > 0,
        "OCOMP public measurement offering duration must be non-zero"
    );
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let worldwide_day = WorldwideDay::from_timestamp(genesis_timestamp);
    let forming_start = worldwide_day.start_timestamp();
    eyre::ensure!(
        forming_start < genesis_timestamp,
        "OCOMP public measurement genesis timestamp must follow WorldwideDay start"
    );
    let offering_end = genesis_timestamp
        .checked_add(offering_after_genesis_secs)
        .ok_or_else(|| eyre::eyre!("OCOMP public measurement offering end overflow"))?;

    let mut provider = HashMapStorageProvider::new(chain_id);
    let account_keys = {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        let mut keys = Vec::with_capacity(3);
        for (address, label) in [
            (METADOSIS_ADDRESS, "Metadosis"),
            (ORACLE_ADDRESS, "Oracle"),
            (TRIBUTE_ADDRESS, "Tribute"),
        ] {
            let account_key = find_alloc_address_key(alloc, address)?
                .ok_or_else(|| eyre::eyre!("generated genesis has no {label} account"))?;
            let account = alloc
                .get(&account_key)
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| eyre::eyre!("{label} genesis account is not an object"))?;
            if let Some(storage) = account.get("storage") {
                let words = storage.as_object().ok_or_else(|| {
                    eyre::eyre!("{label} genesis account storage is not an object")
                })?;
                for (slot, value) in words {
                    provider
                        .storage
                        .insert((address, parse_hex_word(slot)?), parse_storage_word(value)?);
                }
            }
            keys.push((address, account_key));
        }
        keys
    };

    provider.set_block_number(1);
    let changed = StorageHandle::enter(&mut provider, |storage| {
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let seeded_rate = outbe_oracle::api::get_exchange_rate(
            storage.clone(),
            pair.address1(),
            pair.address2(),
        )?;
        if seeded_rate.is_zero() {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement Oracle seed has zero COEN/0xUSD rate".into(),
            ));
        }
        // Keep this fixture's ordinary mineGratis path independent from the
        // stablecoin/vault scenarios. Together with the six-atto Tribute
        // amount, these prices produce non-zero Gratis and zero mining cost.
        let previous_vwap = U256::ONE;
        let current_vwap = U256::from(2);
        let previous_day = worldwide_day.previous_date_key();
        let previous_sample_time = forming_start.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement previous VWAP window underflow".into(),
            )
        })?;
        let volume = U256::from(1_000_000_u64);
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let inherited_scurve_expiry = genesis_timestamp
            .checked_add(
                (outbe_oracle::scurve::PERIOD as u64 + 1) * outbe_oracle::scurve::DAY_SECONDS,
            )
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "OCOMP public measurement S-curve expiry overflow".into(),
                )
            })?;
        outbe_oracle::scurve::evict_expired_scurves(&mut oracle, inherited_scurve_expiry)?;
        oracle.write_snapshot(previous_sample_time, &[(pair, previous_vwap, volume)])?;
        oracle.write_snapshot(forming_start, &[(pair, current_vwap, volume)])?;
        let pair_index = oracle.pair_index_of(pair)?;
        if pair_index == 0 {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement Oracle pair is not registered".into(),
            ));
        }
        for (day, vwap) in [(previous_day, previous_vwap), (worldwide_day, current_vwap)] {
            let start = day.start_timestamp();
            let end = start
                .checked_add(outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::Fatal(
                        "OCOMP public measurement WWD window overflow".into(),
                    )
                })?;
            oracle.worldwide_day_vwap_exists.write(&day, true)?;
            oracle.worldwide_day_vwap_start.write(&day, start)?;
            oracle.worldwide_day_vwap_end.write(&day, end)?;
            oracle
                .worldwide_day_vwap_value
                .get_nested(&day)
                .write(&pair_index, vwap)?;
        }
        let stored_previous = outbe_oracle::api::day_type_pair_vwap(storage.clone(), previous_day)?
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "OCOMP public measurement previous VWAP is missing".into(),
                )
            })?;
        let stored_current = outbe_oracle::api::day_type_pair_vwap(storage.clone(), worldwide_day)?
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "OCOMP public measurement current VWAP is missing".into(),
                )
            })?;
        let day_type = if stored_current > stored_previous {
            WwdDayType::Green
        } else {
            WwdDayType::Red
        };
        let entry_price = stored_current.max(outbe_oracle::api::get_max_active_scurve_value(
            storage.clone(),
            worldwide_day,
            pair,
        )?);
        let qualification_rate = entry_price.checked_mul(U256::from(2)).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement qualification rate overflow".into(),
            )
        })?;
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            pair,
            qualification_rate,
            1,
            genesis_timestamp,
        )?;
        let day_limit = U256::from(500) * outbe_primitives::units::SCALE_1E6_U256;
        let report = FreshDevnetGenesisBuilder::new()
            .seed_active_worldwide_day(GenesisWorldwideDay {
                worldwide_day,
                status: WwdStatus::Offering,
                day_type,
                forming_start,
                forming_end: genesis_timestamp,
                lookback_end: genesis_timestamp,
                offering_end,
                scheduled_process_time: offering_end,
                metadosis_limit_amount: day_limit,
                previous_vwap: stored_previous,
                current_vwap: stored_current,
            })
            .apply(storage.clone())?;
        outbe_tribute::TributeContract::new(storage).unseal_day(worldwide_day)?;
        Ok(report.changed)
    })?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    for (address, account_key) in account_keys {
        let account = alloc
            .get_mut(&account_key)
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("genesis account {address:#x} is not an object"))?;
        let words = account
            .entry("storage".to_owned())
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| eyre::eyre!("genesis account {address:#x} storage is not an object"))?;
        words.clear();
        for ((stored_address, slot), value) in &provider.storage {
            if *stored_address == address && !value.is_zero() {
                words.insert(
                    format!("0x{slot:064x}"),
                    serde_json::Value::String(format!("0x{value:064x}")),
                );
            }
        }
    }
    Ok((changed, worldwide_day))
}

#[cfg(feature = "ocomp-integration")]
fn clear_seeded_metadosis_days(genesis: &mut serde_json::Value, chain_id: u64) -> Result<bool> {
    let mut provider = HashMapStorageProvider::new(chain_id);
    {
        let alloc = genesis
            .get_mut("alloc")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
        let words = alloc
            .get_mut(&metadosis_key)
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account is not an object"))?
            .entry("storage".to_owned())
            .or_insert_with(|| serde_json::json!({}))
            .as_object()
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account storage is not an object"))?;
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

/// Seed only raw Oracle evidence for the runtime-created fresh WWD.
///
/// Metadosis remains pristine: block 1 creates the day and the production
/// ResolveForming edge computes and stores the exact 50-hour VWAP. The harness
/// supplies one ordinary DAY_TYPE_PAIR observation inside that interval so the
/// subsequent public Tribute has a real canonical price instead of relying on a
/// pre-materialized WWD snapshot.
#[cfg(feature = "ocomp-integration")]
fn seed_fresh_metadosis_oracle_input(
    genesis: &mut serde_json::Value,
    chain_id: u64,
) -> Result<bool> {
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let worldwide_day = WorldwideDay::from_timestamp(genesis_timestamp);
    let oracle_key = {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        find_alloc_address_key(alloc, ORACLE_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Oracle account"))?
    };
    let mut provider = HashMapStorageProvider::new(chain_id);
    for (slot, value) in genesis["alloc"][&oracle_key]["storage"]
        .as_object()
        .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?
    {
        provider.storage.insert(
            (ORACLE_ADDRESS, parse_hex_word(slot)?),
            parse_storage_word(value)?,
        );
    }
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let current_vwap = U256::from(2);
        let volume = U256::from(1_000_000_u64);
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        // The scenario advances logical time by one hour per finalized block.
        // A two-block test-genesis period keeps real feeder publications inside
        // the production six-hour freshness bound without changing defaults.
        oracle.config_vote_period.write(2)?;
        if oracle.pair_index_of(pair)? == 0 {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "fresh Metadosis Oracle pair is not registered".into(),
            ));
        }
        oracle.write_snapshot(
            worldwide_day.start_timestamp(),
            &[(pair, current_vwap, volume)],
        )?;
        let scurve = outbe_oracle::api::get_max_active_scurve_value(storage, worldwide_day, pair)?;
        if scurve <= current_vwap {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "fresh Metadosis fixture requires an S-curve above its WWD VWAP".into(),
            ));
        }
        Ok(())
    })?;

    let words = genesis["alloc"][&oracle_key]["storage"]
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?;
    words.clear();
    for ((address, slot), value) in &provider.storage {
        if *address == ORACLE_ADDRESS && !value.is_zero() {
            words.insert(
                format!("0x{slot:064x}"),
                serde_json::Value::String(format!("0x{value:064x}")),
            );
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
    result_deadline_blocks: u64,
    max_terminal_job_records: Option<u16>,
) -> Result<OcompForkInstallV1> {
    let protocol_bundle = provisional_measurement_bundle();
    let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(limits)?;
    let founder_registrations =
        measurement_founder_registrations(validators_path, chain_id, genesis_hash, limits)?;
    let mut capacity_profile = provisional_measurement_capacity_profile(result_deadline_blocks);
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
fn provisional_measurement_capacity_profile(result_deadline_blocks: u64) -> CapacityProfileV1 {
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
        result_deadline_blocks,
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
fn publish_bundle_catalog_entry(root: &Path, bundle_hash: B256, bytes: &[u8]) -> Result<()> {
    let catalog = root.join("protocol-bundles-v1");
    fs::create_dir_all(&catalog)?;
    publish_exact_file(
        &catalog.join(format!("{}.ocb1", hex::encode(bundle_hash.as_slice()))),
        bytes,
        0o640,
    )
}

#[cfg(feature = "ocomp-integration")]
fn installed_protocol_bundle_hashes(root: &Path, initial_hash: B256) -> Result<String> {
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
    use outbe_chain_constants::GENESIS_CONFIG_KEY;
    #[cfg(feature = "ocomp-integration")]
    use outbe_metadosis::{WwdDayType, WwdStatus};

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

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn release_ocomp_layout_uses_the_scenario_base_and_never_the_debug_override() {
        let mut command = Command::new("outbe-ocomp");
        command.arg("snapshot-exporter");
        configure_release_layout(&mut command, Path::new("/tmp/release-e2e"), 7);

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["snapshot-exporter"]);
        assert!(!args.iter().any(|arg| arg == "--development-root"));

        let environment = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            environment.get(OCOMP_BASE_PATH_ENV),
            Some(&Some("/tmp/release-e2e".to_owned()))
        );
        assert_eq!(
            environment.get(OCOMP_VALIDATOR_INDEX_ENV),
            Some(&Some("7".to_owned()))
        );
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
    fn completed_job_topology() -> TestTopology {
        let mut topology = topology();
        topology.launch_identity = Some(OcompLaunchIdentityV1 {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x09),
            protocol_bundle_hash: B256::repeat_byte(0x08),
            fork_install_hash: B256::repeat_byte(0x07),
            classification: OcompForkInstallClassification::Final,
            activation_height: 1,
            metadosis_storage_layout_hash: METADOSIS_STORAGE_LAYOUT_V1_HASH,
        });
        topology
    }

    #[cfg(feature = "ocomp-integration")]
    fn stage_completed_job_footprint(topology: &OcompTopology, job_id: B256) {
        let job_component = hex::encode(job_id);
        let bundle_hash = topology
            .launch_identity
            .expect("completed-job fixture has a launch identity")
            .protocol_bundle_hash;
        for validator_index in topology.validator_indices().unwrap() {
            let root = topology.domain_root(validator_index).unwrap();
            let admissions = root
                .join("supervisor-v1")
                .join("jobs")
                .join(&job_component)
                .join("admissions");
            let worker_outputs = root
                .join("worker-inbox-v1")
                .join(hex::encode(bundle_hash))
                .join("artifacts");
            let votes = root
                .join("supervisor-v1")
                .join("vote-submissions")
                .join(&job_component);
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
        let topology = completed_job_topology();
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
    fn direct_joiner_domain_uses_validator_evm_fallback_without_a_delegate() {
        let topology = topology_with_validators(4);
        let founder_bundle = topology
            .domain_root(0)
            .unwrap()
            .join("protocol-bundle-v1.ocb1");
        fs::create_dir_all(founder_bundle.parent().unwrap()).unwrap();
        fs::write(&founder_bundle, b"pinned-bundle").unwrap();
        let joiner = topology.cfg.validator_dir(4);
        fs::create_dir_all(&joiner).unwrap();
        fs::write(
            joiner.join("ocomp-key-v1.hex"),
            b"joiner-registration-secret\n",
        )
        .unwrap();
        let validator_evm_key = format!("0x{}\n", "ab".repeat(32));
        fs::write(joiner.join("evm-key.hex"), validator_evm_key).unwrap();

        stage_direct_joiner_domain_material(&topology.cfg, 4).unwrap();

        let staged = joiner.join("ocomp").join("domain-v1");
        assert_eq!(
            fs::read(staged.join("protocol-bundle-v1.ocb1")).unwrap(),
            b"pinned-bundle"
        );
        assert_eq!(
            fs::read(staged.join("ocomp-key-v1.hex")).unwrap(),
            b"joiner-registration-secret\n"
        );
        assert_eq!(
            fs::read(staged.join("ocomp-evm-key.hex")).unwrap(),
            format!("{}\n", "ab".repeat(32)).as_bytes()
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn completed_job_artifacts_reject_one_domain_with_different_worker_output() {
        let topology = completed_job_topology();
        let job_id = B256::repeat_byte(0x43);
        stage_completed_job_footprint(&topology, job_id);
        let bundle_hash = topology
            .launch_identity
            .expect("completed-job fixture has a launch identity")
            .protocol_bundle_hash;
        let changed = topology
            .domain_root(3)
            .unwrap()
            .join("worker-inbox-v1")
            .join(hex::encode(bundle_hash))
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

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn keyless_full_node_mismatch_marker_is_empty_and_one_shot() {
        let mut topology = topology_with_validators(4);
        prepare_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_measurement_fork_install().unwrap();
        topology.launch_identity = Some(prepared.launch_identity());
        topology.stage_keyless_full_node_domain(4).unwrap();

        let marker = topology.arm_keyless_full_node_result_mismatch(4).unwrap();
        assert_eq!(fs::read(&marker).unwrap(), Vec::<u8>::new());
        assert!(topology.arm_keyless_full_node_result_mismatch(4).is_err());
        assert_eq!(
            topology.keyless_full_node_fatal_evidence_root(4).unwrap(),
            topology
                .cfg
                .validator_dir(4)
                .join("ocomp/domain-v1/node-v1/fatal-evidence")
        );
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
    fn cross_currency_tribute_fixture_persists_exact_oracle_inputs() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let worldwide_day =
            WorldwideDay::new(topology.prepare_cross_currency_tribute_fixture().unwrap());
        let genesis: serde_json::Value =
            serde_json::from_slice(&fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        let alloc = genesis["alloc"].as_object().unwrap();
        let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (slot, value) in alloc[&oracle_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (ORACLE_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(
                outbe_oracle::api::tribute_pricing_inputs(storage, 949, 978, worldwide_day)
                    .unwrap()
                    .unwrap(),
                outbe_oracle::api::TributePricingInputs {
                    issuance_wwd_vwap_minor: U256::from(10_250_000_u64),
                    reference_wwd_vwap_minor: U256::from(250_000_u64),
                    reference_scurve_minor: U256::from(320_000_u64),
                }
            );
        });
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn cross_currency_tribute_fixture_preserves_a_longer_configured_offering() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(&fs::read(&genesis_path).unwrap()).unwrap();
        genesis["config"][GENESIS_CONFIG_KEY]["metadosis"]["offeringPeriodSeconds"] =
            serde_json::json!(1_800);
        replace_json_atomically(&genesis_path, &genesis).unwrap();

        topology.prepare_cross_currency_tribute_fixture().unwrap();

        let genesis: serde_json::Value =
            serde_json::from_slice(&fs::read(&genesis_path).unwrap()).unwrap();
        let genesis_timestamp =
            u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
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
            let days = outbe_metadosis::api::offering_worldwide_days(storage.clone()).unwrap();
            assert_eq!(days.len(), 1);
            let day = outbe_metadosis::api::worldwide_day(storage, days[0])
                .unwrap()
                .unwrap();
            assert_eq!(day.offering_end - genesis_timestamp, 1_800);
        });
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

        topology
            .ensure_validator_domain_material_before_node_start()
            .unwrap();
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
    fn node_start_rejects_partially_staged_validator_domain_material() {
        let topology = topology();
        prepare_measurement_genesis_fixture(&topology);
        topology.prepare_measurement_fork_install().unwrap();
        fs::remove_file(topology.domain_root(0).unwrap().join("ocomp-evm-key.hex")).unwrap();

        let error = topology
            .ensure_validator_domain_material_before_node_start()
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("partial OCOMP validator domain material"));
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
    fn public_measurement_schedule_materializes_omitted_empty_metadosis_storage() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        let alloc = genesis["alloc"].as_object_mut().unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        alloc[&metadosis_key]
            .as_object_mut()
            .unwrap()
            .remove("storage");
        std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();

        topology.prepare_public_measurement_fork_install().unwrap();

        let materialized: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        let storage = materialized["alloc"][&metadosis_key]["storage"]
            .as_object()
            .expect("the public measurement fixture must materialize Metadosis storage");
        assert!(
            !storage.is_empty(),
            "the scheduled public measurement day must persist Metadosis state"
        );
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn public_measurement_schedule_seeds_consistent_green_day_and_oracle_vwaps() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let prepared = topology.prepare_public_measurement_fork_install().unwrap();
        let genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(topology.cfg.dir.join("genesis.json")).unwrap())
                .unwrap();
        let genesis_timestamp =
            u64::try_from(parse_hex_word(genesis["timestamp"].as_str().unwrap()).unwrap()).unwrap();
        let alloc = genesis["alloc"].as_object().unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        let oracle_key = find_alloc_address_key(alloc, outbe_primitives::addresses::ORACLE_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (address, account_key) in [
            (METADOSIS_ADDRESS, metadosis_key),
            (outbe_primitives::addresses::ORACLE_ADDRESS, oracle_key),
        ] {
            for (slot, value) in alloc[&account_key]["storage"].as_object().unwrap() {
                provider.storage.insert(
                    (address, parse_hex_word(slot).unwrap()),
                    parse_storage_word(value).unwrap(),
                );
            }
        }
        StorageHandle::enter(&mut provider, |storage| {
            let days = outbe_metadosis::api::offering_worldwide_days(storage.clone()).unwrap();
            assert_eq!(days.len(), 1);
            let day = outbe_metadosis::api::worldwide_day(storage.clone(), days[0])
                .unwrap()
                .unwrap();
            assert_eq!(
                day.worldwide_day,
                WorldwideDay::from_timestamp(genesis_timestamp)
            );
            assert_eq!(day.status, WwdStatus::Offering);
            assert_eq!(day.day_type, WwdDayType::Green);
            assert_eq!(
                day.offering_end - genesis_timestamp,
                OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS,
                "the hardware-SGX fixture must retain the frozen public offering allowance"
            );
            assert_eq!(day.scheduled_process_time, day.offering_end);
            assert!(day.metadosis_limit_amount > U256::ZERO);
            assert!(day.previous_vwap > U256::ZERO);
            assert!(day.current_vwap > day.previous_vwap);
            assert_eq!(
                outbe_oracle::api::day_type_pair_vwap(
                    storage.clone(),
                    day.worldwide_day.previous_date_key(),
                )
                .unwrap(),
                Some(day.previous_vwap)
            );
            assert_eq!(
                outbe_oracle::api::day_type_pair_vwap(storage.clone(), day.worldwide_day).unwrap(),
                Some(day.current_vwap)
            );
            let pair = outbe_oracle::api::DAY_TYPE_PAIR;
            let (_, pair_index) =
                outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
            let vwap = outbe_oracle::api::get_worldwide_day_vwap_for_pair(
                storage.clone(),
                day.worldwide_day,
                pair_index,
            )
            .unwrap()
            .unwrap();
            let scurve = outbe_oracle::api::get_max_active_scurve_value(
                storage.clone(),
                day.worldwide_day,
                pair,
            )
            .unwrap();
            let entry_price = vwap.max(scurve);
            assert!(entry_price > U256::ZERO);
            assert!(
                scurve > vwap,
                "the first OCOMP scenario must retain a higher active S-curve so Lysis proves it uses WWD VWAP only"
            );
            let current_rate = outbe_oracle::api::coen_rate_for(storage, 840).unwrap();
            assert_eq!(current_rate, entry_price * U256::from(2));
            let scale = outbe_primitives::units::SCALE_1E6_U256;
            assert_eq!(day.metadosis_limit_amount, U256::from(500) * scale);
            assert_eq!(OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE, "2");
            assert_eq!(OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO, "0");
            let amount_base = U256::from(
                OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE
                    .parse::<u64>()
                    .expect("canonical amount_base"),
            );
            let amount_atto = U256::from(
                OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO
                    .parse::<u64>()
                    .expect("canonical amount_atto"),
            );
            assert!(amount_atto < scale);
            let issuance = amount_base
                .checked_mul(scale)
                .and_then(|value| value.checked_add(amount_atto))
                .expect("canonical Tribute amount");
            assert_eq!(issuance, U256::from(2_000_000));
            let nominal = issuance * scale / day.current_vwap;
            for (population, expected_fraction, expected_load, expected_cost) in [
                (10_u64, 50_u64, 50_000_000_u64, 100_u64),
                (257_u64, 1_u64, 1_000_000_u64, 2_u64),
            ] {
                let total_nominal = nominal * U256::from(population);
                let allocation = (total_nominal * U256::from(32) / U256::from(100))
                    .min(day.metadosis_limit_amount);
                let fraction = allocation * scale / total_nominal;
                let gratis_load_minor = nominal * fraction / scale;
                assert_eq!(
                    fraction,
                    U256::from(expected_fraction),
                    "the {population}-Tribute fixture must preserve the canonical capped allocation fraction"
                );
                assert_eq!(
                    gratis_load_minor,
                    U256::from(expected_load),
                    "the {population}-Tribute fixture must preserve its exact per-Tribute Gratis load"
                );
                assert_eq!(
                    day.current_vwap * gratis_load_minor / scale,
                    U256::from(expected_cost),
                    "the {population}-Tribute fixture must produce a paid Nod under the canonical WWD-VWAP Lysis price"
                );
            }
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
        prepare_public_measurement_genesis_fixture_with_vote_window(
            &topology,
            OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS,
        );
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
        let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (address, account_key) in [
            (METADOSIS_ADDRESS, metadosis_key),
            (ORACLE_ADDRESS, oracle_key),
            (TRIBUTE_ADDRESS, tribute_key),
        ] {
            for (slot, value) in alloc[&account_key]["storage"].as_object().unwrap() {
                provider.storage.insert(
                    (address, parse_hex_word(slot).unwrap()),
                    parse_storage_word(value).unwrap(),
                );
            }
        }
        StorageHandle::enter(&mut provider, |storage| {
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            assert_eq!(
                oracle.config_vote_period.read().unwrap(),
                2,
                "dynamic OCOMP fixture must keep real feeder publications inside the six-hour freshness bound"
            );
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
            for worldwide_day in [prepared.first_worldwide_day, prepared.second_worldwide_day] {
                assert!(
                    outbe_oracle::api::day_type_pair_vwap(storage.clone(), worldwide_day)
                        .unwrap()
                        .is_some_and(|price| !price.is_zero()),
                    "dynamic OCOMP fixture must price COEN/840 for {worldwide_day}"
                );
            }
        });
        assert!(prepared.first_processing_time < prepared.second_processing_time);
        let genesis_timestamp = genesis["timestamp"]
            .as_str()
            .map(parse_hex_word)
            .transpose()
            .unwrap()
            .and_then(|timestamp| u64::try_from(timestamp).ok())
            .expect("dynamic fixture genesis timestamp");
        let expected_controlled_daily_cycle = genesis_timestamp
            .checked_div(86_400)
            .and_then(|day| day.checked_add(2))
            .and_then(|day| day.checked_mul(86_400))
            .and_then(|midnight| midnight.checked_add(1))
            .expect("controlled dynamic Job B daily-cycle boundary");
        assert_eq!(
            prepared.second_processing_time, expected_controlled_daily_cycle,
            "dynamic Job B must stay Scheduled until the controlled daily Cycle after membership activation"
        );
        assert_eq!(prepared.fork.install.founder_registrations.len(), 4);
        assert_eq!(
            prepared
                .fork
                .install
                .request_profile
                .capacity_profile
                .result_deadline_blocks,
            OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS,
            "dynamic OCOMP fixture must use the immutable genesis vote window"
        );
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
            let days = outbe_metadosis::api::offering_worldwide_days(storage.clone()).unwrap();
            assert_eq!(days, vec![WorldwideDay::from_timestamp(genesis_timestamp)]);
            assert_eq!(prepared.public_worldwide_day, Some(days[0]));
            let day = outbe_metadosis::api::worldwide_day(storage, days[0])
                .unwrap()
                .unwrap();
            assert_eq!(day.status, WwdStatus::Offering);
            assert_eq!(day.day_type, WwdDayType::Green);
            assert!(day.metadosis_limit_amount > U256::ZERO);
            assert_eq!(
                day.offering_end - genesis_timestamp,
                3_600,
                "the 257-Tribute real-SGX capacity population needs the full bounded one-hour offering window"
            );
            assert_eq!(
                day.offering_end,
                genesis_timestamp + OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS
            );
            assert_eq!(day.scheduled_process_time, day.offering_end);
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
    fn fresh_metadosis_capacity_fixture_materializes_omitted_storage() {
        let topology = topology();
        prepare_public_measurement_genesis_fixture(&topology);
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        let alloc = genesis["alloc"].as_object_mut().unwrap();
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)
            .unwrap()
            .unwrap();
        alloc[&metadosis_key]
            .as_object_mut()
            .unwrap()
            .remove("storage");
        std::fs::write(&genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();

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
        let oracle_key = find_alloc_address_key(alloc, ORACLE_ADDRESS)
            .unwrap()
            .unwrap();
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis).unwrap());
        for (slot, value) in alloc[&metadosis_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        for (slot, value) in alloc[&oracle_key]["storage"].as_object().unwrap() {
            provider.storage.insert(
                (ORACLE_ADDRESS, parse_hex_word(slot).unwrap()),
                parse_storage_word(value).unwrap(),
            );
        }
        let seeded = crate::world::localnet::worldwide_day()
            .parse::<WorldwideDay>()
            .unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            assert_eq!(oracle.config_vote_period.read().unwrap(), 2);
            assert!(
                outbe_metadosis::test_support::fresh_devnet_sentinel_is_pristine(
                    storage.clone(),
                    seeded,
                )
                .unwrap()
            );
            let start = seeded.start_timestamp();
            let end = start + outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS;
            assert!(
                outbe_oracle::api::store_worldwide_day_vwap_snapshot(
                    storage.clone(),
                    seeded,
                    start,
                    end,
                )
                .unwrap(),
                "fresh fixture must let production form an exact WWD VWAP"
            );
            let inputs = outbe_oracle::api::tribute_pricing_inputs(storage, 840, 840, seeded)
                .unwrap()
                .expect("fresh fixture must price the ordinary USD Tribute after formation");
            assert!(inputs.issuance_wwd_vwap_minor > U256::ZERO);
            assert!(inputs.reference_scurve_minor > inputs.reference_wwd_vwap_minor);
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
        prepare_public_measurement_genesis_fixture_with_vote_window(topology, 120);
    }

    #[cfg(feature = "ocomp-integration")]
    fn prepare_public_measurement_genesis_fixture_with_vote_window(
        topology: &OcompTopology,
        vote_window_blocks: u64,
    ) {
        prepare_measurement_genesis_fixture(topology);
        let genesis_path = topology.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&genesis_path).unwrap()).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        genesis["timestamp"] = serde_json::Value::String(format!("0x{now:x}"));
        genesis["config"][GENESIS_CONFIG_KEY] = serde_json::json!({
            "schemaVersion": 1,
            "metadosis": {
                "formingPeriodSeconds": 60,
                "lookbackDelaySeconds": 0,
                "offeringPeriodSeconds": 120,
                "waitingPeriodSeconds": 30,
                "bootstrapDurationSeconds": 300,
                "advanceIntervalSeconds": 10
            },
            "ocomp": { "computeVoteWindowBlocks": vote_window_blocks }
        });
        genesis["alloc"][format!("{METADOSIS_ADDRESS:#x}")] = serde_json::json!({
            "balance": "0x0",
            "code": "0xef",
            "storage": {},
        });
        let worldwide_day = WorldwideDay::from_timestamp(now);
        let day_pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle_config = outbe_oracle::genesis::OracleGenesisConfig::default_config();
        oracle_config.initial_rates = vec![(
            day_pair.address1(),
            day_pair.address2(),
            U256::from(1_000_000_u64),
        )];
        oracle_config.scurve_entries = vec![outbe_oracle::genesis::GenesisScurveEntry {
            base: day_pair.address1(),
            quote: day_pair.address2(),
            peak_day: worldwide_day.to_timestamp_utc(),
            peak_price: U256::from(1_000_000_u64),
        }];
        let mut oracle_provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut oracle_provider, |storage| {
            let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
            outbe_oracle::genesis::init_from_genesis(&mut oracle, &oracle_config)
        })
        .unwrap();
        let oracle_storage = oracle_provider
            .storage
            .into_iter()
            .filter(|((address, _), value)| {
                *address == outbe_primitives::addresses::ORACLE_ADDRESS && !value.is_zero()
            })
            .map(|((_, slot), value)| {
                (
                    format!("0x{slot:064x}"),
                    serde_json::Value::String(format!("0x{value:064x}")),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        genesis["alloc"][format!("{:#x}", outbe_primitives::addresses::ORACLE_ADDRESS)] = serde_json::json!({
            "balance": "0x0",
            "code": "0xef",
            "storage": oracle_storage,
        });
        genesis["alloc"][format!("{TRIBUTE_ADDRESS:#x}")] = serde_json::json!({
            "balance": "0x0",
            "code": "0xef",
            "storage": {},
        });
        std::fs::write(genesis_path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
    }

    #[test]
    fn node_owned_ocomp_roles_are_not_external_harness_processes() {
        for role in ["supervisor", "follower"] {
            assert!(serde_json::from_str::<OcompProcessRole>(&format!("\"{role}\"")).is_err());
        }
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
            .attach_owned(
                Some(0),
                OcompProcessRole::SnapshotExporter,
                None,
                child_guard(),
            )
            .unwrap();
        topology
            .attach_owned(
                Some(1),
                OcompProcessRole::SnapshotExporter,
                None,
                child_guard(),
            )
            .unwrap();

        topology
            .apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index: 0 })
            .unwrap();

        assert!(topology.records[0].stopped_at_millis.is_some());
        assert!(topology.records[1].stopped_at_millis.is_none());
        assert!(topology.domains[0].snapshot_exporter.is_none());
        assert!(topology.domains[1].snapshot_exporter.is_some());
        assert_eq!(
            topology.faults,
            vec![OcompFaultRecordV1 {
                fault: OcompProcessFault::StopSnapshotExporter { validator_index: 0 },
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

    #[test]
    fn clock_restart_suspends_and_preserves_the_exact_live_role_inventory() {
        if std::env::var_os(CHILD_MODE).is_some() {
            loop {
                std::thread::park_timeout(std::time::Duration::from_secs(60));
            }
        }

        let mut topology = topology();
        topology.add_active_validator_domain(4).unwrap();
        for validator_index in 0..5_u8 {
            topology
                .attach_owned(
                    Some(validator_index),
                    OcompProcessRole::SnapshotExporter,
                    None,
                    child_guard(),
                )
                .unwrap();
        }
        for validator_index in [0_u8, 1, 4] {
            topology
                .attach_owned(
                    Some(validator_index),
                    OcompProcessRole::Worker,
                    Some(0),
                    child_guard(),
                )
                .unwrap();
        }

        let faults_before = topology.faults.clone();
        let resume = topology.suspend_node_facing_roles().unwrap();

        assert_eq!(resume.snapshot_exporters, vec![0, 1, 2, 3, 4]);
        assert_eq!(resume.workers, vec![(0, 0), (1, 0), (4, 0)]);
        assert_eq!(topology.faults, faults_before);
        assert!(topology
            .domains
            .iter()
            .all(|domain| { domain.snapshot_exporter.is_none() && domain.workers.is_empty() }));
    }
}
