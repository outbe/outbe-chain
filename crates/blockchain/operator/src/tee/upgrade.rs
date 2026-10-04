//! Owner-only, crash-consistent enclave upgrade checkpoints.
//!
//! The operator never starts or stops Gramine. It records externally completed
//! checkpoints for finalized network-key provisioning and binding transitions.
//! A separate legacy checkpoint supports copying an existing MRSIGNER seal.

use super::journal_storage::{sync_directory, JournalPaths};
use super::JournalSnapshotV1;

mod storage;
use storage::{
    read_private_bounded_file, read_snapshot, validate_directory, validate_private_file,
};

#[cfg(test)]
mod compatibility_tests;

mod context;
mod identity;
mod journal_validation;
mod preparation;
mod relay;
use identity::{
    ensure_transition_source_or_target_v1, transition_target_matches_v1,
    validate_candidate_identity_v1,
};
use journal_validation::{security_material, validate_checkpoint_transition};
use relay::{finalized_transition_matches_v1, prepare_upgrade_relay_v1};
mod submission;
mod wire;

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    },
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolCall as _;
use eyre::{Result, WrapErr as _};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
        RegistrationIntentV1, RegistryMutatorV1, TeeRegistryGasScheduleV1,
        TransitionKeyReadyProofV1,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::{
    acquire_dcap_collateral_v1, dcap_collateral_validity_window_v1,
    load_replacement_candidate_submission, persist_replacement_candidate_submission,
    ReplacementCandidateEnclaveV1, ReplacementCandidateSubmissionV1,
};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::RenewalRpc,
    tx::{buffered_gas_price, RawRelayTransactionV1, RelaySignerV1},
};

use super::{
    read_finalized_upgrade_policy_v1,
    registry::{read_finalized_bound_renewal_view_v1, NodeBindingSelectorV1},
};

const DIRECTORY: &str = "tee-upgrade-v1";
const SEALED_ROOT: &str = "sealed_root.bin";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SEALED_ROOT_BYTES: u64 = 1024 * 1024;
const MAX_RELAY_VARIANTS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeContextV1 {
    pub predecessor_manifest_hash: B256,
    pub candidate_manifest_hash: B256,
    pub successor_policy_hash: B256,
    pub activation_height: u64,
    pub active_tee_dir: PathBuf,
    pub candidate_tee_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedUpgradeSubmissionV1 {
    pub intent_hash: B256,
    pub evidence_hash: B256,
    pub calldata_hash: B256,
    pub relay: Address,
    pub relay_variants: Vec<RawRelayTransactionV1>,
}

pub trait UpgradeNodeSignerV1 {
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]>;
}

impl<F> UpgradeNodeSignerV1 for F
where
    F: Fn(B256) -> Result<[u8; 65]>,
{
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]> {
        self(hash)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeSubmissionOutcomeV1 {
    Submitted {
        transaction_hash: B256,
        replayed: bool,
    },
    AlreadySubmitted {
        transaction_hash: B256,
    },
    Finalized {
        transaction_hash: B256,
        finalized_height: u64,
    },
    Promoted {
        transaction_hash: B256,
        finalized_height: u64,
    },
}

/// Commitments established when the candidate key becomes ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpgradeSecurityMaterialV1 {
    pub sealed_root_hash: B256,
    pub resident_offer_public: B256,
    pub proof_hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpgradeJournalStateV1 {
    CandidatePrepared {
        context: UpgradeContextV1,
    },
    KeyProvisioned {
        context: UpgradeContextV1,
        sealed_root_hash: B256,
    },
    CandidateKeyReady {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
    },
    SubmissionPrepared {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
    },
    Submitted {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
        submitted_at_finalized_height: u64,
        transaction_hashes: Vec<B256>,
    },
    Finalized {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
        finalized_height: u64,
        finalized_hash: B256,
    },
    Promoted {
        context: UpgradeContextV1,
        security: UpgradeSecurityMaterialV1,
        submission: PreparedUpgradeSubmissionV1,
        finalized_height: u64,
        finalized_hash: B256,
    },
    TerminalMissedCutoff {
        context: UpgradeContextV1,
        finalized_height: u64,
        activation_height: u64,
    },
}

impl UpgradeJournalStateV1 {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::CandidatePrepared { .. } => "candidatePrepared",
            Self::KeyProvisioned { .. } => "keyProvisioned",
            Self::CandidateKeyReady { .. } => "candidateKeyReady",
            Self::SubmissionPrepared { .. } => "submissionPrepared",
            Self::Submitted { .. } => "submitted",
            Self::Finalized { .. } => "finalized",
            Self::Promoted { .. } => "promoted",
            Self::TerminalMissedCutoff { .. } => "terminalMissedCutoff",
        }
    }

    pub const fn context(&self) -> &UpgradeContextV1 {
        match self {
            Self::CandidatePrepared { context }
            | Self::KeyProvisioned { context, .. }
            | Self::CandidateKeyReady { context, .. }
            | Self::SubmissionPrepared { context, .. }
            | Self::Submitted { context, .. }
            | Self::Finalized { context, .. }
            | Self::Promoted { context, .. }
            | Self::TerminalMissedCutoff { context, .. } => context,
        }
    }
}

/// V1 upgrade journal envelope, retaining the upgrade lifecycle type.
pub type UpgradeJournalSnapshotV1 = JournalSnapshotV1<UpgradeJournalStateV1>;

impl UpgradeJournalSnapshotV1 {
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.generation == 0 {
            eyre::bail!("unsupported upgrade journal version or generation");
        }
        self.lifecycle.validate()
    }
}

pub struct UpgradeJournalGuardV1 {
    paths: JournalPaths,
    _lock: File,
}

fn is_next_upgrade(current: &UpgradeJournalStateV1, next: &UpgradeJournalStateV1) -> bool {
    let (
        UpgradeJournalStateV1::Promoted { context: old, .. },
        UpgradeJournalStateV1::CandidatePrepared { context: new },
    ) = (current, next)
    else {
        return false;
    };
    new.predecessor_manifest_hash == old.candidate_manifest_hash
        && new.activation_height > old.activation_height
        && new.successor_policy_hash != old.successor_policy_hash
}

pub fn inspect_upgrade_journal_v1(
    node_data_dir: &Path,
) -> Result<Option<UpgradeJournalSnapshotV1>> {
    let paths = JournalPaths::new(node_data_dir, DIRECTORY);
    if !paths.root.exists() {
        return Ok(None);
    }
    validate_directory(&paths.root)?;
    read_snapshot(&paths.journal)
}

pub fn prepare_upgrade_journal_v1(
    node_data_dir: &Path,
    context: UpgradeContextV1,
) -> Result<UpgradeJournalSnapshotV1> {
    context.validate()?;
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    if let Some(existing) = guard.load()? {
        if existing.lifecycle.context() == &context {
            return Ok(existing);
        }
        if !is_next_upgrade(
            &existing.lifecycle,
            &UpgradeJournalStateV1::CandidatePrepared {
                context: context.clone(),
            },
        ) {
            eyre::bail!("another enclave upgrade is already journaled");
        }
    }
    if let Some(renewal) = super::renewal_journal::inspect_journal(node_data_dir)? {
        if matches!(
            renewal.lifecycle,
            super::renewal_journal::RenewalJournalStateV1::Prepared { .. }
                | super::renewal_journal::RenewalJournalStateV1::Submitted { .. }
        ) {
            eyre::bail!(
                "cannot prepare enclave upgrade while an exact renewal submission is pending"
            );
        }
    }
    let snapshot =
        UpgradeJournalSnapshotV1::new(UpgradeJournalStateV1::CandidatePrepared { context });
    guard.store(snapshot)?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("prepared upgrade journal disappeared"))
}

pub fn copy_same_platform_sealed_root_and_checkpoint_v1(
    node_data_dir: &Path,
) -> Result<UpgradeJournalSnapshotV1> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    match current.lifecycle {
        UpgradeJournalStateV1::CandidatePrepared { context } => {
            let sealed_root_hash = copy_same_platform_sealed_root_v1(&context)?;
            guard.store(UpgradeJournalSnapshotV1::new(
                UpgradeJournalStateV1::KeyProvisioned {
                    context,
                    sealed_root_hash,
                },
            ))?;
            guard
                .load()?
                .ok_or_else(|| eyre::eyre!("key-provisioned checkpoint disappeared"))
        }
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        } => {
            let actual = copy_same_platform_sealed_root_v1(&context)?;
            if actual != sealed_root_hash {
                eyre::bail!("candidate sealed root changed after its durable checkpoint");
            }
            Ok(UpgradeJournalSnapshotV1 {
                lifecycle: UpgradeJournalStateV1::KeyProvisioned {
                    context,
                    sealed_root_hash,
                },
                ..current
            })
        }
        _ => {
            eyre::bail!("sealed root can be copied only at candidate-prepared checkpoint");
        }
    }
}

/// Record a key that the candidate received from the network and sealed locally.
/// The existing field is the actual new local seal hash, never a source blob hash.
pub fn record_network_key_provisioned_v1(node_data_dir: &Path) -> Result<UpgradeJournalSnapshotV1> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade is not prepared"))?;
    if let Some(renewal) = super::renewal_journal::inspect_journal(node_data_dir)? {
        if matches!(
            renewal.lifecycle,
            super::renewal_journal::RenewalJournalStateV1::Prepared { .. }
                | super::renewal_journal::RenewalJournalStateV1::Submitted { .. }
        ) {
            eyre::bail!("key is sealed; finish pending renewal before checkpointing transition readiness, then rerun upgrade-provision");
        }
    }
    let context = current.lifecycle.context().clone();
    let bytes = read_private_bounded_file(
        &context.candidate_tee_dir.join(SEALED_ROOT),
        MAX_SEALED_ROOT_BYTES,
    )?;
    let combined = bytes.starts_with(b"TSGX1");
    #[cfg(feature = "local-e2e")]
    let combined = combined || is_local_e2e_combined_seal(&bytes);
    if !combined {
        eyre::bail!("network-provisioned candidate must use combined SGX sealing");
    }
    let sealed_root_hash = keccak256(bytes);
    if let UpgradeJournalStateV1::KeyProvisioned {
        sealed_root_hash: expected,
        ..
    } = current.lifecycle
    {
        if expected != sealed_root_hash {
            eyre::bail!("provisioned seal changed after checkpoint");
        }
        return Ok(current);
    }
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("provisioned journal disappeared"))
}

// The separate E2E CLI accepts software seals only for the fixed local devnet.
#[cfg(feature = "local-e2e")]
fn is_local_e2e_combined_seal(bytes: &[u8]) -> bool {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    let start = 5 + 5 + 32; // LE2E1 + TSEAL + canonical seal header.
    bytes.starts_with(b"LE2E1TSEAL")
        && bytes
            .get(start..start + NetworkBindingV1::CANONICAL_LEN)
            .and_then(|v| NetworkBindingV1::decode_canonical(v).ok())
            .is_some_and(|v| {
                v.chain_id
                    == alloy_primitives::U256::from(outbe_primitives::chain::DEVNET_CHAIN_ID)
                        .to_be_bytes::<32>()
                    && v.attestation_mode == AttestationMode::GramineDirectDev
            })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkUpgradeSubmissionV1 {
    pub candidate_manifest_hash: B256,
    pub evidence: Vec<u8>,
    pub context: Vec<u8>,
    pub calldata: Vec<u8>,
    pub transaction: RawRelayTransactionV1,
}

impl UpgradeJournalGuardV1 {
    pub fn load_network_submission(&self) -> Result<Option<NetworkUpgradeSubmissionV1>> {
        let path = self.paths.root.join("network-submission.json");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_private_bounded_file(&path, MAX_JOURNAL_BYTES)?;
        let value: NetworkUpgradeSubmissionV1 =
            serde_json::from_slice(&bytes).wrap_err("decode upgrade network submission")?;
        value.validate()?;
        Ok(Some(value))
    }
    pub fn store_network_submission(&self, value: &NetworkUpgradeSubmissionV1) -> Result<()> {
        value.validate()?;
        let encoded = serde_json::to_vec(value)?;
        if encoded.len() as u64 > MAX_JOURNAL_BYTES {
            eyre::bail!("network submission exceeds cap");
        }
        let next = self.paths.root.join("network-submission.next");
        if next.exists() {
            validate_private_file(&next, MAX_JOURNAL_BYTES)?;
            fs::remove_file(&next)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&next)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(next, self.paths.root.join("network-submission.json"))?;
        sync_directory(&self.paths.root)
    }
}

pub fn record_candidate_key_ready_v1(
    node_data_dir: &Path,
    intent: &RegistrationIntentV1,
    proof: &TransitionKeyReadyProofV1,
    expected_offer_public: [u8; 32],
) -> Result<UpgradeJournalSnapshotV1> {
    proof
        .verify_for_transition(intent, expected_offer_public)
        .map_err(|error| eyre::eyre!("candidate key-ready proof is invalid: {error}"))?;
    let proof_hash = keccak256(
        proof
            .encode_canonical()
            .map_err(|error| eyre::eyre!("encode candidate key-ready proof: {error}"))?,
    );
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::KeyProvisioned {
        context,
        sealed_root_hash,
    } = current.lifecycle
    else {
        eyre::bail!("candidate key readiness requires the key-provisioned checkpoint");
    };
    if proof.candidate_manifest_hash != context.candidate_manifest_hash {
        eyre::bail!("candidate key-ready proof does not match the journaled manifest");
    }
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::CandidateKeyReady {
            context,
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash,
                resident_offer_public: B256::from(expected_offer_public),
                proof_hash,
            },
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("candidate-key-ready checkpoint disappeared"))
}

pub fn record_upgrade_submission_prepared_v1(
    node_data_dir: &Path,
    submission: PreparedUpgradeSubmissionV1,
) -> Result<UpgradeJournalSnapshotV1> {
    submission.validate()?;
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::CandidateKeyReady { context, security } = current.lifecycle else {
        eyre::bail!("upgrade submission requires candidate-key-ready checkpoint");
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::SubmissionPrepared {
            context,
            security,
            submission,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("submission-prepared checkpoint disappeared"))
}

pub fn record_upgrade_submitted_v1(
    node_data_dir: &Path,
    finalized_height: u64,
) -> Result<UpgradeJournalSnapshotV1> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::SubmissionPrepared {
        context,
        security,
        submission,
    } = current.lifecycle
    else {
        eyre::bail!("upgrade relay requires submission-prepared checkpoint");
    };
    let transaction_hashes = submission
        .relay_variants
        .iter()
        .map(|variant| variant.transaction_hash)
        .collect();
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::Submitted {
            context,
            security,
            submission,
            submitted_at_finalized_height: finalized_height,
            transaction_hashes,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("submitted upgrade checkpoint disappeared"))
}

pub fn record_upgrade_finalized_v1(
    node_data_dir: &Path,
    finalized_height: u64,
    finalized_hash: B256,
) -> Result<UpgradeJournalSnapshotV1> {
    if finalized_height == 0 || finalized_hash.is_zero() {
        eyre::bail!("finalized upgrade authority is incomplete");
    }
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::Submitted {
        context,
        security,
        submission,
        ..
    } = current.lifecycle
    else {
        eyre::bail!("upgrade finalization requires a submitted checkpoint");
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::Finalized {
            context,
            security,
            submission,
            finalized_height,
            finalized_hash,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("finalized upgrade checkpoint disappeared"))
}

pub fn record_upgrade_promoted_v1(node_data_dir: &Path) -> Result<UpgradeJournalSnapshotV1> {
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::Finalized {
        context,
        security,
        submission,
        finalized_height,
        finalized_hash,
    } = current.lifecycle
    else {
        eyre::bail!("upgrade promotion requires a finalized checkpoint");
    };
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::Promoted {
            context,
            security,
            submission,
            finalized_height,
            finalized_hash,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("promoted upgrade checkpoint disappeared"))
}

pub fn record_upgrade_missed_cutoff_v1(
    node_data_dir: &Path,
    finalized_height: u64,
    activation_height: u64,
) -> Result<UpgradeJournalSnapshotV1> {
    if activation_height == 0 || finalized_height < activation_height {
        eyre::bail!("successor activation cutoff has not been reached");
    }
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    let current = guard
        .load()?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    if matches!(
        current.lifecycle,
        UpgradeJournalStateV1::Finalized { .. } | UpgradeJournalStateV1::Promoted { .. }
    ) {
        eyre::bail!("a finalized upgrade cannot become a missed-cutoff terminal");
    }
    let context = current.lifecycle.context().clone();
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::TerminalMissedCutoff {
            context,
            finalized_height,
            activation_height,
        },
    ))?;
    guard
        .load()?
        .ok_or_else(|| eyre::eyre!("missed-cutoff checkpoint disappeared"))
}

/// RPC, relay and node signing capabilities for the replacement candidate.
pub struct UpgradeSubmissionServicesV1<'a, R, N> {
    pub rpc: &'a R,
    pub relay: &'a RelaySignerV1,
    pub candidate: &'a mut ReplacementCandidateEnclaveV1,
    pub node_signer: &'a N,
}

/// Node storage and binding target for one durable upgrade submission.
pub struct UpgradeSubmissionRequestV1<'a> {
    pub node_data_dir: &'a Path,
    pub selector: &'a NodeBindingSelectorV1,
    pub binding_id: B256,
    pub requested_valid_until: u64,
}

/// Resume one same-platform measurement transition from its durable
/// checkpoints. The deployment manager must have already copied the root and
/// restarted candidate B before calling this reducer.
pub async fn run_upgrade_submission_v1<R: RenewalRpc + Sync, N: UpgradeNodeSignerV1>(
    services: UpgradeSubmissionServicesV1<'_, R, N>,
    request: UpgradeSubmissionRequestV1<'_>,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let mut service = submission::UpgradeSubmissionService {
        rpc: services.rpc,
        relay: services.relay,
        candidate: services.candidate,
        node_signer: services.node_signer,
        node_data_dir: request.node_data_dir,
        selector: request.selector,
        binding_id: request.binding_id,
        requested_valid_until: request.requested_valid_until,
    };
    submission::run(&mut service).await
}

async fn reset_expired_upgrade_submission_v1(
    rpc: &(impl RenewalRpc + Sync),
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
    snapshot: &UpgradeJournalSnapshotV1,
) -> Result<bool> {
    let durable = load_replacement_candidate_submission(node_data_dir)?
        .ok_or_else(|| eyre::eyre!("upgrade submission checkpoint has no durable evidence"))?;
    let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(|e| eyre::eyre!("invalid transition evidence: {e}"))?;
    let view = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    if view.schedule.finalized_timestamp < evidence.intent().requested_valid_until
        || transition_target_matches_v1(&view.binding, evidence.intent())
    {
        return Ok(false);
    }
    ensure_transition_source_or_target_v1(&view.binding, evidence.intent())?;
    let sealed_root_hash = security_material(&snapshot.lifecycle)
        .ok_or_else(|| eyre::eyre!("upgrade has no sealed-root checkpoint"))?
        .0;
    // Reset first. KeyProvisioned can reconcile a crash while the old evidence still
    // exists, and cannot relay again until a fresh key-ready proof is persisted.
    let guard = UpgradeJournalGuardV1::acquire(node_data_dir)?;
    if guard.load()?.as_ref() != Some(snapshot) {
        eyre::bail!("upgrade changed during expiration recovery; retry");
    }
    guard.store(UpgradeJournalSnapshotV1::new(
        UpgradeJournalStateV1::KeyProvisioned {
            context: snapshot.lifecycle.context().clone(),
            sealed_root_hash,
        },
    ))?;
    Ok(true)
}

fn last_transaction_hash(submission: &PreparedUpgradeSubmissionV1) -> Result<B256> {
    submission
        .relay_variants
        .last()
        .map(|raw| raw.transaction_hash)
        .ok_or_else(|| eyre::eyre!("upgrade submission has no relay transaction"))
}

fn transaction_is_already_known(error: &eyre::Report) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("already known") || message.contains("known transaction")
}

pub struct PreparedTransitionEvidenceV1 {
    pub intent: RegistrationIntentV1,
    pub evidence: AttestationEvidenceV1,
    pub enclave_signature: [u8; 64],
    pub collateral_issue_floor: u64,
    pub collateral_expiration: u64,
}

pub fn generate_transition_evidence_v1(
    candidate: &mut ReplacementCandidateEnclaveV1,
    intent: RegistrationIntentV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<PreparedTransitionEvidenceV1> {
    if policy.attestation_mode == AttestationMode::GramineDirectDev {
        let evidence = if intent.operation == AttestationOperationV1::PrepareEnclaveUpgrade {
            let signature = candidate.sign_registration_intent_dev_v1(&intent)?;
            AttestationEvidenceV1::GramineDirectDev(
                outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1 {
                    intent: intent.clone(),
                    dev_attestation_public: intent.attestation_ed25519,
                    dev_signature: signature,
                    transition_key_ready_proof: None,
                },
            )
        } else {
            candidate
                .generate_transition_evidence_dev_v1(&intent)
                .map_err(|e| eyre::eyre!("generate DirectDev transition evidence: {e}"))?
        };
        let AttestationEvidenceV1::GramineDirectDev(dev) = &evidence else {
            unreachable!();
        };
        return Ok(PreparedTransitionEvidenceV1 {
            intent,
            enclave_signature: dev.dev_signature,
            evidence,
            collateral_issue_floor: 0,
            collateral_expiration: u64::MAX,
        });
    }
    let generated = candidate
        .generate_dcap_quote(&intent)
        .map_err(|error| eyre::eyre!("generate candidate transition quote: {error}"))?;
    let components = acquire_dcap_collateral_v1(&generated.quote_body)
        .map_err(|error| eyre::eyre!("acquire candidate transition collateral: {error}"))?;
    let evidence = DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body,
        components,
        transition_key_ready_proof: generated.transition_key_ready_proof,
    };
    let window = dcap_collateral_validity_window_v1(&evidence, policy)
        .map_err(|error| eyre::eyre!("validate transition collateral: {error:?}"))?;
    Ok(PreparedTransitionEvidenceV1 {
        intent,
        evidence: AttestationEvidenceV1::Dcap(evidence),
        enclave_signature: generated.enclave_signature,
        collateral_issue_floor: window.issue_floor,
        collateral_expiration: window.expiration_ceiling,
    })
}

pub fn transition_intent_v1(
    candidate: &ReplacementCandidateEnclaveV1,
    active: &super::registry::FinalizedRenewalChainViewV1,
    successor: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    binding_id: B256,
    requested_valid_until: u64,
) -> Result<RegistrationIntentV1> {
    if binding_id.is_zero() || binding_id == active.binding.binding_id {
        eyre::bail!("measurement transition requires a fresh nonzero binding id");
    }
    let manifest = candidate.manifest();
    let intent = RegistrationIntentV1 {
        chain_id: successor.chain_id,
        genesis_hash: successor.genesis_hash,
        operation: AttestationOperationV1::TransitionEnclaveMeasurement,
        attestation_mode: successor.attestation_mode,
        policy_hash: successor
            .policy_hash()
            .map_err(|error| eyre::eyre!("hash staged successor policy: {error}"))?,
        node_id: manifest.node_id.clone(),
        enclave_id: manifest
            .enclave_id()
            .map_err(|error| eyre::eyre!("derive candidate enclave id: {error}"))?,
        binding_id,
        binding_version: active
            .binding
            .binding_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("binding version exhausted"))?,
        registration_version: active
            .binding
            .registration_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("registration version exhausted"))?,
        renewal_nonce: active.binding.renewal_nonce,
        transition_nonce: active
            .binding
            .transition_nonce
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("transition nonce exhausted"))?,
        requested_valid_until,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: active.binding.node_host_authorization_hash,
    };
    manifest.validate_intent_binding(&intent).map_err(|error| {
        eyre::eyre!("candidate manifest does not bind transition intent: {error}")
    })?;
    Ok(intent)
}

/// Copy exactly `sealed_root.bin` from active A to prepared candidate B.
/// A byte-identical destination is an idempotent crash retry; any other
/// pre-existing destination fails closed.
pub fn copy_same_platform_sealed_root_v1(context: &UpgradeContextV1) -> Result<B256> {
    context.validate()?;
    validate_directory(&context.active_tee_dir)
        .wrap_err("validate active enclave tee directory")?;
    validate_directory(&context.candidate_tee_dir)
        .wrap_err("validate candidate enclave tee directory")?;
    let source = context.active_tee_dir.join(SEALED_ROOT);
    let destination = context.candidate_tee_dir.join(SEALED_ROOT);
    let bytes = read_private_bounded_file(&source, MAX_SEALED_ROOT_BYTES)
        .wrap_err("read active sealed root")?;
    if bytes.is_empty() {
        eyre::bail!("active sealed root is empty");
    }
    let hash = keccak256(&bytes);
    if destination.exists() {
        let existing = read_private_bounded_file(&destination, MAX_SEALED_ROOT_BYTES)
            .wrap_err("read existing candidate sealed root")?;
        if existing != bytes {
            eyre::bail!("candidate sealed root already exists with different bytes");
        }
        return Ok(hash);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&destination)
        .wrap_err("create candidate sealed root")?;
    output
        .write_all(&bytes)
        .wrap_err("write candidate sealed root")?;
    output.sync_all().wrap_err("fsync candidate sealed root")?;
    sync_directory(&context.candidate_tee_dir)?;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    fn store_checkpoint(guard: &UpgradeJournalGuardV1, state: UpgradeJournalStateV1) {
        guard.store(UpgradeJournalSnapshotV1::new(state)).unwrap();
    }
    fn store_candidate_prepared(guard: &UpgradeJournalGuardV1, context: &UpgradeContextV1) {
        store_checkpoint(
            guard,
            UpgradeJournalStateV1::CandidatePrepared {
                context: context.clone(),
            },
        );
    }

    fn key_provisioned(context: UpgradeContextV1, sealed_root_hash: B256) -> UpgradeJournalStateV1 {
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        }
    }
    fn key_ready(context: UpgradeContextV1, sealed_root_hash: B256) -> UpgradeJournalStateV1 {
        UpgradeJournalStateV1::CandidateKeyReady {
            context,
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash,
                resident_offer_public: B256::repeat_byte(7),
                proof_hash: B256::repeat_byte(8),
            },
        }
    }

    #[test]
    fn snapshot_legacy_json_loads_without_rewrite_and_keeps_exact_bytes() {
        let legacy = concat!(
            "{\"version\":1,\"generation\":7,\"lifecycle\":{\"state\":\"candidatePrepared\",",
            "\"context\":{",
            "\"predecessorManifestHash\":\"0x0101010101010101010101010101010101010101010101010101010101010101\",",
            "\"candidateManifestHash\":\"0x0202020202020202020202020202020202020202020202020202020202020202\",",
            "\"successorPolicyHash\":\"0x0303030303030303030303030303030303030303030303030303030303030303\",",
            "\"activationHeight\":100,\"activeTeeDir\":\"/legacy/active\",",
            "\"candidateTeeDir\":\"/legacy/candidate\"}}}"
        );
        let root = tempfile::tempdir().unwrap();
        let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
        let path = root.path().join(DIRECTORY).join("journal.json");
        fs::write(&path, legacy).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        let snapshot = guard.load().unwrap().unwrap();
        assert_eq!(snapshot.generation, 7);
        assert_eq!(snapshot.lifecycle.label(), "candidatePrepared");
        assert_eq!(serde_json::to_vec(&snapshot).unwrap(), legacy.as_bytes());
        assert_eq!(fs::read(&path).unwrap(), legacy.as_bytes());
        let value: serde_json::Value = serde_json::from_str(legacy).unwrap();
        for missing in ["version", "generation", "lifecycle"] {
            let mut malformed = value.clone();
            malformed.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<UpgradeJournalSnapshotV1>(malformed).is_err());
        }
        let mut unknown = value;
        unknown["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<UpgradeJournalSnapshotV1>(unknown).is_err());
    }

    #[test]
    fn snapshot_headers_reject_invalid_values_before_lifecycle_validation() {
        let mut invalid_context = context(Path::new("/unused"));
        invalid_context.activation_height = 0;
        let lifecycle = UpgradeJournalStateV1::CandidatePrepared {
            context: invalid_context,
        };
        for (version, generation) in [(0, 1), (2, 1), (1, 0)] {
            let snapshot = UpgradeJournalSnapshotV1 {
                version,
                generation,
                lifecycle: lifecycle.clone(),
            };
            assert_eq!(
                snapshot.validate().unwrap_err().to_string(),
                "unsupported upgrade journal version or generation"
            );
        }
        assert_ne!(
            UpgradeJournalSnapshotV1::new(lifecycle)
                .validate()
                .unwrap_err()
                .to_string(),
            "unsupported upgrade journal version or generation"
        );
    }

    #[test]
    fn software_seal_checkpoint_is_explicitly_feature_and_network_gated() {
        use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        private_dir(&context.candidate_tee_dir);
        prepare_upgrade_journal_v1(root.path(), context.clone()).unwrap();
        let seal = context.candidate_tee_dir.join(SEALED_ROOT);
        let bytes_for = |chain_id, mode| {
            let mut bytes = b"LE2E1TSEAL".to_vec();
            bytes.extend([0u8; 32]);
            bytes.extend(
                NetworkBindingV1 {
                    chain_id,
                    genesis_hash: B256::repeat_byte(1),
                    attestation_mode: mode,
                }
                .encode_canonical()
                .unwrap(),
            );
            bytes
        };
        let local_chain = U256::from(outbe_primitives::chain::DEVNET_CHAIN_ID).to_be_bytes::<32>();
        for bytes in [
            b"LE2E1".to_vec(),
            bytes_for([1; 32], AttestationMode::GramineDirectDev),
            bytes_for(local_chain, AttestationMode::DcapRequired),
        ] {
            fs::write(&seal, bytes).unwrap();
            fs::set_permissions(&seal, fs::Permissions::from_mode(FILE_MODE)).unwrap();
            assert!(record_network_key_provisioned_v1(root.path()).is_err());
        }
        fs::write(
            &seal,
            bytes_for(local_chain, AttestationMode::GramineDirectDev),
        )
        .unwrap();
        assert_eq!(
            record_network_key_provisioned_v1(root.path()).is_ok(),
            cfg!(feature = "local-e2e")
        );
    }

    fn private_dir(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE)).unwrap();
    }

    pub(super) fn context(root: &Path) -> UpgradeContextV1 {
        UpgradeContextV1 {
            predecessor_manifest_hash: B256::repeat_byte(1),
            candidate_manifest_hash: B256::repeat_byte(2),
            successor_policy_hash: B256::repeat_byte(3),
            activation_height: 100,
            active_tee_dir: root.join("active"),
            candidate_tee_dir: root.join("candidate"),
        }
    }

    pub(super) fn submission() -> PreparedUpgradeSubmissionV1 {
        let calldata = vec![1, 2];
        let raw_transaction = vec![3, 4];
        PreparedUpgradeSubmissionV1 {
            intent_hash: B256::repeat_byte(3),
            evidence_hash: B256::repeat_byte(4),
            calldata_hash: keccak256(&calldata),
            relay: Address::repeat_byte(5),
            relay_variants: vec![RawRelayTransactionV1 {
                relay: Address::repeat_byte(5),
                chain_id: 1,
                account_nonce: 2,
                gas_price: U256::from(3),
                gas_limit: 4,
                calldata_hash: keccak256(&calldata),
                transaction_hash: keccak256(&raw_transaction),
                raw_transaction,
            }],
        }
    }

    #[test]
    fn network_provisioning_checkpoint_is_idempotent_and_rejects_changed_seal() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        private_dir(&context.candidate_tee_dir);
        prepare_upgrade_journal_v1(root.path(), context.clone()).unwrap();
        let seal = context.candidate_tee_dir.join(SEALED_ROOT);
        fs::write(&seal, b"TSGX1 test-only sealed bytes").unwrap();
        fs::set_permissions(&seal, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        let first = record_network_key_provisioned_v1(root.path()).unwrap();
        assert_eq!(
            record_network_key_provisioned_v1(root.path()).unwrap(),
            first
        );
        fs::write(&seal, b"TSGX1 altered test-only sealed bytes").unwrap();
        assert!(record_network_key_provisioned_v1(root.path()).is_err());
    }

    #[test]
    fn copies_only_sealed_root_and_exact_retry_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        private_dir(&context.active_tee_dir);
        private_dir(&context.candidate_tee_dir);
        fs::write(context.active_tee_dir.join(SEALED_ROOT), b"sealed-root").unwrap();
        fs::set_permissions(
            context.active_tee_dir.join(SEALED_ROOT),
            fs::Permissions::from_mode(FILE_MODE),
        )
        .unwrap();
        fs::write(context.active_tee_dir.join("must-not-copy"), b"other").unwrap();

        let hash = copy_same_platform_sealed_root_v1(&context).unwrap();
        assert_eq!(hash, keccak256(b"sealed-root"));
        assert_eq!(
            fs::read(context.candidate_tee_dir.join(SEALED_ROOT)).unwrap(),
            b"sealed-root"
        );
        assert!(!context.candidate_tee_dir.join("must-not-copy").exists());
        assert_eq!(copy_same_platform_sealed_root_v1(&context).unwrap(), hash);

        fs::write(context.candidate_tee_dir.join(SEALED_ROOT), b"conflict").unwrap();
        assert!(copy_same_platform_sealed_root_v1(&context).is_err());
    }

    #[test]
    fn journal_roundtrips_all_security_relevant_checkpoints() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
        store_candidate_prepared(&guard, &context);
        store_checkpoint(
            &guard,
            key_provisioned(context.clone(), B256::repeat_byte(6)),
        );
        store_checkpoint(&guard, key_ready(context.clone(), B256::repeat_byte(6)));
        store_checkpoint(
            &guard,
            UpgradeJournalStateV1::SubmissionPrepared {
                context: context.clone(),
                security: UpgradeSecurityMaterialV1 {
                    sealed_root_hash: B256::repeat_byte(6),
                    resident_offer_public: B256::repeat_byte(7),
                    proof_hash: B256::repeat_byte(8),
                },
                submission: submission(),
            },
        );
        store_checkpoint(
            &guard,
            UpgradeJournalStateV1::Submitted {
                context: context.clone(),
                security: UpgradeSecurityMaterialV1 {
                    sealed_root_hash: B256::repeat_byte(6),
                    resident_offer_public: B256::repeat_byte(7),
                    proof_hash: B256::repeat_byte(8),
                },
                submission: submission(),
                submitted_at_finalized_height: 90,
                transaction_hashes: vec![submission().relay_variants[0].transaction_hash],
            },
        );
        store_checkpoint(
            &guard,
            UpgradeJournalStateV1::Finalized {
                context: context.clone(),
                security: UpgradeSecurityMaterialV1 {
                    sealed_root_hash: B256::repeat_byte(6),
                    resident_offer_public: B256::repeat_byte(7),
                    proof_hash: B256::repeat_byte(8),
                },
                submission: submission(),
                finalized_height: 91,
                finalized_hash: B256::repeat_byte(9),
            },
        );
        store_checkpoint(
            &guard,
            UpgradeJournalStateV1::Promoted {
                context,
                security: UpgradeSecurityMaterialV1 {
                    sealed_root_hash: B256::repeat_byte(6),
                    resident_offer_public: B256::repeat_byte(7),
                    proof_hash: B256::repeat_byte(8),
                },
                submission: submission(),
                finalized_height: 91,
                finalized_hash: B256::repeat_byte(9),
            },
        );
        let snapshot = guard.load().unwrap().unwrap();
        assert_eq!(snapshot.generation, 7);
        assert_eq!(snapshot.lifecycle.label(), "promoted");
        let metadata = fs::metadata(root.path().join(DIRECTORY).join("journal.json")).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, FILE_MODE);
    }

    #[test]
    fn completed_rollout_accepts_only_its_next_successor_and_expired_retry_preserves_root() {
        let root = tempfile::tempdir().unwrap();
        let old = context(root.path());
        let completed = UpgradeJournalStateV1::Promoted {
            context: old.clone(),
            security: UpgradeSecurityMaterialV1 {
                sealed_root_hash: B256::repeat_byte(6),
                resident_offer_public: B256::repeat_byte(7),
                proof_hash: B256::repeat_byte(8),
            },
            submission: submission(),
            finalized_height: 99,
            finalized_hash: B256::repeat_byte(9),
        };
        let mut next = old.clone();
        next.predecessor_manifest_hash = old.candidate_manifest_hash;
        next.candidate_manifest_hash = B256::repeat_byte(10);
        next.successor_policy_hash = B256::repeat_byte(11);
        next.activation_height = 200;
        assert!(is_next_upgrade(
            &completed,
            &UpgradeJournalStateV1::CandidatePrepared {
                context: next.clone()
            }
        ));
        next.activation_height = 100;
        assert!(!is_next_upgrade(
            &completed,
            &UpgradeJournalStateV1::CandidatePrepared { context: next }
        ));
        let pending = key_ready(old.clone(), B256::repeat_byte(6));
        let retry = key_provisioned(old, B256::repeat_byte(6));
        assert!(validate_checkpoint_transition(&pending, &retry).is_ok());
        assert!(validate_checkpoint_transition(&completed, &retry).is_err());
    }

    #[test]
    fn journal_rejects_context_change_and_corrupt_committed_state() {
        let root = tempfile::tempdir().unwrap();
        let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
        let context = context(root.path());
        store_candidate_prepared(&guard, &context);
        let mut different = context;
        different.candidate_manifest_hash = B256::repeat_byte(9);
        assert!(guard
            .store(UpgradeJournalSnapshotV1::new(
                UpgradeJournalStateV1::CandidatePrepared { context: different },
            ))
            .is_err());
        drop(guard);
        fs::write(root.path().join(DIRECTORY).join("journal.json"), b"corrupt").unwrap();
        assert!(inspect_upgrade_journal_v1(root.path()).is_err());
    }

    #[test]
    fn journal_rejects_skipped_or_changed_security_checkpoints() {
        let root = tempfile::tempdir().unwrap();
        let guard = UpgradeJournalGuardV1::acquire(root.path()).unwrap();
        let context = context(root.path());
        store_candidate_prepared(&guard, &context);
        assert!(guard
            .store(UpgradeJournalSnapshotV1::new(key_ready(
                context.clone(),
                B256::repeat_byte(6)
            ),))
            .is_err());
        store_checkpoint(
            &guard,
            key_provisioned(context.clone(), B256::repeat_byte(6)),
        );
        assert!(guard
            .store(UpgradeJournalSnapshotV1::new(key_ready(
                context,
                B256::repeat_byte(9)
            ),))
            .is_err());
    }
}
